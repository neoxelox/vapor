import Combine
import Foundation
import VaporCore

@MainActor
final class AppShellViewModel: ObservableObject {
  @Published private(set) var state: AppShellState = .initial
  private var daemonLifecycleManager: DaemonLifecycleManager
  private let configurationStore: VaporConfigurationStore
  private let localizationStore: VaporLocalizationStore
  private var configuration: VaporConfiguration
  private var localization: VaporLocalizedCatalog
  private let logger = StructuredLogger(component: "app-shell")
  private let lifecycleQueue = DispatchQueue(label: "sh.arn.vapor.lifecycle", qos: .utility)
  private var runtimeController: (any AppRuntimeControlling)?
  private var lifecycleCoordinator: AppLifecycleCoordinator?
  private var hasScheduledBootstrap = false

  convenience init() {
    let configurationStore = VaporConfigurationStore()
    let configuration = configurationStore.load()
    let vaporDirectoryURL = configurationStore.resolveVaporDirectoryURL()
    let localizationStore = VaporLocalizationStore()
    let autoLaunchSettingStore = VaporConfigurationAutoLaunchSettingStore(
      configurationStore: configurationStore)
    self.init(
      daemonLifecycleManager: AppShellViewModel.makeDefaultLifecycleManager(
        vaporDirectoryURL: vaporDirectoryURL,
        autoLaunchSettingStore: autoLaunchSettingStore,
        useGitIgnore: configuration.useGitIgnore,
        useVaporIgnore: configuration.useVaporIgnore,
        localSyncDirectory: configuration.localSyncDirectory,
        cloudSyncDirectory: configuration.cloudSyncDirectory,
        preIgnoreRules: configuration.preIgnoreRules,
        postIgnoreRules: configuration.postIgnoreRules
      ),
      configurationStore: configurationStore,
      localizationStore: localizationStore,
      configuration: configuration
    )
  }

