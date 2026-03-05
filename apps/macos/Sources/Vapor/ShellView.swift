import SwiftUI

struct ShellView: View {
  @ObservedObject var viewModel: AppShellViewModel

  var body: some View {
    VStack(alignment: .leading, spacing: 16) {
      Text("vapor")
        .font(.largeTitle)
        .fontWeight(.semibold)

      GroupBox("Sync Status") {
        VStack(alignment: .leading, spacing: 8) {
          Text(viewModel.state.statusLine)
            .font(.headline)
          Text(viewModel.state.syncState.detail)
            .font(.subheadline)
            .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
      }

      Toggle("Start vapor at login", isOn: autoLaunchBinding)

      HStack {
        Button("Pause / Resume") {
          viewModel.cycleSyncState()
        }

        Button("Flush now") {
          viewModel.cycleSyncState()
        }
      }
    }
    .padding(24)
    .frame(minWidth: 460, minHeight: 320)
  }

  private var autoLaunchBinding: Binding<Bool> {
    Binding(
      get: { viewModel.state.autoLaunchEnabled },
      set: { _ in viewModel.toggleAutoLaunch() }
    )
  }
}

struct MenuBarContentView: View {
  @Environment(\.openWindow) private var openWindow
  @ObservedObject var viewModel: AppShellViewModel
  let mainWindowID: String
  let openVaporAction: () -> Void
  let quitVaporAction: () -> Void

  var body: some View {
    VStack(alignment: .leading, spacing: 10) {
      Text(viewModel.state.statusLine)
        .font(.headline)
      Text(viewModel.state.syncState.detail)
        .font(.subheadline)
        .foregroundStyle(.secondary)
      Divider()
      Button("Open Vapor") {
        openVaporAction()
        openWindow(id: mainWindowID)
      }
      Button("Cycle status") {
        viewModel.cycleSyncState()
      }
      Button("Toggle auto-launch") {
        viewModel.toggleAutoLaunch()
      }
      Divider()
      Button("Quit Vapor") {
        quitVaporAction()
      }
    }
    .padding(14)
    .frame(width: 280)
  }
}

struct SettingsView: View {
  @ObservedObject var viewModel: AppShellViewModel

  var body: some View {
    Form {
      Toggle("Start vapor at login", isOn: autoLaunchBinding)
      Button("Disable auto-launch and stop now") {
        viewModel.disableAutoLaunchAndStopNow()
      }
      .disabled(!viewModel.state.autoLaunchEnabled)
    }
    .padding(24)
    .frame(width: 420)
  }

  private var autoLaunchBinding: Binding<Bool> {
    Binding(
      get: { viewModel.state.autoLaunchEnabled },
      set: { _ in viewModel.toggleAutoLaunch() }
    )
  }
}
