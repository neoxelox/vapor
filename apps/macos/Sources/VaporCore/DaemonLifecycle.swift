import Foundation

/// Result of a lifecycle action, decoded from the `vapor service …`
/// JSON contract. `relaunchDeferred(.infinity)` represents the
/// crash-loop paused state (mirrors `Duration::MAX` on the Rust side).
public enum DaemonLifecycleActionResult: Equatable, Sendable {
  case unchanged
  case started
  case stopped
  case relaunchDeferred(TimeInterval)
}

/// Outcome of one `vapor service check` supervision tick.
public enum ServiceHealthOutcome: Equatable, Sendable {
  case running
  case notInstalled
  case stoppedExpected
  case restartedAfterCrash
  case restartDeferred(TimeInterval)
  case crashLoopPaused
}

/// Decoded `vapor service status --json` report.
public struct ServiceStatusSnapshot: Equatable, Sendable {
  public var status: String
  public var label: String
  public var autoLaunchEnabled: Bool
  public var crashLoopPaused: Bool
  public var consecutiveCrashes: Int

  public init(
    status: String,
    label: String,
    autoLaunchEnabled: Bool,
    crashLoopPaused: Bool,
    consecutiveCrashes: Int
  ) {
    self.status = status
    self.label = label
    self.autoLaunchEnabled = autoLaunchEnabled
    self.crashLoopPaused = crashLoopPaused
    self.consecutiveCrashes = consecutiveCrashes
  }
}

/// Decoded `vapor status --json`: the subset the app renders.
public struct DaemonStatusSnapshot: Equatable, Sendable {
  public var runState: String
  public var throttleState: String
  public var throttleReason: String
  public var providerName: String
  public var queueDepth: UInt64
  public var failedIntents: UInt64
  /// Set when a restart-required key of `vapor.json` changed under the
  /// running daemon; the daemon phrases the notice.
  public var configRestartRequired: String?

  public init(
    runState: String,
    throttleState: String,
    throttleReason: String,
    providerName: String,
    queueDepth: UInt64,
    failedIntents: UInt64,
    configRestartRequired: String? = nil
  ) {
    self.runState = runState
    self.throttleState = throttleState
    self.throttleReason = throttleReason
    self.providerName = providerName
    self.queueDepth = queueDepth
    self.failedIntents = failedIntents
    self.configRestartRequired = configRestartRequired
  }
}

/// The app's seam onto daemon lifecycle operations. The
/// default implementation is `VaporCLIServiceController`, which invokes
/// the bundled `vapor` CLI as a subprocess — all lifecycle *policy*
/// (autolaunch persistence, crash-loop backoff and pause, supervision)
/// lives in the Rust `core/lifecycle` crate behind that CLI. Swift only
/// ever sees the outcomes.
public protocol LaunchAgentControlling {
  /// App-startup path (`vapor service bootstrap`): install + start only
  /// when autolaunch is enabled; `.unchanged` when it is disabled.
  @discardableResult
  func bootstrap() throws -> DaemonLifecycleActionResult
  /// Enable autolaunch, install the service definition, start the
  /// daemon (`vapor service install`).
  @discardableResult
  func installAndEnable() throws -> DaemonLifecycleActionResult
  /// Disable autolaunch and remove the service definition
  /// (`vapor service uninstall [--keep-running]`). `stopDaemonNow`
  /// only controls the explicit stop signal; on macOS the daemon exits
  /// either way because launchd tears the job down when its service
  /// definition is booted out.
  @discardableResult
  func disableAndUninstall(stopDaemonNow: Bool) throws -> DaemonLifecycleActionResult
  /// Start the daemon, subject to the Rust-side crash-loop policy
  /// (`vapor service start`).
  @discardableResult
  func startDaemon() throws -> DaemonLifecycleActionResult
  /// Stop the daemon (`vapor service stop`).
  func stopDaemon() throws
  /// Current service + crash-loop state (`vapor service status`).
  func status() throws -> ServiceStatusSnapshot
  /// One supervision tick (`vapor service check`).
  @discardableResult
  func checkDaemonHealth() throws -> ServiceHealthOutcome
  /// Clear a crash-loop pause (`vapor service acknowledge`).
  func acknowledgeCrashLoopPause() throws
  /// Stop and start the daemon (`vapor service restart`), which is how
  /// a restart-required configuration change takes effect.
  @discardableResult
  func restartDaemon() throws -> DaemonLifecycleActionResult
  /// Live daemon status over IPC (`vapor status --json`). Throws when
  /// the daemon is not running.
  func daemonStatus() throws -> DaemonStatusSnapshot
}

