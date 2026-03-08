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
  public var autoLaunchEnabled: Bool
  public var useGitIgnore: Bool
  public var useVaporIgnore: Bool
  public var preferredLanguageCode: String?
  public var effectiveLanguageCode: String
  public var providerName: String
  public var vaporDirectoryPath: String

  public init(
    syncState: SyncSurfaceState,
    autoLaunchEnabled: Bool,
    useGitIgnore: Bool,
    useVaporIgnore: Bool,
    preferredLanguageCode: String?,
    effectiveLanguageCode: String,
    providerName: String,
    vaporDirectoryPath: String
  ) {
    self.syncState = syncState
    self.autoLaunchEnabled = autoLaunchEnabled
    self.useGitIgnore = useGitIgnore
    self.useVaporIgnore = useVaporIgnore
    self.preferredLanguageCode = preferredLanguageCode
    self.effectiveLanguageCode = effectiveLanguageCode
    self.providerName = providerName
    self.vaporDirectoryPath = vaporDirectoryPath
  }

  public static let initial = AppShellState(
    syncState: .idle,
    autoLaunchEnabled: true,
    useGitIgnore: true,
    useVaporIgnore: true,
    preferredLanguageCode: VaporConfiguration.defaultPreferredLanguageCode,
    effectiveLanguageCode: VaporConstants.Localization.defaultLanguageCode,
    providerName: "Google Drive",
    vaporDirectoryPath: VaporPaths.resolveVaporDirectoryURL().path
  )

  public var statusLine: String {
    "\(syncState.rawValue) · \(providerName)"
  }
}
