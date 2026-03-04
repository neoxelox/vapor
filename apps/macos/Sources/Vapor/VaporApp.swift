import SwiftUI
import VaporCore

@main
struct VaporApp: App {
  @StateObject private var viewModel = AppShellViewModel()
  private let logger = StructuredLogger(component: "app-lifecycle")

  init() {
    logger.info("Vapor app launched")
  }

  var body: some Scene {
    WindowGroup("Vapor") {
      ContentView(viewModel: viewModel)
    }
    .commands {
      CommandMenu("Sync") {
        Button("Pause / Resume") {
          viewModel.cycleSyncState()
        }
        .keyboardShortcut("p")

        Button("Flush Now") {
          viewModel.cycleSyncState()
        }
        .keyboardShortcut("f")
      }
    }

    Settings {
      SettingsView(viewModel: viewModel)
    }

    MenuBarExtra("Vapor", systemImage: "wind") {
      MenuBarContentView(viewModel: viewModel)
    }
  }
}
