import Foundation
import Testing

@testable import VaporCore

// `DaemonLifecycleManager` is a thin facade since M2-1/C4-7: lifecycle
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
func crashLoopPauseStateReflectsCLIStatusReport() {
  let controller = RecordingServiceController()
  controller.statusSnapshot.crashLoopPaused = true
  let manager = DaemonLifecycleManager(launchAgentController: controller)

  #expect(manager.isInCrashLoopPause)
}

@Test
func acknowledgeCrashLoopPauseDelegatesToController() {
  let controller = RecordingServiceController()
  let manager = DaemonLifecycleManager(launchAgentController: controller)

  manager.acknowledgeCrashLoopPause()

  #expect(controller.operations == ["acknowledge"])
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

// MARK: - Fakes

final class RecordingServiceController: LaunchAgentControlling {
  var operations: [String] = []
  var bootstrapResult: DaemonLifecycleActionResult = .started
  var installResult: DaemonLifecycleActionResult = .started
  var uninstallResult: DaemonLifecycleActionResult = .unchanged
  var startResult: DaemonLifecycleActionResult = .started
  var healthOutcome: ServiceHealthOutcome = .running
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
