import Foundation

/// Captured result of one `vapor` CLI invocation.
public struct VaporCLIResult: Equatable, Sendable {
  public var exitCode: Int32
  public var standardOutput: String
  public var standardError: String

  public init(exitCode: Int32, standardOutput: String, standardError: String) {
    self.exitCode = exitCode
    self.standardOutput = standardOutput
    self.standardError = standardError
  }
}

/// Seam for spawning the bundled `vapor` CLI. Tests substitute a fake
/// that returns canned JSON; production uses `ProcessVaporCLIRunner`.
public protocol VaporCLIRunning {
  func run(arguments: [String]) throws -> VaporCLIResult
  /// The same, for the one command that waits on a person (the browser
  /// sign-in) and needs a longer bound than a lifecycle round trip.
  func run(arguments: [String], timeoutSeconds: Int) throws -> VaporCLIResult
}

extension VaporCLIRunning {
  public func run(arguments: [String], timeoutSeconds _: Int) throws -> VaporCLIResult {
    try run(arguments: arguments)
  }
}

public enum VaporCLIServiceError: Error, Equatable {
  case cliExecutableMissing(path: String)
  case commandFailed(arguments: [String], exitCode: Int32, standardError: String)
  case malformedResponse(arguments: [String], detail: String)
  case timedOut(arguments: [String], seconds: Int)
}

/// Spawns the bundled `vapor` binary as a subprocess. The app's
/// environment (including any `VAPOR_DIR` / `VAPOR_ENV` overrides) is
/// inherited, so the CLI resolves the same runtime directory the app
/// uses.
public struct ProcessVaporCLIRunner: VaporCLIRunning {
  private let cliExecutableURL: URL
  private let fileManager: FileManager

  public init(cliExecutableURL: URL, fileManager: FileManager = .default) {
    self.cliExecutableURL = cliExecutableURL
    self.fileManager = fileManager
  }

  /// Upper bound on a single CLI invocation. Lifecycle commands round-trip
  /// through `launchctl` and can take a second or two; this only fires when
  /// the child is genuinely wedged (a stalled launchctl, lock contention),
  /// so it must never permanently freeze the lifecycle queue.
  static let timeoutSeconds = 30

  public func run(arguments: [String]) throws -> VaporCLIResult {
    try run(arguments: arguments, timeoutSeconds: Self.timeoutSeconds)
  }

  public func run(arguments: [String], timeoutSeconds: Int) throws -> VaporCLIResult {
    guard fileManager.isExecutableFile(atPath: cliExecutableURL.path) else {
      throw VaporCLIServiceError.cliExecutableMissing(path: cliExecutableURL.path)
    }

    let process = Process()
    process.executableURL = cliExecutableURL
    process.arguments = arguments

    let outputPipe = Pipe()
    let errorPipe = Pipe()
    process.standardOutput = outputPipe
    process.standardError = errorPipe

    // Drain both pipes concurrently, BEFORE waiting: a child that writes
    // more than the ~64KB pipe buffer (a Rust panic backtrace, verbose
    // stderr) would otherwise block in write() while we block in
    // waitUntilExit(): a permanent deadlock that freezes every serialized
    // lifecycle operation.
    let outputBox = DataBox()
    let errorBox = DataBox()
    let drains = DispatchGroup()
    let drainQueue = DispatchQueue(label: "sh.arn.vapor.cli-drain", attributes: .concurrent)
    let outputHandle = outputPipe.fileHandleForReading
    let errorHandle = errorPipe.fileHandleForReading
    drainQueue.async(group: drains) { outputBox.data = outputHandle.readDataToEndOfFile() }
    drainQueue.async(group: drains) { errorBox.data = errorHandle.readDataToEndOfFile() }

    let exited = DispatchSemaphore(value: 0)
    process.terminationHandler = { _ in exited.signal() }
    do {
      try process.run()
    } catch {
      // The child never started, so nothing will close the write ends;
      // close them here so the drains finish now rather than whenever
      // the pipes happen to deinit.
      try? outputPipe.fileHandleForWriting.close()
      try? errorPipe.fileHandleForWriting.close()
      drains.wait()
      throw error
    }

    // Bounded wait; kill a wedged CLI so a hung invocation cannot hang the
    // lifecycle queue (and, transitively, Quit Vapor) forever.
    let timedOut = exited.wait(timeout: .now() + .seconds(timeoutSeconds)) == .timedOut
    if timedOut {
      process.terminate()
      exited.wait()
    }
    // The drains complete once the child exits (or is terminated) and its
    // pipe write ends close.
    drains.wait()

    if timedOut {
      throw VaporCLIServiceError.timedOut(arguments: arguments, seconds: timeoutSeconds)
    }

    return VaporCLIResult(
      exitCode: process.terminationStatus,
      standardOutput: String(data: outputBox.data, encoding: .utf8) ?? "",
      standardError: String(data: errorBox.data, encoding: .utf8) ?? ""
    )
  }
}

