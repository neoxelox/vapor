public enum SyncSurfaceState: String, CaseIterable, Codable, Sendable {
  case idle = "Idle"
  case queued = "Queued"
  case syncing = "Syncing"
  case throttled = "Throttled"
  case suspended = "Suspended"
  case error = "Error"

  public var detail: String {
    switch self {
    case .idle:
      return "No pending work"
    case .queued:
      return "Changes are queued"
    case .syncing:
      return "Applying lightweight sync work"
    case .throttled:
      return "Deferred because system is active"
    case .suspended:
      return "Paused due to pressure policy"
    case .error:
      return "Action required"
    }
  }

  public var labelLocalizationKey: String {
    switch self {
    case .idle:
      return "sync_state_idle_label"
    case .queued:
      return "sync_state_queued_label"
    case .syncing:
      return "sync_state_syncing_label"
    case .throttled:
      return "sync_state_throttled_label"
    case .suspended:
      return "sync_state_suspended_label"
    case .error:
      return "sync_state_error_label"
    }
  }

  public var detailLocalizationKey: String {
    switch self {
    case .idle:
      return "sync_state_idle_detail"
    case .queued:
      return "sync_state_queued_detail"
    case .syncing:
      return "sync_state_syncing_detail"
    case .throttled:
      return "sync_state_throttled_detail"
    case .suspended:
      return "sync_state_suspended_detail"
    case .error:
      return "sync_state_error_detail"
    }
  }
}

public struct AppShellState: Equatable, Codable, Sendable {
  public var syncState: SyncSurfaceState
  public var configurationIssuePath: String?
  public var configurationIssueReason: String?
  public var autoLaunchEnabled: Bool
  public var useGitIgnore: Bool
  public var useVaporIgnore: Bool
  public var preIgnoreRules: String
  public var postIgnoreRules: String
  public var languageCode: String
  public var effectiveLanguageCode: String
  public var providerName: String
  public var vaporDirectoryPath: String
  public var crashLoopPaused: Bool
  /// The app is set to start at login but macOS is blocking the login
  /// item (user disabled it in System Settings, or MDM policy). The UI
  /// shows a hint with a shortcut to System Settings › Login Items.
  public var loginItemRequiresApproval: Bool

  public init(
    syncState: SyncSurfaceState,
    configurationIssuePath: String?,
    configurationIssueReason: String?,
    autoLaunchEnabled: Bool,
    useGitIgnore: Bool,
    useVaporIgnore: Bool,
    preIgnoreRules: String,
    postIgnoreRules: String,
    languageCode: String,
    effectiveLanguageCode: String,
    providerName: String,
    vaporDirectoryPath: String,
    crashLoopPaused: Bool = false,
    loginItemRequiresApproval: Bool = false
  ) {
    self.syncState = syncState
    self.configurationIssuePath = configurationIssuePath
    self.configurationIssueReason = configurationIssueReason
    self.autoLaunchEnabled = autoLaunchEnabled
    self.useGitIgnore = useGitIgnore
    self.useVaporIgnore = useVaporIgnore
    self.preIgnoreRules = preIgnoreRules
    self.postIgnoreRules = postIgnoreRules
    self.languageCode = languageCode
    self.effectiveLanguageCode = effectiveLanguageCode
    self.providerName = providerName
    self.vaporDirectoryPath = vaporDirectoryPath
    self.crashLoopPaused = crashLoopPaused
    self.loginItemRequiresApproval = loginItemRequiresApproval
  }

  public static let initial = AppShellState(
    syncState: .idle,
    configurationIssuePath: nil,
    configurationIssueReason: nil,
    autoLaunchEnabled: true,
    useGitIgnore: true,
    useVaporIgnore: true,
    preIgnoreRules: VaporConfiguration.defaultPreIgnoreRules,
    postIgnoreRules: VaporConfiguration.defaultPostIgnoreRules,
    languageCode: VaporConfiguration.defaultLanguageCode,
    effectiveLanguageCode: VaporConstants.Localization.defaultLanguageCode,
    providerName: VaporConstants.Provider.displayName(
      forKind: VaporConstants.Provider.defaultKind),
    vaporDirectoryPath: VaporPaths.resolveVaporDirectoryURL().path,
    crashLoopPaused: false
  )

  public var statusLine: String {
    "\(syncState.rawValue) · \(providerName)"
  }

  public var hasConfigurationIssue: Bool {
    configurationIssuePath != nil
  }
}
