import VaporCore

#if canImport(ServiceManagement)
  import ServiceManagement

  @available(macOS 13.0, *)
  final class SMAppServiceLoginItemController: LoginItemControlling {
    private let service: SMAppService

    init() {
      service = .mainApp
    }

    func register() throws {
      try service.register()
    }

    func unregister() throws {
      try service.unregister()
    }
  }
#endif
