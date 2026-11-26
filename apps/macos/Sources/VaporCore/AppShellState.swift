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

  /// Maps one health read (and the daemon's own status when it was
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
    case .running:
      break
    }
    guard let status else {
      return .stopped
    }
    switch status.runState {
    case VaporConstants.RunState.error:
      return .error
    case VaporConstants.RunState.paused, VaporConstants.RunState.unconfigured:
      // No sync profile yet: nothing runs and nothing is wrong. Until
      // the first-run flow exists the surface shows it as paused, with
      // the daemon's reason, which names how to set a profile up.
      return .paused
    default:
      break
    }
    if status.throttleState == VaporConstants.ThrottleState.suspended {
      return .suspended
    }
    if status.queued == 0 {
      return status.failed > 0 ? .error : .idle
    }
    if status.throttleState == VaporConstants.ThrottleState.throttled {
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
  public var useGitignore: Bool
  public var useVaporignore: Bool
  public var preIgnoreRules: String
  public var postIgnoreRules: String
  public var language: String
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
  public var restartRequired: String?
  /// Questions the daemon parked until the user answers them
  /// (`vapor decisions list`): a conflict, a missing or replaced sync
  /// root, a file that changed type.
  public var decisions: UInt64
  /// The whole-scope scan is queued but the throttle holds it (the
  /// user is active, the machine is busy). Sync now runs it anyway.
  public var scanIsWaiting: Bool
  /// The whole-scope scan holds the reconcile permit right now.
  public var scanIsRunning: Bool
  /// Profiles whose provider refused the sign-in (an expired or revoked
  /// token). Sync is on hold for each until the user signs in again.
  public var signInRequired: [SignInRequest]
  /// A browser sign-in started from the app is waiting for the user.
  public var signInInProgress: Bool

  public init(
    syncState: SyncSurfaceState,
    configurationIssuePath: String?,
    configurationIssueReason: String?,
    autoLaunchEnabled: Bool,
    useGitignore: Bool,
    useVaporignore: Bool,
    preIgnoreRules: String,
    postIgnoreRules: String,
    language: String,
    effectiveLanguageCode: String,
    providerName: String,
    vaporDirectoryPath: String,
    crashLoopPaused: Bool = false,
    loginItemRequiresApproval: Bool = false,
    syncDetail: String? = nil,
    restartRequired: String? = nil,
    decisions: UInt64 = 0,
    scanIsWaiting: Bool = false,
    scanIsRunning: Bool = false,
    signInRequired: [SignInRequest] = [],
    signInInProgress: Bool = false
  ) {
    self.syncState = syncState
    self.configurationIssuePath = configurationIssuePath
    self.configurationIssueReason = configurationIssueReason
    self.autoLaunchEnabled = autoLaunchEnabled
    self.useGitignore = useGitignore
    self.useVaporignore = useVaporignore
    self.preIgnoreRules = preIgnoreRules
    self.postIgnoreRules = postIgnoreRules
    self.language = language
    self.effectiveLanguageCode = effectiveLanguageCode
    self.providerName = providerName
    self.vaporDirectoryPath = vaporDirectoryPath
    self.crashLoopPaused = crashLoopPaused
    self.loginItemRequiresApproval = loginItemRequiresApproval
    self.syncDetail = syncDetail
    self.restartRequired = restartRequired
    self.decisions = decisions
    self.scanIsWaiting = scanIsWaiting
    self.scanIsRunning = scanIsRunning
    self.signInRequired = signInRequired
    self.signInInProgress = signInInProgress
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
    if syncState == .error || syncState == .paused {
      // The daemon's reason explains its own run state. An Error the app
      // derived from failed changes comes with the reason of a healthy
      // run ("watching ~/Vapor"), which would only mislead.
      let explains =
        status.runState == VaporConstants.RunState.error
        || status.runState == VaporConstants.RunState.paused
        || status.runState == VaporConstants.RunState.unconfigured
      return explains ? status.runReason : nil
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
    useGitignore: true,
    useVaporignore: true,
    preIgnoreRules: VaporConfiguration.editorText(for: VaporConfiguration.defaultPreIgnoreRules),
    postIgnoreRules: VaporConfiguration.editorText(for: VaporConfiguration.defaultPostIgnoreRules),
    language: VaporConfiguration.defaultLanguage,
    effectiveLanguageCode: VaporConstants.Localization.defaultLanguageCode,
    providerName: VaporConstants.Provider.displayName(
      forKind: VaporConstants.Provider.defaultKind),
    vaporDirectoryPath: VaporPaths.resolveVaporDirectoryURL().path,
    crashLoopPaused: false
  )

  /// The provider to name in the sign-in notice, `nil` while no
  /// profile waits on a sign-in.
  public var signInProviderName: String? {
    signInRequired.first.map { VaporConstants.Provider.displayName(forKind: $0.providerKind) }
  }

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
  /// sign in again, answer a parked question, pick a side of a
  /// conflict, acknowledge a crash-loop pause, approve the login item,
  /// restart after a restart-required setting, fix an unreadable config
  /// file, or look at an Error state (a sync root the daemon cannot
  /// ensure, a failed intent, a lifecycle command that failed), which
  /// the status text already labels "Action required". The menu bar
  /// mark turns the brand colour while this holds.
  public var needsUserAction: Bool {
    !signInRequired.isEmpty
      || decisions > 0
      || crashLoopPaused
      || loginItemRequiresApproval
      || restartRequired != nil
      || hasConfigurationIssue
      || syncState == .error
  }
}
