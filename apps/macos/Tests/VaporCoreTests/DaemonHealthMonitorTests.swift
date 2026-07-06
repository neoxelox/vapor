import Foundation
import Testing

@testable import VaporCore

// M2-5: the monitor owns only the timer; each tick delegates to the
// Rust core via the manager. Tests drive `performHealthCheck()`
// directly — no timing assertions, per the testing strategy.

@Test
func healthCheckForwardsOutcomeToObserver() {
  let controller = RecordingServiceController()
  controller.healthOutcome = .running
  let manager = DaemonLifecycleManager(launchAgentController: controller)
  let observed = OutcomeRecorder()
  let monitor = DaemonHealthMonitor(manager: manager) { outcome in
    observed.append(outcome)
  }

  let outcome = monitor.performHealthCheck()

  #expect(outcome == .running)
  #expect(observed.outcomes == [.running])
  #expect(controller.operations == ["check"])
}

@Test
func healthCheckSurfacesCrashLoopPause() {
  let controller = RecordingServiceController()
  controller.healthOutcome = .crashLoopPaused
  let manager = DaemonLifecycleManager(launchAgentController: controller)
  let observed = OutcomeRecorder()
  let monitor = DaemonHealthMonitor(manager: manager) { outcome in
    observed.append(outcome)
  }

  let outcome = monitor.performHealthCheck()

  #expect(outcome == .crashLoopPaused)
  #expect(observed.outcomes == [.crashLoopPaused])
}

@Test
func failedCheckReturnsNilAndDoesNotNotify() {
  let manager = DaemonLifecycleManager(launchAgentController: FailingServiceController())
  let observed = OutcomeRecorder()
  let monitor = DaemonHealthMonitor(manager: manager) { outcome in
    observed.append(outcome)
  }

  let outcome = monitor.performHealthCheck()

  #expect(outcome == nil)
  #expect(observed.outcomes.isEmpty)
}

@Test
func startAndStopAreIdempotent() {
  let manager = DaemonLifecycleManager(launchAgentController: RecordingServiceController())
  let monitor = DaemonHealthMonitor(manager: manager, interval: 3_600) { _ in }

  monitor.start()
  monitor.start()
  monitor.stop()
  monitor.stop()
}

// MARK: - Fakes

private final class OutcomeRecorder: @unchecked Sendable {
  private(set) var outcomes: [ServiceHealthOutcome] = []

  func append(_ outcome: ServiceHealthOutcome) {
    outcomes.append(outcome)
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
}
