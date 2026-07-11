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
    // waitUntilExit() — a permanent deadlock that freezes every serialized
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
    try process.run()

    // Bounded wait; kill a wedged CLI so a hung invocation cannot hang the
    // lifecycle queue (and, transitively, Quit Vapor) forever.
    let timedOut = exited.wait(timeout: .now() + .seconds(Self.timeoutSeconds)) == .timedOut
    if timedOut {
      process.terminate()
      exited.wait()
    }
    // The drains complete once the child exits (or is terminated) and its
    // pipe write ends close.
    drains.wait()

    if timedOut {
      throw VaporCLIServiceError.timedOut(arguments: arguments, seconds: Self.timeoutSeconds)
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
    let arguments = ["service", "check", "--json"]
    let data = try runExpectingSuccess(arguments: arguments)
    let response: HealthResponse = try decode(data: data, arguments: arguments)
    switch response.health {
    case "running":
      return .running
    case "not_installed":
      return .notInstalled
    case "stopped_expected":
      return .stoppedExpected
    case "restarted_after_crash":
      return .restartedAfterCrash
    case "restart_deferred":
      return .restartDeferred(response.remainingSeconds ?? 0)
    case "crash_loop_paused":
      return .crashLoopPaused
    default:
      throw VaporCLIServiceError.malformedResponse(
        arguments: arguments,
        detail: "unknown health value \"\(response.health)\""
      )
    }
  }

  public func acknowledgeCrashLoopPause() throws {
    let arguments = ["service", "acknowledge", "--json"]
    let data = try runExpectingSuccess(arguments: arguments)
    let response: ActionResponse = try decode(data: data, arguments: arguments)
    guard response.result == "acknowledged" else {
      throw VaporCLIServiceError.malformedResponse(
        arguments: arguments,
        detail: "unexpected acknowledge result \"\(response.result)\""
      )
    }
  }

  // MARK: - JSON contract (mirrors core/cli/src/commands/service.rs)

  private struct ActionResponse: Decodable {
    let result: String
    let remainingSeconds: Double?

    enum CodingKeys: String, CodingKey {
      case result
      case remainingSeconds = "remaining_seconds"
    }
  }

  private struct HealthResponse: Decodable {
    let health: String
    let remainingSeconds: Double?

    enum CodingKeys: String, CodingKey {
      case health
      case remainingSeconds = "remaining_seconds"
    }
  }

  private struct StatusResponse: Decodable {
    struct CrashLoop: Decodable {
      let paused: Bool
      let consecutiveCrashes: Int
      let lastCrashAtMilliseconds: UInt64?
      let awaitingRestart: Bool

      enum CodingKeys: String, CodingKey {
        case paused
        case consecutiveCrashes = "consecutive_crashes"
        case lastCrashAtMilliseconds = "last_crash_at_ms"
        case awaitingRestart = "awaiting_restart"
      }
    }

    let status: String
    let label: String
    let autoLaunch: Bool
    let crashLoop: CrashLoop

    enum CodingKeys: String, CodingKey {
      case status
      case label
      case autoLaunch = "auto_launch"
      case crashLoop = "crash_loop"
    }
  }

  private func actionCommand(_ action: String) throws -> DaemonLifecycleActionResult {
    let arguments = ["service", action, "--json"]
    return try decodeAction(data: runExpectingSuccess(arguments: arguments), arguments: arguments)
  }

  private func decodeAction(data: Data, arguments: [String]) throws -> DaemonLifecycleActionResult {
    let response: ActionResponse = try decode(data: data, arguments: arguments)
    switch response.result {
    case "unchanged":
      return .unchanged
    case "started":
      return .started
    case "stopped":
      return .stopped
    case "relaunch_deferred":
      return .relaunchDeferred(response.remainingSeconds ?? 0)
    case "crash_loop_paused":
      return .relaunchDeferred(.infinity)
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

  private func runExpectingSuccess(arguments: [String]) throws -> Data {
    logger.debug(
      "Running vapor CLI command",
      metadata: ["arguments": arguments.joined(separator: " ")]
    )
    let result = try runner.run(arguments: arguments)
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