/// Outcome of a login-item registration attempt, surfaced so the UI
/// can distinguish "will launch at login" from "macOS is blocking it".
/// Silently reporting success while the app will not actually launch at
/// login violates the degrade-safely rule for permissioned features.
public enum LoginItemRegistrationOutcome: Equatable, Sendable {
  case registered
  /// macOS deferred or refused the registration — typically the user
  /// disabled the login item in System Settings, or device management
  /// policy blocks it. The user must approve it under System Settings
  /// › General › Login Items.
  case requiresApproval
  case failed(String)
  /// No login-item controller is wired (tests, hosts without
  /// ServiceManagement).
  case unavailable
}

public protocol LoginItemControlling {
  @discardableResult
  func register() throws -> LoginItemRegistrationOutcome
  func unregister() throws
  /// Opens System Settings at the Login Items pane so the user can
  /// approve a blocked registration. No-op where unsupported.
  func openLoginItemSettings()
}

public struct NoopLoginItemController: LoginItemControlling {
  public init() {}

  @discardableResult
  public func register() throws -> LoginItemRegistrationOutcome { .unavailable }

  public func unregister() throws {}

  public func openLoginItemSettings() {}
}

public struct NoopLaunchAgentController: LaunchAgentControlling {
  public init() {}

  public func bootstrap() throws -> DaemonLifecycleActionResult { .unchanged }

  public func installAndEnable() throws -> DaemonLifecycleActionResult { .unchanged }

  public func disableAndUninstall(stopDaemonNow _: Bool) throws -> DaemonLifecycleActionResult {
    .unchanged
  }

  public func startDaemon() throws -> DaemonLifecycleActionResult { .unchanged }

  public func stopDaemon() throws {}

  public func status() throws -> ServiceStatusSnapshot {
    ServiceStatusSnapshot(
      status: "not_installed",
      label: VaporConstants.Daemon.launchAgentLabel,
      autoLaunchEnabled: VaporConstants.Defaults.autoLaunch,
      crashLoopPaused: false,
      consecutiveCrashes: 0
    )
  }

  public func checkDaemonHealth() throws -> ServiceHealthOutcome { .notInstalled }

  public func acknowledgeCrashLoopPause() throws {}

  public func restartDaemon() throws -> DaemonLifecycleActionResult { .unchanged }

  public func daemonStatus() throws -> DaemonStatusSnapshot {
    struct NotRunning: Error {}
    throw NotRunning()
  }
}

/// Thin coordinator over the `LaunchAgentControlling` seam. Owns
/// serialization (one lifecycle operation at a time) and macOS login-item
/// registration; every lifecycle decision is delegated to the Rust
/// `core/lifecycle` layer through the controller — no runtime or
/// lifecycle policy lives in Swift.
public final class DaemonLifecycleManager: @unchecked Sendable {
  private let launchAgentController: any LaunchAgentControlling
  private let loginItemController: (any LoginItemControlling)?
  /// Guarded by `stateQueue`: every mutation happens inside the
  /// serialized lifecycle operations, and the public accessor reads
  /// through the same queue.
  private var loginItemOutcome: LoginItemRegistrationOutcome = .unavailable
  // Process-shared so that "one lifecycle operation at a time" holds even
  // when the app swaps in a fresh manager on a config save: the health
  // monitor keeps a reference to the original manager, so a per-instance
  // queue would let a periodic `vapor service check` run concurrently with
  // a user-initiated install/uninstall/stop on a different queue — the
  // check could then observe the daemon vanishing mid-uninstall and
  // restart it (or register a spurious crash toward crash-loop pause).
  private var stateQueue: DispatchQueue { Self.sharedStateQueue }
  private static let sharedStateQueue = DispatchQueue(
    label: "sh.arn.vapor.daemon-lifecycle.state"
  )
  private let logger: StructuredLogger

  public init(
    launchAgentController: any LaunchAgentControlling,
    loginItemController: (any LoginItemControlling)? = nil,
    logger: StructuredLogger = StructuredLogger(component: "daemon-lifecycle")
  ) {
    self.launchAgentController = launchAgentController
    self.loginItemController = loginItemController
    self.logger = logger
  }

  public static func placeholder() -> DaemonLifecycleManager {
    DaemonLifecycleManager(
      launchAgentController: NoopLaunchAgentController(),
      loginItemController: nil
    )
  }

  /// Effective autolaunch preference, as reported by the Rust layer
  /// (which defaults it to `true` and persists the default on first
  /// read). Falls back to the shipped default when the CLI is
  /// unreachable so UI state stays renderable.
  public var autoLaunchEnabled: Bool {
    stateQueue.sync {
      do {
        return try launchAgentController.status().autoLaunchEnabled
      } catch {
        logger.error(
          "Failed to read autolaunch state from the vapor CLI; assuming default",
          metadata: ["error": String(describing: error)]
        )
        return VaporConstants.Defaults.autoLaunch
      }
    }
  }

