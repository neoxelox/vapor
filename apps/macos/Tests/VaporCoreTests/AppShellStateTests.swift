import Testing

@testable import VaporCore

@Test
func initialStateUsesSafeDefaults() {
  let state = AppShellState.initial
  #expect(state.syncState == .idle)
  #expect(state.autoLaunchEnabled)
  #expect(state.providerName == "Google Drive")
  #expect(!state.vaporDirectoryPath.isEmpty)
}

@Test
func statusLineIncludesStateAndProvider() {
  let state = AppShellState(
    syncState: .throttled,
    autoLaunchEnabled: true,
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
