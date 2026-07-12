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
}

public protocol LoginItemControlling {
  func register() throws
  func unregister() throws
}

public struct NoopLoginItemController: LoginItemControlling {
  public init() {}

  public func register() throws {}

  public func unregister() throws {}
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
}

/// Thin coordinator over the `LaunchAgentControlling` seam. Owns
/// serialization (one lifecycle operation at a time) and macOS login-item
/// registration; every lifecycle decision is delegated to the Rust
/// `core/lifecycle` layer through the controller — no runtime or
/// lifecycle policy lives in Swift.
public final class DaemonLifecycleManager: @unchecked Sendable {
  private let launchAgentController: any LaunchAgentControlling
  private let loginItemController: (any LoginItemControlling)?
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

  private func registerLoginItemIfAvailable() {
    do {
      try loginItemController?.register()
    } catch {
      logger.warning(
        "Failed to register app login item",
        metadata: ["error": String(describing: error)]
      )
    }
  }

  private func unregisterLoginItemIfAvailable() {
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
