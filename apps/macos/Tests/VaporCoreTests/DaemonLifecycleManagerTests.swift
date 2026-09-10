import Foundation
import Testing

@testable import VaporCore

// `DaemonLifecycleManager` is a thin facade: lifecycle
// *policy* (autolaunch persistence, crash-loop backoff/pause,
// supervision) lives in the Rust `core/lifecycle` crate behind the
// `vapor` CLI and is tested there. These tests cover what Swift still
// owns: delegation order, outcome mapping, and login-item coupling.

@Test
func bootstrapDelegatesAndRegistersLoginItemWhenDaemonStarts() throws {
  let controller = RecordingServiceController()
  let loginItem = RecordingLoginItemController()
  let manager = DaemonLifecycleManager(
    launchAgentController: controller,
    loginItemController: loginItem
  )

  let result = try manager.bootstrapIfNeeded()

  #expect(result == .started)
  #expect(controller.operations == ["bootstrap"])
  #expect(loginItem.operations == ["register"])
}

@Test
func bootstrapNoopSkipsLoginItemWhenAutoLaunchDisabled() throws {
  let controller = RecordingServiceController()
  controller.bootstrapResult = .unchanged
  let loginItem = RecordingLoginItemController()
  let manager = DaemonLifecycleManager(
    launchAgentController: controller,
    loginItemController: loginItem
  )

  let result = try manager.bootstrapIfNeeded()

  #expect(result == .unchanged)
  #expect(loginItem.operations.isEmpty)
}

@Test
func bootstrapRegistersLoginItemEvenWhenStartIsDeferredByCrashLoopPolicy() throws {
  // A deferred start still means autolaunch is enabled and the service
  // definition was installed — the login item must be registered.
  let controller = RecordingServiceController()
  controller.bootstrapResult = .relaunchDeferred(2)
  let loginItem = RecordingLoginItemController()
  let manager = DaemonLifecycleManager(
    launchAgentController: controller,
    loginItemController: loginItem
  )

  let result = try manager.bootstrapIfNeeded()

  #expect(result == .relaunchDeferred(2))
  #expect(loginItem.operations == ["register"])
}

@Test
func enablingAutoLaunchDelegatesInstallAndRegistersLoginItem() throws {
  let controller = RecordingServiceController()
  let loginItem = RecordingLoginItemController()
  let manager = DaemonLifecycleManager(
    launchAgentController: controller,
    loginItemController: loginItem
  )

  let result = try manager.setAutoLaunchEnabled(true)

  #expect(result == .started)
  #expect(controller.operations == ["install"])
  #expect(loginItem.operations == ["register"])
}

@Test
func disablingAutoLaunchWithoutStopRequestsKeepRunningUninstall() throws {
  let controller = RecordingServiceController()
  let loginItem = RecordingLoginItemController()
  let manager = DaemonLifecycleManager(
    launchAgentController: controller,
    loginItemController: loginItem
  )

  let result = try manager.setAutoLaunchEnabled(false, stopDaemonNow: false)

  #expect(result == .unchanged)
  #expect(controller.operations == ["uninstall(keep-running)"])
  #expect(loginItem.operations == ["unregister"])
}

@Test
func disablingAutoLaunchWithStopRequestsStoppingUninstall() throws {
  let controller = RecordingServiceController()
  controller.uninstallResult = .stopped
  let manager = DaemonLifecycleManager(launchAgentController: controller)

  let result = try manager.setAutoLaunchEnabled(false, stopDaemonNow: true)

  #expect(result == .stopped)
  #expect(controller.operations == ["uninstall(stop-now)"])
}

@Test
func startDaemonIfAllowedSurfacesDeferredOutcomeWithoutRetrying() throws {
  let controller = RecordingServiceController()
  controller.startResult = .relaunchDeferred(4)
  let manager = DaemonLifecycleManager(launchAgentController: controller)

  let result = try manager.startDaemonIfAllowed()

  #expect(result == .relaunchDeferred(4))
  #expect(controller.operations == ["start"])
}

@Test
func stopDaemonForTerminationDelegatesToStop() throws {
  let controller = RecordingServiceController()
  let manager = DaemonLifecycleManager(launchAgentController: controller)

  try manager.stopDaemonForTermination()

  #expect(controller.operations == ["stop"])
}

