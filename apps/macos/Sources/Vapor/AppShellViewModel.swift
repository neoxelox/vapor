import Combine
import Foundation
import VaporCore

@MainActor
final class AppShellViewModel: ObservableObject {
  @Published private(set) var state: AppShellState = .initial
  private var daemonLifecycleManager: DaemonLifecycleManager
  private let lifecycleManagerFactory: (VaporConfiguration) -> DaemonLifecycleManager
  private let configurationStore: VaporConfigurationStore
  private let localizationStore: VaporLocalizationStore
  private var configuration: VaporConfiguration
  private var localization: VaporLocalizedCatalog
  private let logger = StructuredLogger(component: "app-shell")
  private let lifecycleQueue = DispatchQueue(label: "sh.arn.vapor.lifecycle", qos: .utility)
  private var runtimeController: (any AppRuntimeControlling)?
  private var lifecycleCoordinator: AppLifecycleCoordinator?
  private var hasScheduledBootstrap = false
  private var healthMonitor: DaemonHealthMonitor?

  convenience init() {
    let configurationStore = VaporConfigurationStore()
    let configurationLoadResult = configurationStore.loadResult()
    let configuration = configurationLoadResult.configuration
    let localizationStore = VaporLocalizationStore()
    // Lifecycle policy lives in the Rust core behind the bundled
    // `vapor` CLI; the manager is configuration-independent — the
    // factory stays for tests and future launch-relevant settings.
    let lifecycleManagerFactory: (VaporConfiguration) -> DaemonLifecycleManager = { _ in
      AppShellViewModel.makeDefaultLifecycleManager()
    }
    self.init(
      daemonLifecycleManager: lifecycleManagerFactory(configuration),
      configurationStore: configurationStore,
      localizationStore: localizationStore,
      configuration: configuration,
      configurationLoadIssue: configurationLoadResult.issue,
      lifecycleManagerFactory: lifecycleManagerFactory
    )
  }

