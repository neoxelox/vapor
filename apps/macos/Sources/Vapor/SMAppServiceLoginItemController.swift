import VaporCore

#if canImport(ServiceManagement)
  import ServiceManagement

  @available(macOS 13.0, *)
  final class SMAppServiceLoginItemController: LoginItemControlling {
    private let service: SMAppService

    init(loginItemIdentifier: String) {
      service = .loginItem(identifier: loginItemIdentifier)
    }

    func register() throws {
      try service.register()
    }

    func unregister() throws {
      try service.unregister()
    }
  }
#endif