/// Mutable box so the concurrent pipe-drain closures can hand their result
/// back. Each box is written by exactly one closure and read only after
/// `DispatchGroup.wait()`, so no additional synchronization is needed.
private final class DataBox: @unchecked Sendable {
  var data = Data()
}

/// Default `LaunchAgentControlling` implementation: every
/// operation shells out to `vapor service … --json` and decodes the
/// stable JSON contract rendered by `core/cli/src/commands/service.rs`.
/// The Rust lifecycle core owns the LaunchAgent plist, launchctl
/// interaction, autolaunch persistence, and crash-loop policy; this
/// class only translates outcomes into Swift values.
public final class VaporCLIServiceController: LaunchAgentControlling {
  private let runner: any VaporCLIRunning
  private let logger: StructuredLogger

  public init(
    runner: any VaporCLIRunning,
    logger: StructuredLogger = StructuredLogger(component: "vapor-cli-service")
  ) {
    self.runner = runner
    self.logger = logger
  }

  public convenience init(
    cliExecutableURL: URL,
    logger: StructuredLogger = StructuredLogger(component: "vapor-cli-service")
  ) {
    self.init(runner: ProcessVaporCLIRunner(cliExecutableURL: cliExecutableURL), logger: logger)
  }

  public func bootstrap() throws -> DaemonLifecycleActionResult {
    try actionCommand("bootstrap")
  }

  public func installAndEnable() throws -> DaemonLifecycleActionResult {
    try actionCommand("install")
  }

  public func disableAndUninstall(stopDaemonNow: Bool) throws -> DaemonLifecycleActionResult {
    var arguments = ["service", "uninstall", "--json"]
    if !stopDaemonNow {
      arguments.append("--keep-running")
    }
    return try decodeAction(data: runExpectingSuccess(arguments: arguments), arguments: arguments)
  }

  public func startDaemon() throws -> DaemonLifecycleActionResult {
    try actionCommand("start")
  }

  public func stopDaemon() throws {
    _ = try actionCommand("stop")
  }

  public func status() throws -> ServiceStatusSnapshot {
    let arguments = ["service", "status", "--json"]
    let data = try runExpectingSuccess(arguments: arguments)
    let response: StatusResponse = try decode(data: data, arguments: arguments)
    return ServiceStatusSnapshot(
      status: response.status,
      label: response.label,
      autoLaunchEnabled: response.autoLaunch,
      crashLoopPaused: response.crashLoop.paused,
      consecutiveCrashes: response.crashLoop.consecutiveCrashes
    )
  }

