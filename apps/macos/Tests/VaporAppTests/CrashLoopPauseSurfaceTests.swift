import Foundation
import Testing

@testable import Vapor
@testable import VaporCore

@MainActor
@Test
func appShellStateInitialIsNotCrashLoopPaused() {
  #expect(AppShellState.initial.crashLoopPaused == false)
}

@MainActor
@Test
func crashLoopPauseSurfacesOnViewModelAfterRefresh() throws {
  let store = InMemoryAutoLaunchSettingStore(
    seed: [DaemonLifecycleManager.autoLaunchSettingKey: true]
  )
  let manager = DaemonLifecycleManager(
    launchAgentController: NoopLaunchAgentController(),
    settingsStore: store,
    crashLoopPolicy: .init(
      failureWindow: 600,
      baseDelay: 2,
      maxDelay: 120,
      delayStartsAfterFailures: 1,
      maxConsecutiveFailuresBeforePause: 1
    )
  )

  let viewModel = AppShellViewModel(
    daemonLifecycleManager: manager,
    configuration: VaporConfiguration()
  )
  #expect(viewModel.state.crashLoopPaused == false)

  let decision = manager.registerUnexpectedDaemonExit(now: Date(timeIntervalSince1970: 0))
  #expect(decision == .paused)

  viewModel.refreshCrashLoopPauseState()
  #expect(viewModel.state.crashLoopPaused == true)
}

@MainActor
@Test
func acknowledgingCrashLoopPauseClearsViewModelSurface() throws {
  let store = InMemoryAutoLaunchSettingStore(
    seed: [DaemonLifecycleManager.autoLaunchSettingKey: true]
  )
  let manager = DaemonLifecycleManager(
    launchAgentController: NoopLaunchAgentController(),
    settingsStore: store,
    crashLoopPolicy: .init(
      failureWindow: 600,
      baseDelay: 2,
      maxDelay: 120,
      delayStartsAfterFailures: 1,
      maxConsecutiveFailuresBeforePause: 1
    )
  )
  _ = manager.registerUnexpectedDaemonExit(now: Date(timeIntervalSince1970: 0))
  #expect(manager.isInCrashLoopPause)

  let viewModel = AppShellViewModel(
    daemonLifecycleManager: manager,
    configuration: VaporConfiguration()
  )
  #expect(viewModel.state.crashLoopPaused == true)

  viewModel.acknowledgeCrashLoopPause()
  #expect(viewModel.state.crashLoopPaused == false)
  #expect(manager.isInCrashLoopPause == false)
}
