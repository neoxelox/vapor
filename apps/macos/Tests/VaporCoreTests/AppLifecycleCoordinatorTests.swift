import Foundation
import Testing

@testable import VaporCore

@MainActor
@Test
func closingMainWindowKeepsDaemonRunning() {
  let launchAgent = OrderedRecordingServiceController()
  let runtime = RecordingAppRuntimeController()
  let manager = DaemonLifecycleManager(launchAgentController: launchAgent)
  let coordinator = AppLifecycleCoordinator(
    daemonLifecycleManager: manager,
    runtimeController: runtime
  )

  coordinator.handleMainWindowClosed()

  #expect(launchAgent.operations.isEmpty)
  #expect(runtime.operations == ["dock:false"])
}

@MainActor
@Test
func openFromMenubarRestoresDockPresence() {
  let launchAgent = OrderedRecordingServiceController()
  let runtime = RecordingAppRuntimeController()
  let manager = DaemonLifecycleManager(launchAgentController: launchAgent)
  let coordinator = AppLifecycleCoordinator(
    daemonLifecycleManager: manager,
    runtimeController: runtime
  )

  coordinator.handleOpenFromMenuBar()

  #expect(launchAgent.operations.isEmpty)
  #expect(runtime.operations == ["dock:true"])
}

@MainActor
@Test
func quittingFromMenubarStopsDaemonBeforeAppTermination() async {
  let events = EventRecorder()
  let launchAgent = OrderedRecordingServiceController(eventRecorder: events)
  let runtime = RecordingAppRuntimeController(eventRecorder: events)
  let manager = DaemonLifecycleManager(launchAgentController: launchAgent)
  let coordinator = AppLifecycleCoordinator(
    daemonLifecycleManager: manager,
    runtimeController: runtime
  )

  await coordinator.handleQuitFromMenuBar(
    serializingOn: DispatchQueue(label: "test.lifecycle")
  )

  #expect(events.events == ["daemon.stop", "app.terminate"])
}

@MainActor
@Test
func cleanShutdownFromMenubarLeavesLaunchAgentPassive() async {
  let launchAgent = OrderedRecordingServiceController()
  let runtime = RecordingAppRuntimeController()
  let manager = DaemonLifecycleManager(launchAgentController: launchAgent)
  let coordinator = AppLifecycleCoordinator(
    daemonLifecycleManager: manager,
    runtimeController: runtime
  )

  await coordinator.handleQuitFromMenuBar(
    serializingOn: DispatchQueue(label: "test.lifecycle")
  )

  // Menubar quit must only stop the daemon and terminate the app process.
  // It must not bootstrap, install, disable, or start the service —
  // those actions would either tear down the LaunchAgent (losing
  // autolaunch on next login) or coax launchd into a restart cycle.
  // Combined with KeepAlive=false (locked by the plist-policy tests in
  // core/platform/src/service/macos.rs), launchd stays passive after a
  // clean shutdown until the next user-driven trigger.
  #expect(launchAgent.operations == ["stop"])
  #expect(runtime.operations == ["terminate"])
}

private final class EventRecorder {
  var events: [String] = []

  func append(_ event: String) {
    events.append(event)
  }
}

private final class OrderedRecordingServiceController: LaunchAgentControlling {
  var operations: [String] = []
  private let eventRecorder: EventRecorder?

  init(eventRecorder: EventRecorder? = nil) {
    self.eventRecorder = eventRecorder
  }

  func bootstrap() throws -> DaemonLifecycleActionResult {
    operations.append("bootstrap")
    eventRecorder?.append("daemon.bootstrap")
    return .started
  }

  func installAndEnable() throws -> DaemonLifecycleActionResult {
    operations.append("install")
    eventRecorder?.append("daemon.install")
    return .started
  }

  func disableAndUninstall(stopDaemonNow: Bool) throws -> DaemonLifecycleActionResult {
    operations.append("disable")
    eventRecorder?.append("daemon.disable")
    return stopDaemonNow ? .stopped : .unchanged
  }

  func startDaemon() throws -> DaemonLifecycleActionResult {
    operations.append("start")
    eventRecorder?.append("daemon.start")
    return .started
  }

  func stopDaemon() throws {
    operations.append("stop")
    eventRecorder?.append("daemon.stop")
  }

  func status() throws -> ServiceStatusSnapshot {
    ServiceStatusSnapshot(
      status: "running",
      label: VaporConstants.Daemon.launchAgentLabel,
      autoLaunchEnabled: true,
      crashLoopPaused: false,
      consecutiveCrashes: 0
    )
  }

  func checkDaemonHealth() throws -> ServiceHealthOutcome {
    operations.append("check")
    return .running
  }

  func acknowledgeCrashLoopPause() throws {
    operations.append("acknowledge")
  }
}

@MainActor
private final class RecordingAppRuntimeController: AppRuntimeControlling {
  var operations: [String] = []
  private let eventRecorder: EventRecorder?

  init(eventRecorder: EventRecorder? = nil) {
    self.eventRecorder = eventRecorder
  }

  func setDockVisible(_ isVisible: Bool) {
    operations.append("dock:\(isVisible)")
  }

  func terminateApplication() {
    operations.append("terminate")
    eventRecorder?.append("app.terminate")
  }
}