  public func checkDaemonHealth() throws -> ServiceHealthOutcome {
    // `status` reads; `check` restarts, and that is the supervisor
    // job's to run. Two callers of `check` could count one exit twice.
    let arguments = ["service", "status", "--json"]
    let data = try runExpectingSuccess(arguments: arguments)
    let response: HealthResponse = try decode(data: data, arguments: arguments)
    switch response.health {
    case VaporConstants.ServiceHealth.running:
      return .running
    case VaporConstants.ServiceHealth.notInstalled:
      return .notInstalled
    case VaporConstants.ServiceHealth.stoppedExpected:
      return .stoppedExpected
    case VaporConstants.ServiceHealth.restartDeferred:
      return .restartDeferred(response.remainingSeconds ?? 0)
    case VaporConstants.ServiceHealth.crashLoopPaused:
      return .crashLoopPaused
    default:
      throw VaporCLIServiceError.malformedResponse(
        arguments: arguments,
        detail: "unknown health value \"\(response.health)\""
      )
    }
  }

  public func restartDaemon() throws -> DaemonLifecycleActionResult {
    try actionCommand("restart")
  }

  public func daemonStatus() throws -> DaemonStatusSnapshot {
    let arguments = ["status", "--json"]
    let data = try runExpectingSuccess(arguments: arguments)
    let response: DaemonStatusResponse = try decode(data: data, arguments: arguments)
    let runReason = response.run.reason ?? ""
    return DaemonStatusSnapshot(
      runState: response.run.state,
      throttleState: response.throttle.state,
      throttleReason: response.throttle.reason ?? "",
      provider: response.provider,
      queued: response.queued ?? 0,
      failed: response.failed ?? 0,
      restartRequired: response.restartRequired,
      decisions: response.decisions ?? 0,
      runReason: runReason.isEmpty ? nil : runReason,
      reconcileState: response.reconcile?.state ?? "",
      reconcileDetail: response.reconcile?.detail ?? "",
      signInRequired: response.signInRequired
    )
  }

  /// Runs the browser sign-in, and ends it after
  /// `browserSignInTimeoutSeconds` and a margin when the consent has
  /// not come: the CLI itself would wait much longer.
  public func signIn(profile: String) throws {
    let arguments = ["auth", "login", "--profile", profile, "--browser"]
    _ = try runExpectingSuccess(
      arguments: arguments,
      timeoutSeconds: VaporConstants.Provider.browserSignInTimeoutSeconds + 30
    )
  }

  public func reconcile() throws {
    let arguments = ["reconcile", "--json"]
    // A refusal exits non-zero with the daemon's note on stdout; read it
    // so the error says why instead of quoting an exit code.
    let result = try runner.run(arguments: arguments)
    let data = Data(result.standardOutput.utf8)
    guard result.exitCode == 0 || (try? JSONDecoder().decode(AckResponse.self, from: data)) != nil
    else {
      throw VaporCLIServiceError.commandFailed(
        arguments: arguments,
        exitCode: result.exitCode,
        standardError: result.standardError
      )
    }
    let response: AckResponse = try decode(data: data, arguments: arguments)
    guard response.accepted else {
      throw VaporCLIServiceError.malformedResponse(
        arguments: arguments,
        detail: "daemon refused the reconcile: \(response.note ?? "")"
      )
    }
  }

  public func acknowledgeCrashLoopPause() throws {
    let arguments = ["service", "acknowledge", "--json"]
    let data = try runExpectingSuccess(arguments: arguments)
    let response: ActionResponse = try decode(data: data, arguments: arguments)
    guard response.result == VaporConstants.ServiceResult.acknowledged else {
      throw VaporCLIServiceError.malformedResponse(
        arguments: arguments,
        detail: "unexpected acknowledge result \"\(response.result)\""
      )
    }
  }

  // MARK: - JSON contract (mirrors core/cli/src/commands/service.rs and
  // core/ipc/src/protocol.rs; keys are camelCase, so the property names
  // are the keys)

  private struct ActionResponse: Decodable {
    let result: String
    let remainingSeconds: Double?
  }

  /// Mirrors `core/ipc/src/protocol.rs::AckResponse`.
  private struct AckResponse: Decodable {
    let accepted: Bool
    let note: String?
  }

