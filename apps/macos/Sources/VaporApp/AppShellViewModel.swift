import Combine
import VaporAppCore

@MainActor
final class AppShellViewModel: ObservableObject {
  @Published private(set) var state: AppShellState = .initial

  func toggleAutoLaunch() {
    state.autoLaunchEnabled.toggle()
  }

  func cycleSyncState() {
    let allStates = SyncSurfaceState.allCases
    guard let currentIndex = allStates.firstIndex(of: state.syncState) else {
      state.syncState = .idle
      return
    }

    let nextIndex = allStates.index(after: currentIndex)
    let wrappedIndex = nextIndex == allStates.endIndex ? allStates.startIndex : nextIndex
    state.syncState = allStates[wrappedIndex]
  }
}
