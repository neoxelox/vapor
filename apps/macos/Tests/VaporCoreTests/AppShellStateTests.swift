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
  #expect(state.useGitIgnore)
  #expect(state.useVaporIgnore)
  #expect(state.preIgnoreRules == VaporConfiguration.defaultPreIgnoreRules)
  #expect(state.postIgnoreRules == VaporConfiguration.defaultPostIgnoreRules)
  #expect(state.languageCode == "en")
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
    useGitIgnore: true,
    useVaporIgnore: true,
    preIgnoreRules: VaporConfiguration.defaultPreIgnoreRules,
    postIgnoreRules: VaporConfiguration.defaultPostIgnoreRules,
    languageCode: "en",
    effectiveLanguageCode: "en",
    providerName: "Google Drive",
    vaporDirectoryPath: "~/.vapor"
  )

  #expect(state.hasConfigurationIssue)
}

@Test
func surfaceStateFollowsTheDaemonStatusWhenRunning() {
  func status(
    run: String = "Running", throttle: String = "IdleDrain", queue: UInt64 = 0,
    failed: UInt64 = 0
  ) -> DaemonStatusSnapshot {
    DaemonStatusSnapshot(
      runState: run, throttleState: throttle, throttleReason: "", providerName: "filesystem",
      queueDepth: queue, failedIntents: failed)
  }
  #expect(SyncSurfaceState.from(health: .running, status: status()) == .idle)
  #expect(SyncSurfaceState.from(health: .running, status: status(queue: 3)) == .syncing)
  #expect(
    SyncSurfaceState.from(health: .running, status: status(throttle: "Throttled", queue: 3))
      == .throttled)
  #expect(
    SyncSurfaceState.from(health: .running, status: status(throttle: "Suspended", queue: 3))
      == .suspended)
  #expect(SyncSurfaceState.from(health: .running, status: status(run: "Paused")) == .paused)
  #expect(SyncSurfaceState.from(health: .running, status: status(run: "Error")) == .error)
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

  state.decisionsPending = 1
  #expect(state.needsUserAction)
  state.decisionsPending = 0

  state.conflictsUnresolved = 2
  #expect(state.needsUserAction)
  state.conflictsUnresolved = 0

  state.crashLoopPaused = true
  #expect(state.needsUserAction)
  state.crashLoopPaused = false

  state.loginItemRequiresApproval = true
  #expect(state.needsUserAction)
  state.loginItemRequiresApproval = false

  state.configRestartRequired = "syncMode changed"
  #expect(state.needsUserAction)
  state.configRestartRequired = nil

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
    throttle: String = "user activity is active",
    runStateReason: String? = nil,
    scan: String = "",
    scanDetail: String = ""
  ) -> DaemonStatusSnapshot {
    DaemonStatusSnapshot(
      runState: "Running", throttleState: "Throttled", throttleReason: throttle,
      providerName: "filesystem", queueDepth: 1, failedIntents: 0,
      runStateReason: runStateReason, reconcileState: scan, reconcileDetail: scanDetail)
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
        scan: "waiting", scanDetail: "waiting for an idle moment: user activity is active")
    ) == "waiting for an idle moment: user activity is active")
  #expect(
    AppShellState.syncDetail(
      for: .syncing, status: status(scan: "running", scanDetail: "scanning /Users/alex/Vapor")
    ) == "scanning /Users/alex/Vapor")
  // Under Error the daemon's own reason wins over both.
  #expect(
    AppShellState.syncDetail(
      for: .error,
      status: status(
        runStateReason: "cloud sync directory /Vapor is unavailable", scan: "waiting",
        scanDetail: "waiting")
    ) == "cloud sync directory /Vapor is unavailable")
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
