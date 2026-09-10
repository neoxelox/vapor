import Foundation
import Testing

@testable import VaporCore

// The monitor owns only the timer; each tick delegates to the
// Rust core via the manager. Tests drive `performHealthCheck()`
// directly — no timing assertions, per the testing strategy.

@Test
func healthCheckForwardsOutcomeToObserver() {
  let controller = RecordingServiceController()
  controller.healthOutcome = .running
  let manager = DaemonLifecycleManager(launchAgentController: controller)
  let observed = OutcomeRecorder()
  let monitor = DaemonHealthMonitor(manager: manager) { outcome, status in
    observed.append(outcome, status)
  }

  let outcome = monitor.performHealthCheck()

  #expect(outcome == .running)
  #expect(observed.outcomes == [.running])
  #expect(observed.statuses == [controller.liveStatus])
  #expect(controller.operations == ["check", "status"])
}

@Test
func healthCheckSkipsTheStatusReadWhenTheDaemonIsNotRunning() {
  let controller = RecordingServiceController()
  controller.healthOutcome = .stoppedExpected
  let manager = DaemonLifecycleManager(launchAgentController: controller)
  let observed = OutcomeRecorder()
  let monitor = DaemonHealthMonitor(manager: manager) { outcome, status in
    observed.append(outcome, status)
  }

  _ = monitor.performHealthCheck()

  #expect(observed.statuses == [nil])
  #expect(controller.operations == ["check"])
}

@Test
func healthCheckSurfacesCrashLoopPause() {
  let controller = RecordingServiceController()
  controller.healthOutcome = .crashLoopPaused
  let manager = DaemonLifecycleManager(launchAgentController: controller)
  let observed = OutcomeRecorder()
  let monitor = DaemonHealthMonitor(manager: manager) { outcome, status in
    observed.append(outcome, status)
  }

  let outcome = monitor.performHealthCheck()

  #expect(outcome == .crashLoopPaused)
  #expect(observed.outcomes == [.crashLoopPaused])
}

@Test
func failedCheckReturnsNilAndDoesNotNotify() {
  let manager = DaemonLifecycleManager(launchAgentController: FailingServiceController())
  let observed = OutcomeRecorder()
  let monitor = DaemonHealthMonitor(manager: manager) { outcome, status in
    observed.append(outcome, status)
  }

  let outcome = monitor.performHealthCheck()

  #expect(outcome == nil)
  #expect(observed.outcomes.isEmpty)
}

@Test
func startAndStopAreIdempotent() {
  let manager = DaemonLifecycleManager(launchAgentController: RecordingServiceController())
  let monitor = DaemonHealthMonitor(manager: manager, interval: 3_600) { _, _ in }

  monitor.start()
  monitor.start()
  monitor.stop()
  monitor.stop()
}

// MARK: - Fakes

private final class OutcomeRecorder: @unchecked Sendable {
  private(set) var outcomes: [ServiceHealthOutcome] = []
  private(set) var statuses: [DaemonStatusSnapshot?] = []

  func append(_ outcome: ServiceHealthOutcome, _ status: DaemonStatusSnapshot?) {
    outcomes.append(outcome)
    statuses.append(status)
  }
}

private struct FailingServiceController: LaunchAgentControlling {
  struct CheckError: Error {}

  func bootstrap() throws -> DaemonLifecycleActionResult { .unchanged }

  func installAndEnable() throws -> DaemonLifecycleActionResult { .unchanged }

  func disableAndUninstall(stopDaemonNow _: Bool) throws -> DaemonLifecycleActionResult {
    .unchanged
  }

  func startDaemon() throws -> DaemonLifecycleActionResult { .unchanged }

  func stopDaemon() throws {}

  func status() throws -> ServiceStatusSnapshot {
    ServiceStatusSnapshot(
      status: "running",
      label: VaporConstants.Daemon.launchAgentLabel,
      autoLaunchEnabled: true,
      crashLoopPaused: false,
      consecutiveCrashes: 0
    )
  }

  func checkDaemonHealth() throws -> ServiceHealthOutcome { throw CheckError() }

  func acknowledgeCrashLoopPause() throws {}

  func restartDaemon() throws -> DaemonLifecycleActionResult { .unchanged }

  func daemonStatus() throws -> DaemonStatusSnapshot { throw CheckError() }
  func syncNow() throws { throw CheckError() }
}
