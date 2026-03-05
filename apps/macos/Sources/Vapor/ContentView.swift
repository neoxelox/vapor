import SwiftUI

private enum SidebarDestination: String, CaseIterable, Identifiable {
  case dashboard = "Dashboard"
  case diagnostics = "Diagnostics"

  var id: String { rawValue }

  var symbolName: String {
    switch self {
    case .dashboard:
      return "speedometer"
    case .diagnostics:
      return "stethoscope"
    }
  }
}

struct ContentView: View {
  @ObservedObject var viewModel: AppShellViewModel
  @State private var selection: SidebarDestination? = .dashboard

  var body: some View {
    NavigationSplitView {
      List(SidebarDestination.allCases, selection: $selection) { destination in
        Label(destination.rawValue, systemImage: destination.symbolName)
          .tag(destination)
      }
      .navigationSplitViewColumnWidth(min: 180, ideal: 210)
      .navigationTitle("Vapor")
    } detail: {
      detailView
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .navigationTitle(selection?.rawValue ?? "Vapor")
    }
    .toolbar {
      ToolbarItemGroup {
        Button("Pause / Resume", systemImage: "pause.circle") {
          viewModel.cycleSyncState()
        }
        Button("Flush now", systemImage: "arrow.clockwise.circle") {
          viewModel.cycleSyncState()
        }
      }
    }
  }

  @ViewBuilder
  private var detailView: some View {
    switch selection ?? .dashboard {
    case .dashboard:
      ShellView(viewModel: viewModel)
    case .diagnostics:
      DiagnosticsSummaryView(viewModel: viewModel)
    }
  }
}

private struct DiagnosticsSummaryView: View {
  @ObservedObject var viewModel: AppShellViewModel

  var body: some View {
    VStack(alignment: .leading, spacing: 16) {
      GroupBox("Current Status") {
        VStack(alignment: .leading, spacing: 8) {
          Text(viewModel.state.statusLine)
            .font(.headline)
          Text(viewModel.state.syncState.detail)
            .font(.subheadline)
            .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
      }

      GroupBox("Lifecycle") {
        VStack(alignment: .leading, spacing: 8) {
          Text("Auto-launch is \(viewModel.state.autoLaunchEnabled ? "enabled" : "disabled")")
          Text("Provider: \(viewModel.state.providerName)")
            .foregroundStyle(.secondary)
          Text("Runtime directory: \(viewModel.state.vaporDirectoryPath)")
            .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
      }

      Spacer(minLength: 0)
    }
    .padding(24)
    .frame(minWidth: 520, minHeight: 360)
  }
}