  @discardableResult
  public func bootstrapIfNeeded() throws -> DaemonLifecycleActionResult {
    try stateQueue.sync {
      let result = try launchAgentController.bootstrap()
      if result == .unchanged {
        logger.debug("Lifecycle bootstrap was a no-op (auto-launch disabled)")
        return result
      }

      registerLoginItemIfAvailable()
      logger.info(
        "Lifecycle bootstrap completed",
        metadata: ["result": String(describing: result)]
      )
      return result
    }
  }

  @discardableResult
  public func setAutoLaunchEnabled(
    _ enabled: Bool,
    stopDaemonNow: Bool = false
  ) throws -> DaemonLifecycleActionResult {
    try stateQueue.sync {
      logger.info(
        "Updating auto-launch setting",
        metadata: ["enabled": String(enabled), "stop_now": String(stopDaemonNow)]
      )

      if enabled {
        let result = try launchAgentController.installAndEnable()
        registerLoginItemIfAvailable()
        return result
      }

      let result = try launchAgentController.disableAndUninstall(stopDaemonNow: stopDaemonNow)
      unregisterLoginItemIfAvailable()
      logger.warning("Disabled auto-launch; crash-loop state was reset by the lifecycle core")
      return result
    }
  }

  /// Crash-loop pause state, or the CLI error. Unlike the fail-open
  /// `autoLaunchEnabled` default, this must NOT default to "not paused":
  /// a transient read failure that silently reported healthy would let
  /// the UI clear a real pause banner. Callers keep their previous known
  /// state on error (the 30s health tick restores the truth regardless).
  public func crashLoopPauseState() throws -> Bool {
    try stateQueue.sync {
      try launchAgentController.status().crashLoopPaused
    }
  }

  public func acknowledgeCrashLoopPause() throws {
    try stateQueue.sync {
      try launchAgentController.acknowledgeCrashLoopPause()
      logger.warning("Acknowledged crash-loop pause; auto-restart may proceed again")
    }
  }

  @discardableResult
  public func startDaemonIfAllowed() throws -> DaemonLifecycleActionResult {
    try stateQueue.sync {
      let result = try launchAgentController.startDaemon()
      switch result {
      case .relaunchDeferred(let remaining):
        logger.warning(
          "Daemon start deferred by crash-loop policy",
          metadata: ["remaining_seconds": String(remaining)]
        )
      default:
        logger.info("Requested daemon start", metadata: ["result": String(describing: result)])
      }
      return result
    }
  }

  public func stopDaemonForTermination() throws {
    try stateQueue.sync {
      try launchAgentController.stopDaemon()
      logger.warning("Requested daemon stop for app termination")
    }
  }

  /// One supervision tick, delegated to `vapor service check`
  /// (detection, crash registration, and restart policy all run in the
  /// Rust lifecycle core). Called periodically by `DaemonHealthMonitor`.
  @discardableResult
  public func checkDaemonHealth() throws -> ServiceHealthOutcome {
    try stateQueue.sync {
      try launchAgentController.checkDaemonHealth()
    }
  }

  /// Live daemon status over IPC. Read-only, so it does not need the
  /// lifecycle queue; a daemon that is not running throws.
  public func daemonStatus() throws -> DaemonStatusSnapshot {
    try launchAgentController.daemonStatus()
  }

  /// Stop and start the daemon; the way a restart-required
  /// configuration change is applied.
  @discardableResult
  public func restartDaemon() throws -> DaemonLifecycleActionResult {
    try stateQueue.sync {
      let result = try launchAgentController.restartDaemon()
      logger.info("Requested daemon restart", metadata: ["result": String(describing: result)])
      return result
    }
  }

  /// Last login-item registration outcome. Read after
  /// `setAutoLaunchEnabled` / `bootstrapIfNeeded` so the UI can surface
  /// a blocked registration instead of showing the toggle ON while the
  /// app will not actually launch at login.
  public func lastLoginItemRegistrationOutcome() -> LoginItemRegistrationOutcome {
    stateQueue.sync { loginItemOutcome }
  }

  /// Opens System Settings at the Login Items pane.
  public func openLoginItemSettings() {
    loginItemController?.openLoginItemSettings()
  }

  private func registerLoginItemIfAvailable() {
    guard let loginItemController else {
      loginItemOutcome = .unavailable
      return
    }
    do {
      let outcome = try loginItemController.register()
      loginItemOutcome = outcome
      if outcome == .requiresApproval {
        logger.warning(
          "Login item registration requires user approval in System Settings"
        )
      }
    } catch {
      loginItemOutcome = .failed(String(describing: error))
      logger.warning(
        "Failed to register app login item",
        metadata: ["error": String(describing: error)]
      )
    }
  }

  private func unregisterLoginItemIfAvailable() {
    loginItemOutcome = .unavailable
    do {
      try loginItemController?.unregister()
    } catch {
      logger.warning(
        "Failed to unregister app login item",
        metadata: ["error": String(describing: error)]
      )
    }
  }
}
