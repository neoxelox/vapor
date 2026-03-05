import Combine
import Foundation
import VaporCore

@MainActor
final class AppShellViewModel: ObservableObject {
  @Published private(set) var state: AppShellState = .initial
  private var daemonLifecycleManager: DaemonLifecycleManager
  private let configurationStore: VaporConfigurationStore
  private var configuration: VaporConfiguration
  private let logger = StructuredLogger(component: "app-shell")
  private var runtimeController: (any AppRuntimeControlling)?
  private var lifecycleCoordinator: AppLifecycleCoordinator?
  private var hasScheduledBootstrap = false

  convenience init() {
    let configurationStore = VaporConfigurationStore()
    let configuration = configurationStore.load()
    let vaporDirectoryURL = configurationStore.resolveVaporDirectoryURL()
    let autoLaunchSettingStore = VaporConfigurationAutoLaunchSettingStore(
      configurationStore: configurationStore)
    self.init(
      daemonLifecycleManager: AppShellViewModel.makeDefaultLifecycleManager(
        vaporDirectoryURL: vaporDirectoryURL,
        autoLaunchSettingStore: autoLaunchSettingStore
      ),
      configurationStore: configurationStore,
      configuration: configuration
    )
  }

  init(
    daemonLifecycleManager: DaemonLifecycleManager,
    configurationStore: VaporConfigurationStore = VaporConfigurationStore(),
    configuration: VaporConfiguration? = nil
  ) {
    self.daemonLifecycleManager = daemonLifecycleManager
    self.configurationStore = configurationStore
    self.configuration = configuration ?? configurationStore.load()

    state.autoLaunchEnabled = daemonLifecycleManager.autoLaunchEnabled
    state.vaporDirectoryPath = self.configurationStore.resolveVaporDirectoryURL().path

    do {
      try self.configurationStore.save(self.configuration)
    } catch {
      logger.error(
        "Failed to persist configuration during startup",
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
      configuration.autoLaunchEnabled = state.autoLaunchEnabled
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
      configuration.autoLaunchEnabled = state.autoLaunchEnabled
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

  private static func makeDefaultLifecycleManager(
    vaporDirectoryURL: URL,
    autoLaunchSettingStore: any AutoLaunchSettingStore
  ) -> DaemonLifecycleManager {
    if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil {
      return .placeholder()
    }

    let daemonExecutableURL = inferredDaemonExecutableURL()
    let launchAgentLabel = "dev.vapor.vapord"
    var daemonEnvironment = [
      "PATH": "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
      "VAPOR_DIR": vaporDirectoryURL.path,
    ]
    if let runtimeEnvironment = ProcessInfo.processInfo.environment[VaporPaths.environmentKey],
      !runtimeEnvironment.isEmpty
    {
      daemonEnvironment[VaporPaths.environmentKey] = runtimeEnvironment
    }
    let configuration = LaunchAgentConfiguration(
      label: launchAgentLabel,
      plistURL: LaunchAgentConfiguration.defaultPlistURL(label: launchAgentLabel),
      daemonExecutableURL: daemonExecutableURL,
      workingDirectoryURL: vaporDirectoryURL,
      environment: daemonEnvironment
    )

    return DaemonLifecycleManager(
      launchAgentController: LaunchAgentController(configuration: configuration),
      settingsStore: autoLaunchSettingStore,
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
