import VaporCore

#if canImport(ServiceManagement)
  import ServiceManagement

  @available(macOS 13.0, *)
  final class SMAppServiceLoginItemController: LoginItemControlling {
    private let service: SMAppService

    init() {
      service = .mainApp
    }

    @discardableResult
    func register() throws -> LoginItemRegistrationOutcome {
      do {
        try service.register()
      } catch {
        // A denied registration surfaces as an error (commonly
        // "Operation not permitted" after the user disabled the item in
        // System Settings). When the service itself reports it is
        // waiting on approval, the right response is guidance, not a
        // failure.
        if service.status == .requiresApproval {
          return .requiresApproval
        }
        throw error
      }
      switch service.status {
      case .enabled:
        return .registered
      case .requiresApproval:
        return .requiresApproval
      default:
        // register() returned without error but the service is not
        // enabled — report it rather than pretending the app will
        // launch at login.
        return .failed("login item status \(service.status.rawValue) after registration")
      }
    }

    func unregister() throws {
      try service.unregister()
    }

    func openLoginItemSettings() {
      SMAppService.openSystemSettingsLoginItems()
    }
  }
#endif
