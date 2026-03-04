import Combine
import Foundation
import VaporCore

@MainActor
final class AppShellViewModel: ObservableObject {
  @Published private(set) var state: AppShellState = .initial
  private let daemonLifecycleManager: DaemonLifecycleManager
  private let logger = StructuredLogger(component: "app-shell")

  convenience init() {
    self.init(daemonLifecycleManager: AppShellViewModel.makeDefaultLifecycleManager())
  }

  init(daemonLifecycleManager: DaemonLifecycleManager) {
    self.daemonLifecycleManager = daemonLifecycleManager
    state.autoLaunchEnabled = daemonLifecycleManager.autoLaunchEnabled
    logger.debug(
      "Initialized app shell state",
      metadata: ["auto_launch_enabled": String(state.autoLaunchEnabled)]
    )
  }

  func toggleAutoLaunch() {
    let nextState = !state.autoLaunchEnabled
    logger.info("Toggling auto-launch", metadata: ["next_value": String(nextState)])

    do {
      let result = try daemonLifecycleManager.setAutoLaunchEnabled(nextState)
      state.autoLaunchEnabled = daemonLifecycleManager.autoLaunchEnabled
      logger.info(
        "Auto-launch toggle completed",
        metadata: [
          "persisted_value": String(state.autoLaunchEnabled),
          "result": String(describing: result),
        ]
      )
    } catch {
      state.syncState = .error
      logger.error(
        "Auto-launch toggle failed",
        metadata: ["error": String(describing: error)]
      )
    }
  }

  func disableAutoLaunchAndStopNow() {
    logger.info("Disabling auto-launch and requesting immediate daemon stop")

    do {
      let result = try daemonLifecycleManager.setAutoLaunchEnabled(false, stopDaemonNow: true)
      state.autoLaunchEnabled = daemonLifecycleManager.autoLaunchEnabled
      logger.warning(
        "Auto-launch disabled with stop-now",
        metadata: ["result": String(describing: result)]
      )
    } catch {
      state.syncState = .error
      logger.error(
        "Disable auto-launch and stop-now failed",
        metadata: ["error": String(describing: error)]
      )
    }
  }

  func cycleSyncState() {
    let allStates = SyncSurfaceState.allCases
    guard let currentIndex = allStates.firstIndex(of: state.syncState) else {
      state.syncState = .idle
      return
    }

    let nextIndex = allStates.index(after: currentIndex)
    let wrappedIndex = nextIndex == allStates.endIndex ? allStates.startIndex : nextIndex
    state.syncState = allStates[wrappedIndex]
    logger.debug("Cycled sync state", metadata: ["new_state": String(describing: state.syncState)])
  }

  private static func makeDefaultLifecycleManager() -> DaemonLifecycleManager {
    if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil {
      return .placeholder()
    }

    let daemonExecutableURL = inferredDaemonExecutableURL()
    let launchAgentLabel = "dev.vapor.vapord"
    let configuration = LaunchAgentConfiguration(
      label: launchAgentLabel,
      plistURL: LaunchAgentConfiguration.defaultPlistURL(label: launchAgentLabel),
      daemonExecutableURL: daemonExecutableURL,
      environment: ["PATH": "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"]
    )

    return DaemonLifecycleManager(
      launchAgentController: LaunchAgentController(configuration: configuration),
      settingsStore: UserDefaultsAutoLaunchSettingStore(),
      loginItemController: makeOptionalLoginItemController()
    )
  }

  private static func inferredDaemonExecutableURL() -> URL {
    let bundledURL = Bundle.main.bundleURL
      .appendingPathComponent("Contents")
      .appendingPathComponent("MacOS")
      .appendingPathComponent("vapord")

    if FileManager.default.fileExists(atPath: bundledURL.path) {
      return bundledURL
    }

    return URL(fileURLWithPath: "/usr/local/bin/vapord")
  }

  private static func makeOptionalLoginItemController() -> (any LoginItemControlling)? {
    #if canImport(ServiceManagement)
      if #available(macOS 13.0, *) {
        guard let identifier = ProcessInfo.processInfo.environment["VAPOR_LOGIN_ITEM_IDENTIFIER"],
          !identifier.isEmpty
        else {
          return nil
        }

        return SMAppServiceLoginItemController(loginItemIdentifier: identifier)
      }
    #endif

    return nil
  }
}