@Test
func autoLaunchEnabledReflectsCLIStatusReport() {
  let controller = RecordingServiceController()
  controller.statusSnapshot.autoLaunchEnabled = false
  let manager = DaemonLifecycleManager(launchAgentController: controller)

  #expect(manager.autoLaunchEnabled == false)
}

@Test
func autoLaunchEnabledFallsBackToDefaultWhenCLIIsUnreachable() {
  let manager = DaemonLifecycleManager(launchAgentController: ThrowingServiceController())

  #expect(manager.autoLaunchEnabled == VaporConstants.Defaults.autoLaunch)
}

@Test
func crashLoopPauseStateReflectsCLIStatusReport() throws {
  let controller = RecordingServiceController()
  controller.statusSnapshot.crashLoopPaused = true
  let manager = DaemonLifecycleManager(launchAgentController: controller)

  #expect(try manager.crashLoopPauseState())
}

@Test
func acknowledgeCrashLoopPauseDelegatesToController() throws {
  let controller = RecordingServiceController()
  let manager = DaemonLifecycleManager(launchAgentController: controller)

  try manager.acknowledgeCrashLoopPause()

  #expect(controller.operations == ["acknowledge"])
}

@Test
func acknowledgeCrashLoopPausePropagatesCLIFailure() {
  let manager = DaemonLifecycleManager(launchAgentController: ThrowingServiceController())

  #expect(throws: (any Error).self) {
    try manager.acknowledgeCrashLoopPause()
  }
}

@Test
func crashLoopPauseStatePropagatesCLIFailureInsteadOfReportingHealthy() {
  let manager = DaemonLifecycleManager(launchAgentController: ThrowingServiceController())

  #expect(throws: (any Error).self) {
    _ = try manager.crashLoopPauseState()
  }
}

@Test
func checkDaemonHealthDelegatesToController() throws {
  let controller = RecordingServiceController()
  controller.healthOutcome = .restartDeferred(2)
  let manager = DaemonLifecycleManager(launchAgentController: controller)

  let outcome = try manager.checkDaemonHealth()

  #expect(outcome == .restartDeferred(2))
  #expect(controller.operations == ["check"])
}

@Test
func loginItemRegistrationFailureDoesNotBlockDaemonLifecycle() throws {
  let controller = RecordingServiceController()
  let manager = DaemonLifecycleManager(
    launchAgentController: controller,
    loginItemController: ThrowingLoginItemController()
  )

  let result = try manager.bootstrapIfNeeded()

  #expect(result == .started)
  #expect(controller.operations == ["bootstrap"])
}

@Test
func restartDelegatesToTheControllerOnTheLifecycleQueue() throws {
  let controller = RecordingServiceController()
  let manager = DaemonLifecycleManager(launchAgentController: controller)
  let result = try manager.restartDaemon()
  #expect(result == .started)
  #expect(controller.operations == ["restart"])
}

// MARK: - Fakes

final class RecordingServiceController: LaunchAgentControlling {
  var operations: [String] = []
  var bootstrapResult: DaemonLifecycleActionResult = .started
  var installResult: DaemonLifecycleActionResult = .started
  var uninstallResult: DaemonLifecycleActionResult = .unchanged
  var startResult: DaemonLifecycleActionResult = .started
  var healthOutcome: ServiceHealthOutcome = .running
  var liveStatus = DaemonStatusSnapshot(
    runState: "Running",
    throttleState: "IdleDrain",
    throttleReason: "idle and cool",
    providerName: "filesystem",
    queueDepth: 0,
    failedIntents: 0
  )
  var statusSnapshot = ServiceStatusSnapshot(
    status: "running",
    label: VaporConstants.Daemon.launchAgentLabel,
    autoLaunchEnabled: true,
    crashLoopPaused: false,
    consecutiveCrashes: 0
  )

  func bootstrap() throws -> DaemonLifecycleActionResult {
    operations.append("bootstrap")
    return bootstrapResult
  }

  func installAndEnable() throws -> DaemonLifecycleActionResult {
    operations.append("install")
    return installResult
  }

  func disableAndUninstall(stopDaemonNow: Bool) throws -> DaemonLifecycleActionResult {
    operations.append(stopDaemonNow ? "uninstall(stop-now)" : "uninstall(keep-running)")
    return uninstallResult
  }

