import SwiftUI

struct ShellView: View {
  @ObservedObject var viewModel: AppShellViewModel

  var body: some View {
    VStack(alignment: .leading, spacing: 16) {
      Text(viewModel.localized("app_title"))
        .font(.largeTitle)
        .fontWeight(.semibold)

      GroupBox(viewModel.localized("sync_status_group")) {
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

      Toggle(viewModel.localized("settings_start_at_login"), isOn: autoLaunchBinding)

      Text(viewModel.localized("runtime_directory_format", viewModel.state.vaporDirectoryPath))
        .font(.caption)
        .foregroundStyle(.secondary)

      HStack {
        Button(viewModel.localized("toolbar_pause_resume")) {
          viewModel.cycleSyncState()
        }

        Button(viewModel.localized("toolbar_flush_now")) {
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
      Divider()
      Button(viewModel.localized("menubar_open_vapor")) {
        openVaporAction()
        openWindow(id: mainWindowID)
      }
      Button(viewModel.localized("menubar_cycle_status")) {
        viewModel.cycleSyncState()
      }
      Button(viewModel.localized("menubar_toggle_auto_launch")) {
        viewModel.toggleAutoLaunch()
      }
      Divider()
      Button(viewModel.localized("menubar_quit_vapor")) {
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
      Toggle(viewModel.localized("settings_start_at_login"), isOn: autoLaunchBinding)
      Toggle(viewModel.localized("settings_use_gitignore"), isOn: useGitIgnoreBinding)
      Toggle(viewModel.localized("settings_use_vaporignore"), isOn: useVaporIgnoreBinding)

      Section(viewModel.localized("settings_user_ignore_rules")) {
        VStack(alignment: .leading, spacing: 6) {
          Text(viewModel.localized("settings_pre_ignore_rules"))
            .font(.headline)
          Text(viewModel.localized("settings_pre_ignore_rules_help"))
            .font(.caption)
            .foregroundStyle(.secondary)
          TextEditor(text: preIgnoreRulesBinding)
            .font(.system(.body, design: .monospaced))
            .frame(minHeight: 110)
        }

        VStack(alignment: .leading, spacing: 6) {
          Text(viewModel.localized("settings_post_ignore_rules"))
            .font(.headline)
          Text(viewModel.localized("settings_post_ignore_rules_help"))
            .font(.caption)
            .foregroundStyle(.secondary)
          TextEditor(text: postIgnoreRulesBinding)
            .font(.system(.body, design: .monospaced))
            .frame(minHeight: 110)
        }

        Button(viewModel.localized("settings_save_ignore_rules")) {
          viewModel.saveIgnoreRuleSettings()
        }
        .disabled(!viewModel.hasPendingIgnoreRuleChanges)

        Text(viewModel.localized("settings_ignore_rules_apply_note"))
          .font(.caption)
          .foregroundStyle(.secondary)
      }

      Picker(viewModel.localized("settings_language"), selection: languageCodeBinding) {
        ForEach(viewModel.availableLanguageCodes, id: \.self) { code in
          Text(code).tag(code)
        }
      }

      Text(viewModel.localized("runtime_directory_format", viewModel.state.vaporDirectoryPath))
        .font(.caption)
        .foregroundStyle(.secondary)

      Button(viewModel.localized("settings_disable_auto_launch_and_stop_now")) {
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

  private var useGitIgnoreBinding: Binding<Bool> {
    Binding(
      get: { viewModel.state.useGitIgnore },
      set: { viewModel.setUseGitIgnore($0) }
    )
  }

  private var useVaporIgnoreBinding: Binding<Bool> {
    Binding(
      get: { viewModel.state.useVaporIgnore },
      set: { viewModel.setUseVaporIgnore($0) }
    )
  }

  private var preIgnoreRulesBinding: Binding<String> {
    Binding(
      get: { viewModel.state.preIgnoreRules },
      set: { viewModel.updatePreIgnoreRulesDraft($0) }
    )
  }

  private var postIgnoreRulesBinding: Binding<String> {
    Binding(
      get: { viewModel.state.postIgnoreRules },
      set: { viewModel.updatePostIgnoreRulesDraft($0) }
    )
  }

  private var languageCodeBinding: Binding<String> {
    Binding(
      get: { viewModel.state.languageCode },
      set: { viewModel.setLanguageCode($0) }
    )
  }
}
