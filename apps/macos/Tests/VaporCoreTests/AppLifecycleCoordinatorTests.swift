import Testing

@testable import VaporCore

@MainActor
@Test
func closingMainWindowKeepsDaemonRunning() {
  let launchAgent = OrderedRecordingLaunchAgentController()
  let runtime = RecordingAppRuntimeController()
  let manager = DaemonLifecycleManager(
    launchAgentController: launchAgent,
    settingsStore: InMemoryAutoLaunchSettingStore()
  )
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
  let launchAgent = OrderedRecordingLaunchAgentController()
  let runtime = RecordingAppRuntimeController()
  let manager = DaemonLifecycleManager(
    launchAgentController: launchAgent,
    settingsStore: InMemoryAutoLaunchSettingStore()
  )
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
func quittingFromMenubarStopsDaemonBeforeAppTermination() {
  let events = EventRecorder()
  let launchAgent = OrderedRecordingLaunchAgentController(eventRecorder: events)
  let runtime = RecordingAppRuntimeController(eventRecorder: events)
  let manager = DaemonLifecycleManager(
    launchAgentController: launchAgent,
    settingsStore: InMemoryAutoLaunchSettingStore()
  )
  let coordinator = AppLifecycleCoordinator(
    daemonLifecycleManager: manager,
    runtimeController: runtime
  )

  coordinator.handleQuitFromMenuBar()

  #expect(events.events == ["daemon.stop", "app.terminate"])
}

@MainActor
@Test
func cleanShutdownFromMenubarLeavesLaunchAgentPassive() {
  let launchAgent = OrderedRecordingLaunchAgentController()
  let runtime = RecordingAppRuntimeController()
  let manager = DaemonLifecycleManager(
    launchAgentController: launchAgent,
    settingsStore: InMemoryAutoLaunchSettingStore(
      seed: [DaemonLifecycleManager.autoLaunchSettingKey: true]
    )
  )
  let coordinator = AppLifecycleCoordinator(
    daemonLifecycleManager: manager,
    runtimeController: runtime
  )

  coordinator.handleQuitFromMenuBar()

  // Menubar quit must only stop the daemon and terminate the app process.
  // It must not bootstrap, install, disable, or start the LaunchAgent —
  // those actions would either tear down the plist (losing autolaunch on
  // next login) or coax launchd into a restart cycle. Combined with
  // KeepAlive=false (audited in LaunchAgentControllerTests), launchd stays
  // passive after a clean shutdown until the next user-driven trigger.
  #expect(launchAgent.operations == ["stop"])
  #expect(runtime.operations == ["terminate"])
}

private final class EventRecorder {
  var events: [String] = []

  func append(_ event: String) {
    events.append(event)
  }
}

private final class OrderedRecordingLaunchAgentController: LaunchAgentControlling {
  var operations: [String] = []
  private let eventRecorder: EventRecorder?

  init(eventRecorder: EventRecorder? = nil) {
    self.eventRecorder = eventRecorder
  }

  func installAndEnable() {
    operations.append("install")
    eventRecorder?.append("daemon.install")
  }

  func disableAndUninstall() {
    operations.append("disable")
    eventRecorder?.append("daemon.disable")
  }

  func startDaemon() {
    operations.append("start")
    eventRecorder?.append("daemon.start")
  }

  func stopDaemon() {
    operations.append("stop")
    eventRecorder?.append("daemon.stop")
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
