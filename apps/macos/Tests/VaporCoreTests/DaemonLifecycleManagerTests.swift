import Foundation
import Testing

@testable import VaporCore

@Test
func bootstrapDefaultsToAutoLaunchEnabledAndStartsDaemon() throws {
  let store = InMemoryAutoLaunchSettingStore()
  let controller = RecordingLaunchAgentController()
  let manager = DaemonLifecycleManager(
    launchAgentController: controller,
    settingsStore: store,
    crashLoopPolicy: .init(
      failureWindow: 60,
      baseDelay: 2,
      maxDelay: 32,
      delayStartsAfterFailures: 2,
      maxConsecutiveFailuresBeforePause: 10
    )
  )

  #expect(manager.autoLaunchEnabled)

  let result = try manager.bootstrapIfNeeded(now: Date(timeIntervalSince1970: 0))
  #expect(result == .started)
  #expect(store.bool(forKey: DaemonLifecycleManager.autoLaunchSettingKey) == true)
  #expect(controller.operations == ["install", "start"])
}

@Test
func disablingAutoLaunchWithoutStopKeepsDaemonRunning() throws {
  let store = InMemoryAutoLaunchSettingStore(
    seed: [DaemonLifecycleManager.autoLaunchSettingKey: true]
  )
  let controller = RecordingLaunchAgentController()
  let manager = DaemonLifecycleManager(launchAgentController: controller, settingsStore: store)

  let result = try manager.setAutoLaunchEnabled(false, stopDaemonNow: false)

  #expect(result == .unchanged)
  #expect(manager.autoLaunchEnabled == false)
  #expect(controller.operations == ["disable"])
}

@Test
func disablingAutoLaunchWithStopAlsoStopsDaemon() throws {
  let store = InMemoryAutoLaunchSettingStore(
    seed: [DaemonLifecycleManager.autoLaunchSettingKey: true]
  )
  let controller = RecordingLaunchAgentController()
  let manager = DaemonLifecycleManager(launchAgentController: controller, settingsStore: store)

  let result = try manager.setAutoLaunchEnabled(false, stopDaemonNow: true)

  #expect(result == .stopped)
  #expect(controller.operations == ["disable", "stop"])
}

@Test
func crashLoopDefersRelaunchWithExponentialBackoff() throws {
  let store = InMemoryAutoLaunchSettingStore(
    seed: [DaemonLifecycleManager.autoLaunchSettingKey: true]
  )
  let controller = RecordingLaunchAgentController()
  let manager = DaemonLifecycleManager(
    launchAgentController: controller,
    settingsStore: store,
    crashLoopPolicy: .init(
      failureWindow: 60,
      baseDelay: 4,
      maxDelay: 32,
      delayStartsAfterFailures: 2,
      maxConsecutiveFailuresBeforePause: 10
    )
  )

  // Canonical schedule (parity with core/lifecycle): the first
  // `delayStartsAfterFailures` crashes are delay-free; backoff starts on
  // the next crash at `baseDelay` and doubles from there.
  let t0 = Date(timeIntervalSince1970: 0)
  #expect(manager.registerUnexpectedDaemonExit(now: t0) == .noDelay)
  #expect(manager.registerUnexpectedDaemonExit(now: t0.addingTimeInterval(1)) == .noDelay)
  #expect(manager.registerUnexpectedDaemonExit(now: t0.addingTimeInterval(2)) == .backoff(4))

  let deferred = try manager.startDaemonIfAllowed(now: t0.addingTimeInterval(3))
  #expect(deferred == .relaunchDeferred(3))
  #expect(controller.operations.isEmpty)

  let started = try manager.startDaemonIfAllowed(now: t0.addingTimeInterval(6))
  #expect(started == .started)
  #expect(controller.operations == ["start"])
}

@Test
func defaultPolicyScheduleMatchesTheRustParityContract() {
  // Mirrors `default_policy_schedule_is_nodelay_then_doubling_backoff_then_pause`
  // in core/lifecycle/tests/crash_loop_parity.rs. If either side's
  // schedule drifts, exactly one of the pair fails.
  var guardrail = CrashLoopGuard(policy: .default)

  let t0 = Date(timeIntervalSince1970: 0)
  #expect(guardrail.registerCrash(at: t0) == .noDelay)
  #expect(guardrail.registerCrash(at: t0.addingTimeInterval(1)) == .backoff(2))
  #expect(guardrail.registerCrash(at: t0.addingTimeInterval(2)) == .backoff(4))
  #expect(guardrail.registerCrash(at: t0.addingTimeInterval(3)) == .backoff(8))
  #expect(guardrail.registerCrash(at: t0.addingTimeInterval(4)) == .paused)
}

@Test
func unexpectedDaemonExitDoesNotTriggerSpontaneousRestartFor30Seconds() throws {
  let store = InMemoryAutoLaunchSettingStore(
    seed: [DaemonLifecycleManager.autoLaunchSettingKey: true]
  )
  let controller = RecordingLaunchAgentController()
  let manager = DaemonLifecycleManager(
    launchAgentController: controller,
    settingsStore: store,
    crashLoopPolicy: .init(
      failureWindow: 600,
      baseDelay: 2,
      maxDelay: 120,
      delayStartsAfterFailures: 1,
      maxConsecutiveFailuresBeforePause: 5
    )
  )

  let killedAt = Date(timeIntervalSince1970: 0)
  _ = manager.registerUnexpectedDaemonExit(now: killedAt)

  // No timer-driven path inside the manager spontaneously restarts the daemon
  // after an unclean exit; combined with launchd KeepAlive=false (audited in
  // LaunchAgentControllerTests), nothing in the system attempts a restart for
  // the next 30 seconds.
  #expect(controller.operations.isEmpty)

  // Once a coordinator-driven trigger fires (user reopens window, login item,
  // explicit menubar action) after the backoff has elapsed, the manager owns
  // the restart — not launchd.
  let restart = try manager.startDaemonIfAllowed(now: killedAt.addingTimeInterval(30))
  #expect(restart == .started)
  #expect(controller.operations == ["start"])
}

