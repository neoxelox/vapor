import Combine
import Foundation
import VaporCore

@MainActor
final class AppShellViewModel: ObservableObject {
  @Published private(set) var state: AppShellState = .initial
  private let daemonLifecycleManager: DaemonLifecycleManager

  convenience init() {
    self.init(daemonLifecycleManager: AppShellViewModel.makeDefaultLifecycleManager())
  }

  init(daemonLifecycleManager: DaemonLifecycleManager) {
    self.daemonLifecycleManager = daemonLifecycleManager
    state.autoLaunchEnabled = daemonLifecycleManager.autoLaunchEnabled
  }

  func toggleAutoLaunch() {
    let nextState = !state.autoLaunchEnabled

    do {
      _ = try daemonLifecycleManager.setAutoLaunchEnabled(nextState)
      state.autoLaunchEnabled = daemonLifecycleManager.autoLaunchEnabled
    } catch {
      state.syncState = .error
    }
  }

  func disableAutoLaunchAndStopNow() {
    do {
      _ = try daemonLifecycleManager.setAutoLaunchEnabled(false, stopDaemonNow: true)
      state.autoLaunchEnabled = daemonLifecycleManager.autoLaunchEnabled
    } catch {
      state.syncState = .error
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
