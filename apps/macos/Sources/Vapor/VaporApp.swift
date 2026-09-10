import AppKit
import SwiftUI
import VaporCore

@main
struct VaporApp: App {
  private static let mainWindowID = "main-window"

  @StateObject private var viewModel: AppShellViewModel
  private let logger = StructuredLogger(component: "app-lifecycle")
  private let menuBarIcons: MenuBarIconSet?

  init() {
    let viewModel = AppShellViewModel()
    _viewModel = StateObject(wrappedValue: viewModel)
    logger.info("Vapor app launched")
    menuBarIcons = MenuBarIcon.loadSet()
    if menuBarIcons == nil {
      logger.warning(
        "Menu bar icon missing from the resource bundle; showing the system symbol instead",
        metadata: ["resource": MenuBarIcon.resourceName]
      )
    }
    viewModel.configureAppRuntimeControllerIfNeeded(MacAppRuntimeController())
    viewModel.prepareMenubarOnlyStartupSurface()
    viewModel.bootstrapDaemonLifecycleIfNeeded()
  }

  var body: some Scene {
    Window(viewModel.localized("app_title"), id: Self.mainWindowID) {
      ContentView(viewModel: viewModel)
        .onDisappear {
          viewModel.handleMainWindowClosed()
        }
    }
    // Menubar-first startup (CLAUDE.md §2.1): the main window opens only
    // via the menubar `openWindow` action, never automatically at launch
    // or via state restoration. An activation-policy change cannot suppress
    // window presentation, so this is the declarative fix.
    .defaultLaunchBehavior(.suppressed)
    .restorationBehavior(.disabled)

    Settings {
      SettingsView(viewModel: viewModel)
    }

    MenuBarExtra {
      MenuBarContentView(
        viewModel: viewModel,
        mainWindowID: Self.mainWindowID,
        openVaporAction: {
          viewModel.handleOpenFromMenuBar()
        },
        quitVaporAction: {
          Task { @MainActor in await viewModel.handleQuitFromMenuBar() }
        }
      )
    } label: {
      MenuBarLabel(
        title: viewModel.localized("app_title"),
        icons: menuBarIcons,
        needsUserAction: viewModel.state.needsUserAction
      )
    }
  }
}