  init(
    daemonLifecycleManager: DaemonLifecycleManager,
    configurationStore: VaporConfigurationStore = VaporConfigurationStore(),
    localizationStore: VaporLocalizationStore = VaporLocalizationStore(),
    configuration: VaporConfiguration? = nil
  ) {
    let resolvedConfiguration = configuration ?? configurationStore.load()

    self.daemonLifecycleManager = daemonLifecycleManager
    self.configurationStore = configurationStore
    self.localizationStore = localizationStore
    self.configuration = resolvedConfiguration
    self.localization = localizationStore.resolve(
      preferredLanguageCodeOverride: resolvedConfiguration.preferredLanguageCode)

    state.autoLaunchEnabled = daemonLifecycleManager.autoLaunchEnabled
    state.useGitIgnore = self.configuration.useGitIgnore
    state.useVaporIgnore = self.configuration.useVaporIgnore
    state.preferredLanguageCode = self.configuration.preferredLanguageCode
    state.effectiveLanguageCode = localization.effectiveLanguageCode
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
        "use_gitignore": String(state.useGitIgnore),
        "use_vaporignore": String(state.useVaporIgnore),
        "preferred_language": state.preferredLanguageCode ?? "system",
        "effective_language": state.effectiveLanguageCode,
        "version": VaporBuildInfo.version,
        "bundle_build": VaporBuildInfo.buildVersion,
        "git_commit": VaporBuildInfo.gitCommitShort ?? "unknown",
        "vapor_directory": state.vaporDirectoryPath,
      ]
    )
  }

  var availableLanguageCodes: [String] {
    localization.availableLanguageCodes
  }

  var versionDisplay: String {
    VaporBuildInfo.displayVersion
  }

  var buildVersionDisplay: String {
    VaporBuildInfo.buildVersion
  }

  func localized(_ key: String) -> String {
    localization.text(key)
  }

  func localized(_ key: String, _ arguments: CVarArg...) -> String {
    localization.formatted(key, arguments)
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

    let daemonLifecycleManager = self.daemonLifecycleManager
    let logger = self.logger

    lifecycleQueue.async { [weak self] in
      do {
        let result = try daemonLifecycleManager.bootstrapIfNeeded()
        Task { @MainActor [weak self] in
          guard self != nil else {
            return
          }

          logger.info(
            "Daemon lifecycle bootstrap completed",
            metadata: ["result": String(describing: result)]
          )
        }
      } catch {
        Task { @MainActor [weak self] in
          guard let self else {
            return
          }

          self.state.syncState = .error
          logger.error(
            "Daemon lifecycle bootstrap failed",
            metadata: ["error": String(describing: error)]
          )
        }
      }
    }
  }

  func prepareMenubarOnlyStartupSurface() {
    DispatchQueue.main.async { [weak self] in
      guard let self else {
        return
      }

      self.lifecycleCoordinator?.handleMainWindowClosed()
      self.logger.info("Prepared menubar-only startup surface")
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

    let daemonLifecycleManager = self.daemonLifecycleManager
    let logger = self.logger

    lifecycleQueue.async { [weak self] in
      do {
        let result = try daemonLifecycleManager.setAutoLaunchEnabled(nextState)
        let persistedValue = daemonLifecycleManager.autoLaunchEnabled

        Task { @MainActor [weak self] in
          guard let self else {
            return
          }

          self.state.autoLaunchEnabled = persistedValue
          self.configuration.autoLaunchEnabled = persistedValue
          logger.info(
            "Auto-launch toggle completed",
            metadata: [
              "persisted_value": String(persistedValue),
              "result": String(describing: result),
            ]
          )
        }
      } catch {
        Task { @MainActor [weak self] in
          guard let self else {
            return
          }

          self.state.syncState = .error
          logger.error(
            "Auto-launch toggle failed",
            metadata: ["error": String(describing: error)]
          )
        }
      }
    }
  }

  func disableAutoLaunchAndStopNow() {
    logger.info("Disabling auto-launch and requesting immediate daemon stop")

    let daemonLifecycleManager = self.daemonLifecycleManager
    let logger = self.logger

    lifecycleQueue.async { [weak self] in
      do {
        let result = try daemonLifecycleManager.setAutoLaunchEnabled(false, stopDaemonNow: true)
        let persistedValue = daemonLifecycleManager.autoLaunchEnabled

        Task { @MainActor [weak self] in
          guard let self else {
            return
          }

          self.state.autoLaunchEnabled = persistedValue
          self.configuration.autoLaunchEnabled = persistedValue
          logger.warning(
            "Auto-launch disabled with stop-now",
            metadata: ["result": String(describing: result)]
          )
        }
      } catch {
        Task { @MainActor [weak self] in
          guard let self else {
            return
          }

          self.state.syncState = .error
          logger.error(
            "Disable auto-launch and stop-now failed",
            metadata: ["error": String(describing: error)]
          )
        }
      }
    }
  }

  func setUseGitIgnore(_ enabled: Bool) {
    guard configuration.useGitIgnore != enabled else {
      return
    }

    configuration.useGitIgnore = enabled
    state.useGitIgnore = enabled

    do {
      try configurationStore.save(configuration)
      logger.info(
        "Updated useGitIgnore setting",
        metadata: [
          "use_gitignore": String(enabled),
          "note": "applies on next daemon launch",
        ]
      )
    } catch {
      state.syncState = .error
      logger.error(
        "Failed to persist useGitIgnore setting",
        metadata: ["error": String(describing: error)]
      )
    }
  }

  func setUseVaporIgnore(_ enabled: Bool) {
    guard configuration.useVaporIgnore != enabled else {
      return
    }

    configuration.useVaporIgnore = enabled
    state.useVaporIgnore = enabled

    do {
      try configurationStore.save(configuration)
      logger.info(
        "Updated useVaporIgnore setting",
        metadata: [
          "use_vaporignore": String(enabled),
          "note": "applies on next daemon launch",
        ]
      )
    } catch {
      state.syncState = .error
      logger.error(
        "Failed to persist useVaporIgnore setting",
        metadata: ["error": String(describing: error)]
      )
    }
  }

  func setPreferredLanguageCode(_ languageCode: String?) {
    let normalizedLanguageCode = languageCode?.trimmingCharacters(in: .whitespacesAndNewlines)
    let sanitizedLanguageCode: String?
    if let normalizedLanguageCode, !normalizedLanguageCode.isEmpty {
      sanitizedLanguageCode = normalizedLanguageCode.lowercased()
    } else {
      sanitizedLanguageCode = nil
    }

    guard configuration.preferredLanguageCode != sanitizedLanguageCode else {
      return
    }

    configuration.preferredLanguageCode = sanitizedLanguageCode
    refreshLocalization()

    do {
      try configurationStore.save(configuration)
      logger.info(
        "Updated preferred language setting",
        metadata: [
          "preferred_language": configuration.preferredLanguageCode ?? "system",
          "effective_language": state.effectiveLanguageCode,
        ]
      )
    } catch {
      state.syncState = .error
      logger.error(
        "Failed to persist preferred language setting",
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

  private func refreshLocalization() {
    localization = localizationStore.resolve(
      preferredLanguageCodeOverride: configuration.preferredLanguageCode)
    state.preferredLanguageCode = configuration.preferredLanguageCode
    state.effectiveLanguageCode = localization.effectiveLanguageCode
  }

  private static func makeDefaultLifecycleManager(
    vaporDirectoryURL: URL,
    autoLaunchSettingStore: any AutoLaunchSettingStore,
    useGitIgnore: Bool,
    useVaporIgnore: Bool,
    localSyncDirectory: String,
    cloudSyncDirectory: String,
    preIgnoreRules: String,
    postIgnoreRules: String
  ) -> DaemonLifecycleManager {
    if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil {
      return .placeholder()
    }

    let daemonExecutableURL = bundledDaemonExecutableURL()
    let launchAgentLabel = VaporConstants.Daemon.launchAgentLabel
    var daemonEnvironment = [
      "PATH": VaporConstants.Daemon.processPath,
      VaporPaths.directoryEnvironmentKey: vaporDirectoryURL.path,
      VaporPaths.useGitIgnoreEnvironmentKey: useGitIgnore ? "true" : "false",
      VaporPaths.useVaporIgnoreEnvironmentKey: useVaporIgnore ? "true" : "false",
      VaporPaths.localSyncDirectoryEnvironmentKey: localSyncDirectory,
      VaporPaths.cloudSyncDirectoryEnvironmentKey: cloudSyncDirectory,
      VaporPaths.preIgnoreRulesEnvironmentKey: preIgnoreRules,
      VaporPaths.postIgnoreRulesEnvironmentKey: postIgnoreRules,
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

  private static func bundledDaemonExecutableURL() -> URL {
    guard let executableURL = Bundle.main.executableURL else {
      return Bundle.main.bundleURL
        .appendingPathComponent("Contents")
        .appendingPathComponent("MacOS")
        .appendingPathComponent("vapord")
    }

    return
      executableURL
      .deletingLastPathComponent()
      .appendingPathComponent("vapord")
  }

  private static func makeOptionalLoginItemController() -> (any LoginItemControlling)? {
    #if canImport(ServiceManagement)
      if #available(macOS 13.0, *) {
        return SMAppServiceLoginItemController()
      }
    #endif

    return nil
  }
}
