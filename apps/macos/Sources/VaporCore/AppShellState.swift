public enum SyncSurfaceState: String, CaseIterable, Codable, Sendable {
  case idle = "Idle"
  case queued = "Queued"
  case syncing = "Syncing"
  case throttled = "Throttled"
  case suspended = "Suspended"
  case error = "Error"

  public var detail: String {
    switch self {
    case .idle:
      return "No pending work"
    case .queued:
      return "Changes are queued"
    case .syncing:
      return "Applying lightweight sync work"
    case .throttled:
      return "Deferred because system is active"
    case .suspended:
      return "Paused due to pressure policy"
    case .error:
      return "Action required"
    }
  }
}

public struct AppShellState: Equatable, Codable, Sendable {
  public var syncState: SyncSurfaceState
  public var autoLaunchEnabled: Bool
  public var providerName: String

  public init(syncState: SyncSurfaceState, autoLaunchEnabled: Bool, providerName: String) {
    self.syncState = syncState
    self.autoLaunchEnabled = autoLaunchEnabled
    self.providerName = providerName
  }

  public static let initial = AppShellState(
    syncState: .idle,
    autoLaunchEnabled: true,
    providerName: "Google Drive"
  )

  public var statusLine: String {
    "\(syncState.rawValue) · \(providerName)"
  }
}