@Test
func crashHistoryExpiresOutsideFailureWindow() {
  var guardrail = CrashLoopGuard(
    policy: .init(
      failureWindow: 10,
      baseDelay: 2,
      maxDelay: 30,
      delayStartsAfterFailures: 2,
      maxConsecutiveFailuresBeforePause: 10
    )
  )

  let t0 = Date(timeIntervalSince1970: 0)
  #expect(guardrail.registerCrash(at: t0) == .noDelay)
  #expect(guardrail.registerCrash(at: t0.addingTimeInterval(1)) == .noDelay)
  #expect(guardrail.registerCrash(at: t0.addingTimeInterval(2)) == .backoff(2))
  // 20 seconds later every prior crash fell out of the 10 s window, so
  // the count restarts and the crash is delay-free again.
  #expect(guardrail.registerCrash(at: t0.addingTimeInterval(20)) == .noDelay)
}

@Test
func crashLoopPausesAfterMaxConsecutiveFailuresAndRefusesAutoRestartUntilAcknowledged() throws {
  let store = InMemoryAutoLaunchSettingStore(
    seed: [DaemonLifecycleManager.autoLaunchSettingKey: true]
  )
  let controller = RecordingLaunchAgentController()
  let manager = DaemonLifecycleManager(
    launchAgentController: controller,
    settingsStore: store,
    crashLoopPolicy: .init(
      failureWindow: 600,
      baseDelay: 2,
      maxDelay: 120,
      delayStartsAfterFailures: 1,
      maxConsecutiveFailuresBeforePause: 3
    )
  )

  let t0 = Date(timeIntervalSince1970: 0)
  _ = manager.registerUnexpectedDaemonExit(now: t0)
  _ = manager.registerUnexpectedDaemonExit(now: t0.addingTimeInterval(1))
  let decision = manager.registerUnexpectedDaemonExit(now: t0.addingTimeInterval(2))
  #expect(decision == .paused)
  #expect(manager.isInCrashLoopPause)

  let result = try manager.startDaemonIfAllowed(now: t0.addingTimeInterval(3_600))
  if case .relaunchDeferred(let remaining) = result {
    #expect(remaining == .infinity)
  } else {
    Issue.record("Expected relaunchDeferred(.infinity) while paused, got \(result)")
  }
  #expect(controller.operations.isEmpty)

  manager.acknowledgeCrashLoopPause()
  #expect(!manager.isInCrashLoopPause)

  let resumed = try manager.startDaemonIfAllowed(now: t0.addingTimeInterval(3_700))
  #expect(resumed == .started)
  #expect(controller.operations == ["start"])
}

@Test
func enablingAutoLaunchRegistersOptionalLoginItem() throws {
  let store = InMemoryAutoLaunchSettingStore(seed: [
    DaemonLifecycleManager.autoLaunchSettingKey: false
  ])
  let launchAgent = RecordingLaunchAgentController()
  let loginItem = RecordingLoginItemController()
  let manager = DaemonLifecycleManager(
    launchAgentController: launchAgent,
    settingsStore: store,
    loginItemController: loginItem
  )

  _ = try manager.setAutoLaunchEnabled(true)

  #expect(loginItem.operations == ["register"])
}

@Test
func disablingAutoLaunchUnregistersOptionalLoginItem() throws {
  let store = InMemoryAutoLaunchSettingStore(seed: [
    DaemonLifecycleManager.autoLaunchSettingKey: true
  ])
  let launchAgent = RecordingLaunchAgentController()
  let loginItem = RecordingLoginItemController()
  let manager = DaemonLifecycleManager(
    launchAgentController: launchAgent,
    settingsStore: store,
    loginItemController: loginItem
  )

  _ = try manager.setAutoLaunchEnabled(false)

  #expect(loginItem.operations == ["unregister"])
}

@Test
func loginItemRegistrationFailureDoesNotBlockDaemonLifecycle() throws {
  let store = InMemoryAutoLaunchSettingStore(seed: [
    DaemonLifecycleManager.autoLaunchSettingKey: true
  ])
  let launchAgent = RecordingLaunchAgentController()
  let loginItem = ThrowingLoginItemController()
  let manager = DaemonLifecycleManager(
    launchAgentController: launchAgent,
    settingsStore: store,
    loginItemController: loginItem
  )

  let result = try manager.bootstrapIfNeeded(now: Date(timeIntervalSince1970: 0))

  #expect(result == .started)
  #expect(launchAgent.operations == ["install", "start"])
}

private final class RecordingLaunchAgentController: LaunchAgentControlling {
  var operations: [String] = []

  func installAndEnable() {
    operations.append("install")
  }

  func disableAndUninstall() {
    operations.append("disable")
  }

  func startDaemon() {
    operations.append("start")
  }

  func stopDaemon() {
    operations.append("stop")
  }
}

private final class RecordingLoginItemController: LoginItemControlling {
  var operations: [String] = []

  func register() {
    operations.append("register")
  }

  func unregister() {
    operations.append("unregister")
  }
}

private final class ThrowingLoginItemController: LoginItemControlling {
  struct LoginItemError: Error {}

  func register() throws {
    throw LoginItemError()
  }

  func unregister() throws {
    throw LoginItemError()
  }
}
