import Testing

@testable import VaporAppCore

@Test
func initialStateUsesSafeDefaults() {
  let state = AppShellState.initial
  #expect(state.syncState == .idle)
  #expect(state.autoLaunchEnabled)
  #expect(state.providerName == "Google Drive")
}

@Test
func statusLineIncludesStateAndProvider() {
  let state = AppShellState(
    syncState: .throttled, autoLaunchEnabled: true, providerName: "Google Drive")
  #expect(state.statusLine == "Throttled · Google Drive")
}

@Test
func syncStateDetailsAreNonEmpty() {
  for state in SyncSurfaceState.allCases {
    #expect(!state.detail.isEmpty)
  }
}
