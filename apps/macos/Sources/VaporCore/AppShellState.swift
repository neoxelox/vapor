public enum SyncSurfaceState: String, CaseIterable, Codable, Sendable {
  case idle = "Idle"
  case queued = "Queued"
  case syncing = "Syncing"
  case throttled = "Throttled"
  case suspended = "Suspended"
  case paused = "Paused"
  case stopped = "Stopped"
  case error = "Error"

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
    case .paused:
      return "sync_state_paused_label"
    case .stopped:
      return "sync_state_stopped_label"
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
    case .paused:
      return "sync_state_paused_detail"
    case .stopped:
      return "sync_state_stopped_detail"
    case .error:
      return "sync_state_error_detail"
    }
  }

  /// Maps one supervision tick (and the daemon's own status when it was
  /// running) onto the surface state. Order matters: a daemon in
  /// `Error` or `Paused` says so before any queue or throttle detail.
  public static func from(
    health: ServiceHealthOutcome,
    status: DaemonStatusSnapshot?
  ) -> SyncSurfaceState {
    switch health {
    case .crashLoopPaused:
      return .error
    case .notInstalled, .stoppedExpected, .restartDeferred:
      return .stopped
    case .running, .restartedAfterCrash:
      break
    }
    guard let status else {
      return .stopped
    }
    switch status.runState {
    case "Error":
      return .error
    case "Paused":
      return .paused
    default:
      break
    }
    if status.throttleState == "Suspended" {
      return .suspended
    }
    if status.queueDepth == 0 {
      return status.failedIntents > 0 ? .error : .idle
    }
    if status.throttleState == "Throttled" {
      return .throttled
    }
    return .syncing
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
  /// The daemon's own throttle reason, shown under the state label
  /// while the daemon is running (diagnostic text, English).
  public var syncDetail: String?
  /// The daemon's notice that a restart-required setting changed.
  public var configRestartRequired: String?
  /// Questions the daemon parked until the user answers them
  /// (`vapor decisions list`): a missing or replaced sync root, a
  /// mass deletion, a file that changed type.
  public var decisionsPending: UInt64
  /// Keep-both conflict copies waiting for the user to pick a version
  /// (`vapor conflicts list`).
  public var conflictsUnresolved: UInt64
  /// The whole-scope scan is queued but the throttle holds it (the
  /// user is active, the machine is busy). Sync now runs it anyway.
  public var scanIsWaiting: Bool
  /// The whole-scope scan holds the reconcile permit right now.
  public var scanIsRunning: Bool

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
    loginItemRequiresApproval: Bool = false,
    syncDetail: String? = nil,
    configRestartRequired: String? = nil,
    decisionsPending: UInt64 = 0,
    conflictsUnresolved: UInt64 = 0,
    scanIsWaiting: Bool = false,
    scanIsRunning: Bool = false
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
    self.syncDetail = syncDetail
    self.configRestartRequired = configRestartRequired
    self.decisionsPending = decisionsPending
    self.conflictsUnresolved = conflictsUnresolved
    self.scanIsWaiting = scanIsWaiting
    self.scanIsRunning = scanIsRunning
  }

  /// The one line under the status label. Under Error or Paused the
  /// run-state reason (a root that cannot be ensured, a decision waited
  /// on) is what the user needs; a scan that waits or runs is next,
  /// since it explains why a change has not moved yet; otherwise the
  /// throttle reason explains the pace. `nil` when the daemon did not
  /// answer or has nothing to say.
  public static func syncDetail(
    for syncState: SyncSurfaceState,
    status: DaemonStatusSnapshot?
  ) -> String? {
    guard let status else {
      return nil
    }
    if syncState == .error || syncState == .paused, let reason = status.runStateReason {
      return reason
    }
    if (status.scanIsWaiting || status.scanIsRunning) && !status.reconcileDetail.isEmpty {
      return status.reconcileDetail
    }
    return status.throttleReason.isEmpty ? nil : status.throttleReason
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

  public var hasConfigurationIssue: Bool {
    configurationIssuePath != nil
  }

  /// A running daemon answered the last status read, so a control that
  /// talks to it (Sync now) has someone to talk to.
  public var daemonIsReachable: Bool {
    switch syncState {
    case .stopped:
      return false
    case .idle, .queued, .syncing, .throttled, .suspended, .paused, .error:
      return !crashLoopPaused
    }
  }

  /// True while Vapor is waiting on something only the user can do:
  /// answer a parked question, pick a side of a conflict, acknowledge a
  /// crash-loop pause, approve the login item, restart after a
  /// restart-required setting, fix an unreadable config file, or look
  /// at an Error state (a sync root the daemon cannot ensure, a failed
  /// intent, a lifecycle command that failed), which the status text
  /// already labels "Action required". The menu bar mark turns the
  /// brand colour while this holds.
  public var needsUserAction: Bool {
    decisionsPending > 0
      || conflictsUnresolved > 0
      || crashLoopPaused
      || loginItemRequiresApproval
      || configRestartRequired != nil
      || hasConfigurationIssue
      || syncState == .error
  }
}
