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
func statusLineIncludesStateAndProvider() {
  let state = AppShellState(
    syncState: .throttled,
    configurationIssuePath: nil,
    configurationIssueReason: nil,
    autoLaunchEnabled: true,
    useGitIgnore: true,
    useVaporIgnore: true,
    preIgnoreRules: "node_modules/",
    postIgnoreRules: "!node_modules/keep.txt",
    languageCode: "en",
    effectiveLanguageCode: "en",
    providerName: "Google Drive",
    vaporDirectoryPath: "~/.vapor"
  )
  #expect(state.statusLine == "Throttled · Google Drive")
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
func syncStateDetailsAreNonEmpty() {
  for state in SyncSurfaceState.allCases {
    #expect(!state.detail.isEmpty)
  }
}
