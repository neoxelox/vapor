import Testing

@testable import VaporCore

@Test
func providerKindReadsThePreservedConfigKeyAndMapsDisplayNames() {
  var config = VaporConfiguration()
  #expect(config.providerKind == VaporConstants.Provider.filesystem)
  config.additionalKeys[VaporConstants.ConfigKeys.provider] = .string("gdrive")
  #expect(config.providerKind == "gdrive")
  #expect(VaporConstants.Provider.displayName(forKind: "gdrive") == "Google Drive")
  #expect(VaporConstants.Provider.displayName(forKind: "filesystem") == "Filesystem")
  #expect(
    VaporConstants.Provider.displayName(forKind: "gdrvie") == "gdrvie",
    "unknown provider values must stay visible verbatim")
}

@Test
func initialStateUsesSafeDefaults() {
  let state = AppShellState.initial
  #expect(state.syncState == .idle)
  #expect(state.configurationIssuePath == nil)
  #expect(state.configurationIssueReason == nil)
  #expect(!state.hasConfigurationIssue)
  #expect(state.autoLaunchEnabled)
  #expect(state.useGitignore)
  #expect(state.useVaporignore)
  #expect(
    VaporConfiguration.rules(fromEditorText: state.preIgnoreRules)
      == VaporConfiguration.defaultPreIgnoreRules)
  #expect(state.postIgnoreRules.isEmpty)
  #expect(state.language == "en")
  #expect(state.effectiveLanguageCode == "en")
  #expect(state.providerName == "Filesystem")
  #expect(!state.vaporDirectoryPath.isEmpty)
}

@Test
func configurationIssueFlagReflectsPresenceOfDiagnosticPath() {
  let state = AppShellState(
    syncState: .error,
    configurationIssuePath: "/tmp/.vapor/vapor.json",
    configurationIssueReason: "invalid JSON",
    autoLaunchEnabled: true,
    useGitignore: true,
    useVaporignore: true,
    preIgnoreRules: "",
    postIgnoreRules: "",
    language: "en",
    effectiveLanguageCode: "en",
    providerName: "Google Drive",
    vaporDirectoryPath: "~/.vapor"
  )

  #expect(state.hasConfigurationIssue)
}

@Test
func surfaceStateFollowsTheDaemonStatusWhenRunning() {
  func status(
    run: String = "running", throttle: String = "idle", queue: UInt64 = 0,
    failed: UInt64 = 0
  ) -> DaemonStatusSnapshot {
    DaemonStatusSnapshot(
      runState: run, throttleState: throttle, throttleReason: "", provider: "filesystem",
      queued: queue, failed: failed)
  }
  #expect(SyncSurfaceState.from(health: .running, status: status()) == .idle)
  #expect(SyncSurfaceState.from(health: .running, status: status(queue: 3)) == .syncing)
  #expect(
    SyncSurfaceState.from(health: .running, status: status(throttle: "throttled", queue: 3))
      == .throttled)
  #expect(
    SyncSurfaceState.from(health: .running, status: status(throttle: "suspended", queue: 3))
      == .suspended)
  #expect(SyncSurfaceState.from(health: .running, status: status(run: "paused")) == .paused)
  #expect(SyncSurfaceState.from(health: .running, status: status(run: "unconfigured")) == .paused)
  #expect(SyncSurfaceState.from(health: .running, status: status(run: "error")) == .error)
  #expect(SyncSurfaceState.from(health: .running, status: status(failed: 2)) == .error)
  // No status while the tick says running: the endpoint did not answer.
  #expect(SyncSurfaceState.from(health: .running, status: nil) == .stopped)
  #expect(SyncSurfaceState.from(health: .stoppedExpected, status: nil) == .stopped)
  #expect(SyncSurfaceState.from(health: .notInstalled, status: nil) == .stopped)
  #expect(SyncSurfaceState.from(health: .restartDeferred(30), status: nil) == .stopped)
  #expect(SyncSurfaceState.from(health: .crashLoopPaused, status: nil) == .error)
}

@Test
func needsUserActionFollowsEveryStateOnlyTheUserCanClear() {
  var state = AppShellState.initial
  #expect(!state.needsUserAction)

  state.signInRequired = [SignInRequest(providerKind: "gdrive", profileId: "default")]
  #expect(state.needsUserAction)
  #expect(state.signInProviderName == "Google Drive")
  state.signInRequired = []
  #expect(state.signInProviderName == nil)

  state.decisions = 1
  #expect(state.needsUserAction)
  state.decisions = 0

  state.crashLoopPaused = true
  #expect(state.needsUserAction)
  state.crashLoopPaused = false

  state.loginItemRequiresApproval = true
  #expect(state.needsUserAction)
  state.loginItemRequiresApproval = false

  state.restartRequired = "syncMode changed"
  #expect(state.needsUserAction)
  state.restartRequired = nil

  state.configurationIssuePath = "/tmp/.vapor/vapor.json"
  #expect(state.needsUserAction)
  state.configurationIssuePath = nil

  // The Error state's own copy says "Action required"; the mark agrees.
  state.syncState = .error
  #expect(state.needsUserAction)
  state.syncState = .paused
  #expect(!state.needsUserAction)
}

@Test
func syncDetailPrefersTheReasonTheUserCanActOn() {
  func status(
    run: String = "running",
    throttle: String = "user activity is active",
    runReason: String? = nil,
    scan: String = "",
    scanDetail: String = ""
  ) -> DaemonStatusSnapshot {
    DaemonStatusSnapshot(
      runState: run, throttleState: "throttled", throttleReason: throttle,
      provider: "filesystem", queued: 1, failed: 0,
      runReason: runReason, reconcileState: scan, reconcileDetail: scanDetail)
  }
  // Nothing answered: nothing to say.
  #expect(AppShellState.syncDetail(for: .stopped, status: nil) == nil)
  // The throttle explains the pace by default.
  #expect(
    AppShellState.syncDetail(for: .throttled, status: status()) == "user activity is active")
  // A held or running scan explains why nothing has moved yet.
  #expect(
    AppShellState.syncDetail(
      for: .throttled,
      status: status(
        scan: "waiting", scanDetail: "waiting: thermal pressure is critical")
    ) == "waiting: thermal pressure is critical")
  #expect(
    AppShellState.syncDetail(
      for: .syncing, status: status(scan: "running", scanDetail: "scanning /Users/alex/Vapor")
    ) == "scanning /Users/alex/Vapor")
  // Under Error the daemon's own reason wins over both.
  #expect(
    AppShellState.syncDetail(
      for: .error,
      status: status(
        run: "error", runReason: "cloud root /Vapor is unavailable", scan: "waiting",
        scanDetail: "waiting")
    ) == "cloud root /Vapor is unavailable")
  // An Error the app derived from failed changes never borrows the
  // reason of a healthy run.
  #expect(
    AppShellState.syncDetail(for: .error, status: status(runReason: "watching ~/Vapor")) == nil)
  // An empty throttle reason is no detail at all.
  #expect(AppShellState.syncDetail(for: .idle, status: status(throttle: "")) == nil)
}

@Test
func daemonIsReachableOnlyWhileTheDaemonAnswers() {
  var state = AppShellState.initial
  #expect(state.daemonIsReachable)
  state.syncState = .stopped
  #expect(!state.daemonIsReachable)
  state.syncState = .error
  #expect(state.daemonIsReachable)
  state.crashLoopPaused = true
  #expect(!state.daemonIsReachable)
}
