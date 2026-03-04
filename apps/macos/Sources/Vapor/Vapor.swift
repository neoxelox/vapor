import SwiftUI

@main
struct Vapor: App {
  @StateObject private var viewModel = AppShellViewModel()

  var body: some Scene {
    WindowGroup("vapor") {
      ShellView(viewModel: viewModel)
    }

    Settings {
      SettingsView(viewModel: viewModel)
    }

    MenuBarExtra("vapor", systemImage: "wind") {
      MenuBarContentView(viewModel: viewModel)
    }
  }
}
