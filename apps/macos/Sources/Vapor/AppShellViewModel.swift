import Combine
import Foundation
import VaporCore

@MainActor
final class AppShellViewModel: ObservableObject {
  @Published private(set) var state: AppShellState = .initial
  private var daemonLifecycleManager: DaemonLifecycleManager
  private let userConfigurationStore: VaporUserConfigurationStore
  private var userConfiguration: VaporUserConfiguration
  private let logger = StructuredLogger(component: "app-shell")
  private var runtimeController: (any AppRuntimeControlling)?
  private var lifecycleCoordinator: AppLifecycleCoordinator?
  private var hasScheduledBootstrap = false

  convenience init() {
    let userConfigurationStore = VaporUserConfigurationStore()
    let userConfiguration = userConfigurationStore.load()
    let vaporDirectoryURL = URL(
      fileURLWithPath: userConfiguration.vaporDirectoryPath, isDirectory: true)
    self.init(
      daemonLifecycleManager: AppShellViewModel.makeDefaultLifecycleManager(
        vaporDirectoryURL: vaporDirectoryURL
      ),
      userConfigurationStore: userConfigurationStore,
      userConfiguration: userConfiguration
    )
  }

  init(
    daemonLifecycleManager: DaemonLifecycleManager,
    userConfigurationStore: VaporUserConfigurationStore = VaporUserConfigurationStore(),
    userConfiguration: VaporUserConfiguration? = nil
  ) {
    self.daemonLifecycleManager = daemonLifecycleManager
    self.userConfigurationStore = userConfigurationStore
    self.userConfiguration = userConfiguration ?? userConfigurationStore.load()

    state.autoLaunchEnabled = daemonLifecycleManager.autoLaunchEnabled
    state.vaporDirectoryPath = self.userConfiguration.vaporDirectoryPath

    do {
      try self.userConfigurationStore.save(self.userConfiguration)
    } catch {
      logger.error(
        "Failed to persist user configuration during startup",
        metadata: ["error": String(describing: error)]
      )
    }

    logger.debug(
      "Initialized app shell state",
      metadata: [
        "auto_launch_enabled": String(state.autoLaunchEnabled),
        "vapor_directory": state.vaporDirectoryPath,
      ]
    )
  }

  func configureAppRuntimeControllerIfNeeded(_ runtimeController: any AppRuntimeControlling) {
    guard self.runtimeController == nil else {
      return
    }

    self.runtimeController = runtimeController
    refreshLifecycleCoordinator()
    logger.debug("Configured app runtime lifecycle controller")
  }

  func bootstrapDaemonLifecycleIfNeeded() {
    guard !hasScheduledBootstrap else {
      return
    }

    hasScheduledBootstrap = true
    logger.info("Scheduling non-blocking daemon lifecycle bootstrap")

    DispatchQueue.main.async { [weak self] in
      self?.runDaemonLifecycleBootstrap()
    }
  }

  private func runDaemonLifecycleBootstrap() {
    do {
      let result = try daemonLifecycleManager.bootstrapIfNeeded()
      logger.info(
        "Daemon lifecycle bootstrap completed",
        metadata: ["result": String(describing: result)]
      )
    } catch {
      state.syncState = .error
      logger.error(
        "Daemon lifecycle bootstrap failed",
        metadata: ["error": String(describing: error)]
      )
    }
  }

  func handleMainWindowClosed() {
    lifecycleCoordinator?.handleMainWindowClosed()
  }

  func handleOpenFromMenuBar() {
    lifecycleCoordinator?.handleOpenFromMenuBar()
  }

  func handleQuitFromMenuBar() {
    lifecycleCoordinator?.handleQuitFromMenuBar()
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

  func updateVaporDirectoryPath(_ rawPath: String) {
    guard
      let vaporDirectoryURL = VaporPaths.normalizedDirectoryURL(pathString: rawPath),
      !vaporDirectoryURL.path.isEmpty
    else {
      state.syncState = .error
      logger.error("Invalid vapor directory path", metadata: ["raw_path": rawPath])
      return
    }

    do {
      var updatedConfiguration = userConfiguration
      updatedConfiguration.vaporDirectoryPath = vaporDirectoryURL.path
      try userConfigurationStore.save(updatedConfiguration)
      userConfiguration = updatedConfiguration
      state.vaporDirectoryPath = updatedConfiguration.vaporDirectoryPath

      daemonLifecycleManager = Self.makeDefaultLifecycleManager(
        vaporDirectoryURL: vaporDirectoryURL)
      state.autoLaunchEnabled = daemonLifecycleManager.autoLaunchEnabled
      refreshLifecycleCoordinator()

      if state.autoLaunchEnabled {
        _ = try daemonLifecycleManager.setAutoLaunchEnabled(true)
      }

      logger.info(
        "Updated vapor runtime directory",
        metadata: ["vapor_directory": vaporDirectoryURL.path]
      )
    } catch {
      state.syncState = .error
      logger.error(
        "Failed to update vapor runtime directory",
        metadata: [
          "raw_path": rawPath,
          "error": String(describing: error),
        ]
      )
    }
  }

  private func refreshLifecycleCoordinator() {
    guard let runtimeController else {
      lifecycleCoordinator = nil
      return
    }

    lifecycleCoordinator = AppLifecycleCoordinator(
      daemonLifecycleManager: daemonLifecycleManager,
      runtimeController: runtimeController
    )
  }

  private static func makeDefaultLifecycleManager(vaporDirectoryURL: URL) -> DaemonLifecycleManager
  {
    if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil {
      return .placeholder()
    }

    let daemonExecutableURL = inferredDaemonExecutableURL()
    let launchAgentLabel = "dev.vapor.vapord"
    let configuration = LaunchAgentConfiguration(
      label: launchAgentLabel,
      plistURL: LaunchAgentConfiguration.defaultPlistURL(label: launchAgentLabel),
      daemonExecutableURL: daemonExecutableURL,
      workingDirectoryURL: vaporDirectoryURL,
      environment: [
        "PATH": "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        "VAPOR_DIR": vaporDirectoryURL.path,
      ]
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
