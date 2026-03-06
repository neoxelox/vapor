import AppKit
import VaporCore

@MainActor
struct MacAppRuntimeController: AppRuntimeControlling {
  func setDockVisible(_ isVisible: Bool) {
    let application = NSApp ?? NSApplication.shared
    let targetPolicy: NSApplication.ActivationPolicy = isVisible ? .regular : .accessory
    guard application.activationPolicy() != targetPolicy else {
      return
    }

    application.setActivationPolicy(targetPolicy)

    if isVisible {
      application.activate(ignoringOtherApps: true)
    }
  }

  func terminateApplication() {
    (NSApp ?? NSApplication.shared).terminate(nil)
  }
}
