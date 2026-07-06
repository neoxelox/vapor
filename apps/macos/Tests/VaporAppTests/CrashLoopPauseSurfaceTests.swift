import Foundation
import Testing

@testable import Vapor
@testable import VaporCore

// The crash-loop pause itself is decided and persisted by the Rust
// lifecycle core (tested in core/lifecycle); these tests cover the
// Swift surface: the view model reflecting and acknowledging the state
// reported over the `vapor service` seam.

@MainActor
@Test
func appShellStateInitialIsNotCrashLoopPaused() {
  #expect(AppShellState.initial.crashLoopPaused == false)
}

@MainActor
@Test
func crashLoopPauseSurfacesOnViewModelAfterRefresh() throws {
  let controller = StubServiceController()
  controller.crashLoopPaused = true
  let manager = DaemonLifecycleManager(launchAgentController: controller)

  let viewModel = AppShellViewModel(
    daemonLifecycleManager: manager,
    configuration: VaporConfiguration()
  )
  #expect(viewModel.state.crashLoopPaused == false)

  viewModel.refreshCrashLoopPauseState()
  #expect(viewModel.state.crashLoopPaused == true)
}

@MainActor
@Test
func acknowledgingCrashLoopPauseClearsViewModelSurface() throws {
  let controller = StubServiceController()
  controller.crashLoopPaused = true
  let manager = DaemonLifecycleManager(launchAgentController: controller)

  let viewModel = AppShellViewModel(
    daemonLifecycleManager: manager,
    configuration: VaporConfiguration()
  )
  viewModel.refreshCrashLoopPauseState()
  #expect(viewModel.state.crashLoopPaused == true)

  viewModel.acknowledgeCrashLoopPause()
  #expect(viewModel.state.crashLoopPaused == false)
  #expect(controller.acknowledged)
}

// MARK: - Fakes

private final class StubServiceController: LaunchAgentControlling {
  var crashLoopPaused = false
  private(set) var acknowledged = false

  func bootstrap() throws -> DaemonLifecycleActionResult { .unchanged }

  func installAndEnable() throws -> DaemonLifecycleActionResult { .started }

  func disableAndUninstall(stopDaemonNow: Bool) throws -> DaemonLifecycleActionResult {
    stopDaemonNow ? .stopped : .unchanged
  }

  func startDaemon() throws -> DaemonLifecycleActionResult {
    crashLoopPaused ? .relaunchDeferred(.infinity) : .started
  }

  func stopDaemon() throws {}

  func status() throws -> ServiceStatusSnapshot {
    ServiceStatusSnapshot(
      status: crashLoopPaused ? "crash_loop_paused" : "running",
      label: VaporConstants.Daemon.launchAgentLabel,
      autoLaunchEnabled: true,
      crashLoopPaused: crashLoopPaused,
      consecutiveCrashes: crashLoopPaused ? 5 : 0
    )
  }

  func checkDaemonHealth() throws -> ServiceHealthOutcome {
    crashLoopPaused ? .crashLoopPaused : .running
  }

  func acknowledgeCrashLoopPause() throws {
    acknowledged = true
    crashLoopPaused = false
  }
}