  private struct HealthResponse: Decodable {
    let health: String
    let remainingSeconds: Double?
  }

  private struct StatusResponse: Decodable {
    struct CrashLoop: Decodable {
      let paused: Bool
      let consecutiveCrashes: Int
      let lastCrashAtMs: UInt64?
      let awaitingRestart: Bool
    }

    let status: String
    let label: String
    let autoLaunch: Bool
    let crashLoop: CrashLoop
  }

  /// Mirrors `core/ipc/src/protocol.rs::StatusResponse` (the fields the
  /// app renders; unknown fields are ignored).
  private struct DaemonStatusResponse: Decodable {
    struct StateReason: Decodable {
      let state: String
      let reason: String?
    }

    struct Reconcile: Decodable {
      let state: String?
      let detail: String?
    }

    struct ProfileResponse: Decodable {
      let id: String?
      let provider: String?
      let signInRequired: Bool?
    }

    let run: StateReason
    let throttle: StateReason
    let provider: String
    let queued: UInt64?
    let failed: UInt64?
    let decisions: UInt64?
    let restartRequired: String?
    let reconcile: Reconcile?
    let profiles: [ProfileResponse]?

    /// The profiles whose provider refused the sign-in.
    var signInRequired: [SignInRequest] {
      (profiles ?? []).compactMap { profile in
        guard profile.signInRequired == true, let id = profile.id,
          let provider = profile.provider
        else {
          return nil
        }
        return SignInRequest(providerKind: provider, profileId: id)
      }
    }
  }

  private func actionCommand(_ action: String) throws -> DaemonLifecycleActionResult {
    let arguments = ["service", action, "--json"]
    return try decodeAction(data: runExpectingSuccess(arguments: arguments), arguments: arguments)
  }

  private func decodeAction(data: Data, arguments: [String]) throws -> DaemonLifecycleActionResult {
    let response: ActionResponse = try decode(data: data, arguments: arguments)
    switch response.result {
    case VaporConstants.ServiceResult.unchanged:
      return .unchanged
    case VaporConstants.ServiceResult.started:
      return .started
    case VaporConstants.ServiceResult.stopped:
      return .stopped
    case VaporConstants.ServiceResult.relaunchDeferred:
      return .relaunchDeferred(response.remainingSeconds ?? 0)
    case VaporConstants.ServiceResult.crashLoopPaused:
      return .relaunchDeferred(.infinity)
    case VaporConstants.ServiceResult.notInstalled:
      return .notInstalled
    default:
      throw VaporCLIServiceError.malformedResponse(
        arguments: arguments,
        detail: "unknown result value \"\(response.result)\""
      )
    }
  }

  private func decode<Response: Decodable>(data: Data, arguments: [String]) throws -> Response {
    do {
      return try JSONDecoder().decode(Response.self, from: data)
    } catch {
      throw VaporCLIServiceError.malformedResponse(
        arguments: arguments,
        detail: String(describing: error)
      )
    }
  }

  private func runExpectingSuccess(arguments: [String], timeoutSeconds: Int? = nil) throws -> Data {
    logger.debug(
      "Running vapor CLI command",
      metadata: ["arguments": arguments.joined(separator: " ")]
    )
    let result =
      if let timeoutSeconds {
        try runner.run(arguments: arguments, timeoutSeconds: timeoutSeconds)
      } else {
        try runner.run(arguments: arguments)
      }
    guard result.exitCode == 0 else {
      logger.error(
        "vapor CLI command failed",
        metadata: [
          "arguments": arguments.joined(separator: " "),
          "exit_code": String(result.exitCode),
          "stderr": result.standardError,
        ]
      )
      throw VaporCLIServiceError.commandFailed(
        arguments: arguments,
        exitCode: result.exitCode,
        standardError: result.standardError
      )
    }
    return Data(result.standardOutput.utf8)
  }
}
