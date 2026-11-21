import SwiftUI
import VaporCore

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

  var accessibilityID: String {
    switch self {
    case .dashboard:
      return AccessibilityID.Sidebar.dashboard
    case .diagnostics:
      return AccessibilityID.Sidebar.diagnostics
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
          .accessibilityIdentifier(destination.accessibilityID)
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
          .accessibilityIdentifier(AccessibilityID.Diagnostics.status)
          Text(viewModel.localized(viewModel.state.syncState.detailLocalizationKey))
            .font(.subheadline)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier(AccessibilityID.Diagnostics.statusDetail)
          SignInNotice(viewModel: viewModel)
          UserActionNotice(viewModel: viewModel)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
      }

      GroupBox(viewModel.localized("diagnostics_lifecycle")) {
        VStack(alignment: .leading, spacing: 8) {
          Text(
            viewModel.localized(
              viewModel.state.autoLaunchEnabled
                ? "diagnostics_auto_launch_enabled" : "diagnostics_auto_launch_disabled"
            )
          )
          .accessibilityIdentifier(AccessibilityID.Diagnostics.autoLaunch)
          Text(viewModel.localized("diagnostics_provider_format", viewModel.state.providerName))
            .foregroundStyle(.secondary)
            .accessibilityIdentifier(AccessibilityID.Diagnostics.provider)
          Text(viewModel.localized("diagnostics_version_format", viewModel.versionDisplay))
            .foregroundStyle(.secondary)
            .accessibilityIdentifier(AccessibilityID.Diagnostics.version)
          Text(viewModel.localized("diagnostics_build_format", viewModel.buildVersionDisplay))
            .foregroundStyle(.secondary)
            .accessibilityIdentifier(AccessibilityID.Diagnostics.build)
          Text(viewModel.localized("runtime_directory_format", viewModel.state.vaporDirectoryPath))
            .foregroundStyle(.secondary)
            .accessibilityIdentifier(AccessibilityID.Diagnostics.runtimeDirectory)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
      }

      Spacer(minLength: 0)
    }
    .padding(24)
    .frame(minWidth: 520, minHeight: 360)
  }
}
