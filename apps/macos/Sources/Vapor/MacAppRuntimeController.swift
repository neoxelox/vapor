import AppKit
import VaporCore

@MainActor
struct MacAppRuntimeController: AppRuntimeControlling {
  func setDockVisible(_ isVisible: Bool) {
    let targetPolicy: NSApplication.ActivationPolicy = isVisible ? .regular : .accessory
    guard NSApp.activationPolicy() != targetPolicy else {
      return
    }

    NSApp.setActivationPolicy(targetPolicy)

    if isVisible {
      NSApp.activate(ignoringOtherApps: true)
    }
  }

  func terminateApplication() {
    NSApp.terminate(nil)
  }
}