  init(
    daemonLifecycleManager: DaemonLifecycleManager,
    configurationStore: VaporConfigurationStore = VaporConfigurationStore(),
    localizationStore: VaporLocalizationStore = VaporLocalizationStore(),
    configuration: VaporConfiguration? = nil,
    configurationLoadIssue: VaporConfigurationLoadIssue? = nil,
    lifecycleManagerFactory: ((VaporConfiguration) -> DaemonLifecycleManager)? = nil
  ) {
    let configurationLoadResult = configuration == nil ? configurationStore.loadResult() : nil
    let resolvedConfiguration = configuration ?? configurationLoadResult!.configuration
    let resolvedConfigurationLoadIssue = configurationLoadIssue ?? configurationLoadResult?.issue

    self.daemonLifecycleManager = daemonLifecycleManager
    self.lifecycleManagerFactory = lifecycleManagerFactory ?? { _ in daemonLifecycleManager }
    self.configurationStore = configurationStore
    self.localizationStore = localizationStore
    self.configuration = resolvedConfiguration
    self.localization = localizationStore.resolve(languageCode: resolvedConfiguration.languageCode)

    // Startup reads must stay in-process (config file only). The
    // lifecycle-backed values (`autoLaunchEnabled`, `crashLoopPaused`)
    // would each spawn a `vapor` CLI subprocess, so the UI seeds from
    // `vapor.json` / defaults here and refreshes asynchronously once
    // the bootstrap and health-tick paths report back.
    state.autoLaunchEnabled = self.configuration.autoLaunch
    // The provider label reflects the loaded configuration; a provider
    // change requires the daemon (and app) to restart anyway, so a
    // startup read is accurate. Live provider status over IPC arrives
    // with the diagnostics work.
    state.providerName = VaporConstants.Provider.displayName(
      forKind: self.configuration.providerKind)
    state.useGitIgnore = self.configuration.useGitIgnore
    state.useVaporIgnore = self.configuration.useVaporIgnore
    state.preIgnoreRules = self.configuration.preIgnoreRules
    state.postIgnoreRules = self.configuration.postIgnoreRules
    state.languageCode = self.configuration.languageCode
    state.effectiveLanguageCode = localization.effectiveLanguageCode
    state.vaporDirectoryPath = self.configurationStore.resolveVaporDirectoryURL().path
    state.configurationIssuePath = resolvedConfigurationLoadIssue?.configPath
    state.configurationIssueReason = resolvedConfigurationLoadIssue?.reason
    state.crashLoopPaused = false
    if resolvedConfigurationLoadIssue != nil {
      state.syncState = .error
    }

    if let resolvedConfigurationLoadIssue {
      logger.error(
        "Preserved unreadable vapor configuration on startup",
        metadata: [
          "config_path": resolvedConfigurationLoadIssue.configPath,
          "error": resolvedConfigurationLoadIssue.reason,
        ]
      )
    }

    logger.debug(
      "Initialized app shell state",
      metadata: [
        "auto_launch_enabled": String(state.autoLaunchEnabled),
        "use_gitignore": String(state.useGitIgnore),
        "use_vaporignore": String(state.useVaporIgnore),
        "language_code": state.languageCode,
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

  var hasPendingIgnoreRuleChanges: Bool {
    Self.normalizeRuleEditorText(state.preIgnoreRules)
      != Self.normalizeRuleEditorText(configuration.preIgnoreRules)
      || Self.normalizeRuleEditorText(state.postIgnoreRules)
        != Self.normalizeRuleEditorText(configuration.postIgnoreRules)
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
        let isPaused = result == .relaunchDeferred(.infinity)
        let loginItemOutcome = daemonLifecycleManager.lastLoginItemRegistrationOutcome()
        Task { @MainActor [weak self] in
          guard let self else {
            return
          }

          self.state.crashLoopPaused = isPaused
          self.state.autoLaunchEnabled = self.configuration.autoLaunch
          self.state.loginItemRequiresApproval =
            self.state.autoLaunchEnabled && Self.loginItemNeedsAttention(loginItemOutcome)
          self.startDaemonHealthMonitoringIfNeeded()
          logger.info(
            "Daemon lifecycle bootstrap completed",
            metadata: [
              "result": String(describing: result),
              "crash_loop_paused": String(isPaused),
            ]
          )
        }
      } catch {
        Task { @MainActor [weak self] in
          guard let self else {
            return
          }

          self.state.syncState = .error
          self.startDaemonHealthMonitoringIfNeeded()
          logger.error(
            "Daemon lifecycle bootstrap failed",
            metadata: ["error": String(describing: error)]
          )
        }
      }
    }
  }

  /// Periodic supervision. The Rust core (via `vapor service
  /// check`) owns detection and restart policy; the monitor only owns
  /// the timer and reflects the outcome into UI state.
  private func startDaemonHealthMonitoringIfNeeded() {
    guard healthMonitor == nil else {
      return
    }

    let monitor = DaemonHealthMonitor(manager: daemonLifecycleManager) { [weak self] outcome in
      Task { @MainActor [weak self] in
        self?.state.crashLoopPaused = outcome == .crashLoopPaused
      }
    }
    healthMonitor = monitor
    monitor.start()
    logger.info("Daemon health monitoring active")
  }

  /// Reads the crash-loop pause state over the `vapor` CLI. The read is
  /// a blocking subprocess round-trip, so it runs on `lifecycleQueue`
  /// (never the main actor) and publishes back after the `await`.
  func refreshCrashLoopPauseState() async {
    state.crashLoopPaused = await readCrashLoopPausedOffMain()
  }

  func acknowledgeCrashLoopPause() async {
    let manager = self.daemonLifecycleManager
    let logger = self.logger
    // Keep the current banner on any CLI failure: silently clearing it
    // would report the app healthy while the daemon is still paused.
    let previous = state.crashLoopPaused
    // Acknowledge and re-read in one background hop so the click never
    // parks the main run loop on two back-to-back subprocess round-trips.
    let paused: Bool = await withCheckedContinuation { continuation in
      lifecycleQueue.async {
        do {
          try manager.acknowledgeCrashLoopPause()
          continuation.resume(returning: try manager.crashLoopPauseState())
        } catch {
          logger.error(
            "Failed to acknowledge crash-loop pause; keeping the current state",
            metadata: ["error": String(describing: error)]
          )
          continuation.resume(returning: previous)
        }
      }
    }
    state.crashLoopPaused = paused
    logger.warning("Acknowledged crash-loop pause from app surface")
  }

  /// Reads the crash-loop state on `lifecycleQueue` so its CLI subprocess
  /// never runs on the main actor; keeps the previous state on error.
  private func readCrashLoopPausedOffMain() async -> Bool {
    let manager = self.daemonLifecycleManager
    let previous = state.crashLoopPaused
    return await withCheckedContinuation { continuation in
      lifecycleQueue.async {
        continuation.resume(returning: (try? manager.crashLoopPauseState()) ?? previous)
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

  func handleQuitFromMenuBar() async {
    // Drain the health tick off the main actor first: `stop()` blocks on
    // the health queue behind any in-flight `vapor service check`, which
    // could otherwise beachball the main thread and (worse) race a
    // restart against the stop below. Running it on `lifecycleQueue` also
    // orders it ahead of the coordinator's stop on the same queue.
    let monitor = self.healthMonitor
    await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
      lifecycleQueue.async {
        monitor?.stop()
        continuation.resume()
      }
    }
    await lifecycleCoordinator?.handleQuitFromMenuBar(serializingOn: lifecycleQueue)
  }

  /// Whether a registration outcome means "the app will not actually
  /// launch at login despite the preference being ON".
  static func loginItemNeedsAttention(_ outcome: LoginItemRegistrationOutcome) -> Bool {
    switch outcome {
    case .requiresApproval, .failed:
      return true
    case .registered, .unavailable:
      return false
    }
  }

  func openLoginItemSettings() {
    logger.info("Opening System Settings at the Login Items pane")
    daemonLifecycleManager.openLoginItemSettings()
  }

  func toggleAutoLaunch() {
    let nextState = !state.autoLaunchEnabled
    // Optimistically flip the published value now so a rapid second click
    // computes its target from this intent (not the pre-op value that only
    // updates after the full CLI round-trip); the queued operations run
    // serially and the final completion publishes the persisted truth.
    state.autoLaunchEnabled = nextState
    logger.info("Toggling auto-launch", metadata: ["next_value": String(nextState)])

    let daemonLifecycleManager = self.daemonLifecycleManager
    let logger = self.logger

    lifecycleQueue.async { [weak self] in
      do {
        let result = try daemonLifecycleManager.setAutoLaunchEnabled(nextState)
        let persistedValue = daemonLifecycleManager.autoLaunchEnabled
        let loginItemOutcome = daemonLifecycleManager.lastLoginItemRegistrationOutcome()

        Task { @MainActor [weak self] in
          guard let self else {
            return
          }

          self.state.autoLaunchEnabled = persistedValue
          self.state.loginItemRequiresApproval =
            persistedValue && Self.loginItemNeedsAttention(loginItemOutcome)
          self.configuration.autoLaunch = persistedValue
          self.clearConfigurationIssueIfResolved()
          logger.info(
            "Auto-launch toggle completed",
            metadata: [
              "persisted_value": String(persistedValue),
              "result": String(describing: result),
              "login_item": String(describing: loginItemOutcome),
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
          self.state.loginItemRequiresApproval = false
          self.configuration.autoLaunch = persistedValue
          self.clearConfigurationIssueIfResolved()
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
      refreshDaemonLifecycleManagerForCurrentConfiguration()
      clearConfigurationIssueIfResolved()
      logger.info(
        "Updated useGitIgnore setting",
        metadata: [
          "use_gitignore": String(enabled),
          "note": "persisted to vapor.json; the daemon reads it on next start",
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
      refreshDaemonLifecycleManagerForCurrentConfiguration()
      clearConfigurationIssueIfResolved()
      logger.info(
        "Updated useVaporIgnore setting",
        metadata: [
          "use_vaporignore": String(enabled),
          "note": "persisted to vapor.json; the daemon reads it on next start",
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

  func updatePreIgnoreRulesDraft(_ rules: String) {
    state.preIgnoreRules = rules
  }

  func updatePostIgnoreRulesDraft(_ rules: String) {
    state.postIgnoreRules = rules
  }

  func saveIgnoreRuleSettings() {
    let normalizedPreIgnoreRules = Self.normalizeRuleEditorText(state.preIgnoreRules)
    let normalizedPostIgnoreRules = Self.normalizeRuleEditorText(state.postIgnoreRules)

    guard
      normalizedPreIgnoreRules != Self.normalizeRuleEditorText(configuration.preIgnoreRules)
        || normalizedPostIgnoreRules != Self.normalizeRuleEditorText(configuration.postIgnoreRules)
    else {
      state.preIgnoreRules = normalizedPreIgnoreRules
      state.postIgnoreRules = normalizedPostIgnoreRules
      return
    }

    configuration.preIgnoreRules = normalizedPreIgnoreRules
    configuration.postIgnoreRules = normalizedPostIgnoreRules
    state.preIgnoreRules = normalizedPreIgnoreRules
    state.postIgnoreRules = normalizedPostIgnoreRules

    do {
      try configurationStore.save(configuration)
      refreshDaemonLifecycleManagerForCurrentConfiguration()
      clearConfigurationIssueIfResolved()
      logger.info(
        "Updated user ignore rules",
        metadata: [
          "pre_rule_count": String(Self.countConfiguredRules(in: normalizedPreIgnoreRules)),
          "post_rule_count": String(Self.countConfiguredRules(in: normalizedPostIgnoreRules)),
          "note": "persisted to vapor.json; the daemon reads it on next start",
        ]
      )
    } catch {
      state.syncState = .error
      logger.error(
        "Failed to persist user ignore rules",
        metadata: ["error": String(describing: error)]
      )
    }
  }

  func setLanguageCode(_ languageCode: String) {
    let sanitizedLanguageCode = Self.normalizedLanguageCode(languageCode)

    guard configuration.languageCode != sanitizedLanguageCode else {
      return
    }

    configuration.languageCode = sanitizedLanguageCode
    refreshLocalization()

    do {
      try configurationStore.save(configuration)
      clearConfigurationIssueIfResolved()
      logger.info(
        "Updated language setting",
        metadata: [
          "language_code": configuration.languageCode,
          "effective_language": state.effectiveLanguageCode,
        ]
      )
    } catch {
      state.syncState = .error
      logger.error(
        "Failed to persist language setting",
        metadata: ["error": String(describing: error)]
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

  private func refreshLocalization() {
    localization = localizationStore.resolve(languageCode: configuration.languageCode)
    state.languageCode = configuration.languageCode
    state.effectiveLanguageCode = localization.effectiveLanguageCode
  }

  private func refreshDaemonLifecycleManagerForCurrentConfiguration() {
    daemonLifecycleManager = lifecycleManagerFactory(configuration)
    // `state.autoLaunchEnabled` is deliberately not re-read here: the
    // manager-backed read spawns a CLI subprocess (main-thread jank)
    // and sync/filter config changes cannot affect autolaunch.
    refreshLifecycleCoordinator()
  }

  private func clearConfigurationIssueIfResolved() {
    guard state.configurationIssuePath != nil || state.configurationIssueReason != nil else {
      return
    }

    state.configurationIssuePath = nil
    state.configurationIssueReason = nil
    if state.syncState == .error {
      state.syncState = .idle
    }
  }

  private static func normalizeRuleEditorText(_ rules: String) -> String {
    rules
      .replacingOccurrences(of: "\r\n", with: "\n")
      .replacingOccurrences(of: "\r", with: "\n")
  }

  private static func normalizedLanguageCode(_ languageCode: String) -> String {
    let normalized = languageCode.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    return normalized.isEmpty ? VaporConfiguration.defaultLanguageCode : normalized
  }

  private static func countConfiguredRules(in rules: String) -> Int {
    normalizeRuleEditorText(rules)
      .split(whereSeparator: \.isNewline)
      .map { $0.trimmingCharacters(in: .whitespaces) }
      .filter { !$0.isEmpty }
      .count
  }

  /// Builds the production lifecycle manager: a thin facade over the
  /// bundled `vapor` CLI. The Rust `core/lifecycle` layer behind
  /// the CLI owns the LaunchAgent definition (per
  /// `docs/operations/macos/launchagent-policy.md`), autolaunch
  /// persistence, and crash-loop policy — one implementation for every
  /// surface, so the CLI and the app can never clobber each other.
  private static func makeDefaultLifecycleManager() -> DaemonLifecycleManager {
    if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil {
      return .placeholder()
    }

    return DaemonLifecycleManager(
      launchAgentController: VaporCLIServiceController(
        cliExecutableURL: bundledCLIExecutableURL()
      ),
      loginItemController: makeOptionalLoginItemController()
    )
  }

  private static func bundledCLIExecutableURL() -> URL {
    VaporBundleLayout.bundledCLIExecutableURL(
      bundleURL: Bundle.main.bundleURL,
      executableURL: Bundle.main.executableURL
    )
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
