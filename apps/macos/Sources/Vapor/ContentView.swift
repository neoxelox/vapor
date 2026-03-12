import SwiftUI

private enum SidebarDestination: CaseIterable, Identifiable {
  case dashboard
  case diagnostics

  var id: String {
    switch self {
    case .dashboard:
      return "dashboard"
    case .diagnostics:
      return "diagnostics"
    }
  }

  var localizationKey: String {
    switch self {
    case .dashboard:
      return "nav_dashboard"
    case .diagnostics:
      return "nav_diagnostics"
    }
  }

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
        Label(viewModel.localized(destination.localizationKey), systemImage: destination.symbolName)
          .tag(destination)
      }
      .navigationSplitViewColumnWidth(min: 180, ideal: 210)
      .navigationTitle(viewModel.localized("app_title"))
    } detail: {
      detailView
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .navigationTitle(
          selection.map { viewModel.localized($0.localizationKey) }
            ?? viewModel.localized("app_title")
        )
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
      GroupBox(viewModel.localized("diagnostics_current_status")) {
        VStack(alignment: .leading, spacing: 8) {
          Text(
            viewModel.localized(
              "status_line_format",
              viewModel.localized(viewModel.state.syncState.labelLocalizationKey),
              viewModel.state.providerName
            )
          )
          .font(.headline)
          Text(viewModel.localized(viewModel.state.syncState.detailLocalizationKey))
            .font(.subheadline)
            .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
      }

      GroupBox(viewModel.localized("diagnostics_lifecycle")) {
        VStack(alignment: .leading, spacing: 8) {
          Text(
            viewModel.localized(
              viewModel.state.autoLaunchEnabled
                ? "diagnostics_auto_launch_enabled" : "diagnostics_auto_launch_disabled"
            ))
          Text(viewModel.localized("diagnostics_provider_format", viewModel.state.providerName))
            .foregroundStyle(.secondary)
          Text(viewModel.localized("diagnostics_version_format", viewModel.versionDisplay))
            .foregroundStyle(.secondary)
          Text(viewModel.localized("diagnostics_build_format", viewModel.buildVersionDisplay))
            .foregroundStyle(.secondary)
          Text(viewModel.localized("runtime_directory_format", viewModel.state.vaporDirectoryPath))
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
