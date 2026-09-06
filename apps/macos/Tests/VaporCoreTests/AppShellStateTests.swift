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
