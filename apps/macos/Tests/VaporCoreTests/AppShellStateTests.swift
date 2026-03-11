import Testing

@testable import VaporCore

@Test
func initialStateUsesSafeDefaults() {
  let state = AppShellState.initial
  #expect(state.syncState == .idle)
  #expect(state.autoLaunchEnabled)
  #expect(state.useGitIgnore)
  #expect(state.useVaporIgnore)
  #expect(state.preIgnoreRules == VaporConfiguration.defaultPreIgnoreRules)
  #expect(state.postIgnoreRules == VaporConfiguration.defaultPostIgnoreRules)
  #expect(state.languageCode == "en")
  #expect(state.effectiveLanguageCode == "en")
  #expect(state.providerName == "Google Drive")
  #expect(!state.vaporDirectoryPath.isEmpty)
}

@Test
func statusLineIncludesStateAndProvider() {
  let state = AppShellState(
    syncState: .throttled,
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
func syncStateDetailsAreNonEmpty() {
  for state in SyncSurfaceState.allCases {
    #expect(!state.detail.isEmpty)
  }
}
