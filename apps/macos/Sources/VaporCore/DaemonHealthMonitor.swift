import Foundation

/// Periodic daemon supervision tick.
///
/// The app owns only the *timer*; each tick delegates to
/// `DaemonLifecycleManager.checkDaemonHealth()`, which runs
/// `vapor service check` — detection of unexpected daemon exits, crash
/// registration, backoff, and restart policy all execute in the Rust
/// lifecycle core against durable state. When the daemon is running the
/// tick also reads `vapor status --json`, so the UI can show what the
/// daemon is doing. The `onOutcome` callback carries both.
///
/// Tests drive `performHealthCheck()` directly; the timer itself is OS
/// plumbing and is exercised manually per the UI-testing carve-out.
public final class DaemonHealthMonitor: @unchecked Sendable {
  public static let defaultInterval: TimeInterval =
    VaporConstants.Daemon.healthTickIntervalSeconds

  private let manager: DaemonLifecycleManager
  private let interval: TimeInterval
  private let queue: DispatchQueue
  private let onOutcome: @Sendable (ServiceHealthOutcome, DaemonStatusSnapshot?) -> Void
  private let logger: StructuredLogger
  private var timer: DispatchSourceTimer?

  public init(
    manager: DaemonLifecycleManager,
    interval: TimeInterval = DaemonHealthMonitor.defaultInterval,
    queue: DispatchQueue = DispatchQueue(label: "sh.arn.vapor.daemon-health", qos: .utility),
    logger: StructuredLogger = StructuredLogger(component: "daemon-health"),
    onOutcome: @escaping @Sendable (ServiceHealthOutcome, DaemonStatusSnapshot?) -> Void
  ) {
    self.manager = manager
    self.interval = interval
    self.queue = queue
    self.logger = logger
    self.onOutcome = onOutcome
  }

  public func start() {
    queue.sync {
      guard timer == nil else {
        return
      }

      let source = DispatchSource.makeTimerSource(queue: queue)
      source.schedule(deadline: .now() + interval, repeating: interval)
      source.setEventHandler { [weak self] in
        self?.performHealthCheck()
      }
      source.resume()
      timer = source
      logger.info(
        "Started daemon health monitoring",
        metadata: ["interval_seconds": String(interval)]
      )
    }
  }

  public func stop() {
    queue.sync {
      timer?.cancel()
      timer = nil
    }
  }

  /// One supervision tick. Returns the outcome, or `nil` when the check
  /// itself failed (e.g. the CLI could not be spawned) — a failed tick
  /// is logged and retried on the next interval, never fatal.
  @discardableResult
  public func performHealthCheck() -> ServiceHealthOutcome? {
    do {
      let outcome = try manager.checkDaemonHealth()
      switch outcome {
      case .running, .stoppedExpected, .notInstalled:
        break
      case .restartedAfterCrash:
        logger.warning("Health tick detected an unexpected daemon exit; daemon restarted")
      case .restartDeferred(let remaining):
        logger.warning(
          "Health tick detected an unexpected daemon exit; restart deferred",
          metadata: ["remaining_seconds": String(remaining)]
        )
      case .crashLoopPaused:
        logger.error(
          "Daemon is in crash-loop pause; auto-restart suspended until user acknowledges"
        )
      }
      onOutcome(outcome, liveStatus(for: outcome))
      return outcome
    } catch {
      logger.error(
        "Daemon health check failed",
        metadata: ["error": String(describing: error)]
      )
      return nil
    }
  }

  /// The daemon's own status when the supervision tick found it running.
  /// A failed read is logged and reported as `nil`; the next tick retries.
  private func liveStatus(for outcome: ServiceHealthOutcome) -> DaemonStatusSnapshot? {
    guard outcome == .running || outcome == .restartedAfterCrash else {
      return nil
    }
    do {
      return try manager.daemonStatus()
    } catch {
      logger.warning(
        "Daemon reported running but its status endpoint did not answer",
        metadata: ["error": String(describing: error)]
      )
      return nil
    }
  }
}