  func startDaemon() throws -> DaemonLifecycleActionResult {
    operations.append("start")
    return startResult
  }

  func stopDaemon() throws {
    operations.append("stop")
  }

  func status() throws -> ServiceStatusSnapshot {
    statusSnapshot
  }

  func checkDaemonHealth() throws -> ServiceHealthOutcome {
    operations.append("check")
    return healthOutcome
  }

  func acknowledgeCrashLoopPause() throws {
    operations.append("acknowledge")
    statusSnapshot.crashLoopPaused = false
  }

  func restartDaemon() throws -> DaemonLifecycleActionResult {
    operations.append("restart")
    return startResult
  }

  func daemonStatus() throws -> DaemonStatusSnapshot {
    operations.append("status")
    return liveStatus
  }
  func syncNow() throws {}
}

private struct ThrowingServiceController: LaunchAgentControlling {
  struct ControllerError: Error {}

  func bootstrap() throws -> DaemonLifecycleActionResult { throw ControllerError() }

  func installAndEnable() throws -> DaemonLifecycleActionResult { throw ControllerError() }

  func disableAndUninstall(stopDaemonNow _: Bool) throws -> DaemonLifecycleActionResult {
    throw ControllerError()
  }

  func startDaemon() throws -> DaemonLifecycleActionResult { throw ControllerError() }

  func stopDaemon() throws { throw ControllerError() }

  func status() throws -> ServiceStatusSnapshot { throw ControllerError() }

  func checkDaemonHealth() throws -> ServiceHealthOutcome { throw ControllerError() }

  func acknowledgeCrashLoopPause() throws { throw ControllerError() }

  func restartDaemon() throws -> DaemonLifecycleActionResult { throw ControllerError() }

  func daemonStatus() throws -> DaemonStatusSnapshot { throw ControllerError() }
  func syncNow() throws { throw ControllerError() }
}

@Test
func blockedLoginItemRegistrationSurfacesRequiresApproval() throws {
  let controller = RecordingServiceController()
  let loginItem = RecordingLoginItemController()
  loginItem.registrationOutcome = .requiresApproval
  let manager = DaemonLifecycleManager(
    launchAgentController: controller,
    loginItemController: loginItem
  )

  try manager.setAutoLaunchEnabled(true)

  #expect(manager.lastLoginItemRegistrationOutcome() == .requiresApproval)
}

@Test
func throwingLoginItemRegistrationSurfacesFailureInsteadOfSilentSuccess() throws {
  let controller = RecordingServiceController()
  let manager = DaemonLifecycleManager(
    launchAgentController: controller,
    loginItemController: ThrowingLoginItemController()
  )

  try manager.setAutoLaunchEnabled(true)

  guard case .failed = manager.lastLoginItemRegistrationOutcome() else {
    Issue.record("a throwing registration must surface as .failed")
    return
  }
}

@Test
func disablingAutoLaunchClearsTheLoginItemOutcome() throws {
  let controller = RecordingServiceController()
  let loginItem = RecordingLoginItemController()
  loginItem.registrationOutcome = .requiresApproval
  let manager = DaemonLifecycleManager(
    launchAgentController: controller,
    loginItemController: loginItem
  )

  try manager.setAutoLaunchEnabled(true)
  try manager.setAutoLaunchEnabled(false)

  #expect(manager.lastLoginItemRegistrationOutcome() == .unavailable)
}

private final class RecordingLoginItemController: LoginItemControlling {
  var operations: [String] = []
  var registrationOutcome: LoginItemRegistrationOutcome = .registered

  @discardableResult
  func register() -> LoginItemRegistrationOutcome {
    operations.append("register")
    return registrationOutcome
  }

  func unregister() {
    operations.append("unregister")
  }

  func openLoginItemSettings() {
    operations.append("openSettings")
  }
}

private final class ThrowingLoginItemController: LoginItemControlling {
  struct LoginItemError: Error {}

  @discardableResult
  func register() throws -> LoginItemRegistrationOutcome {
    throw LoginItemError()
  }

  func unregister() throws {
    throw LoginItemError()
  }

  func openLoginItemSettings() {}
}
