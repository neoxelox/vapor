import SwiftUI
import VaporCore

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
          .accessibilityIdentifier(AccessibilityID.Dashboard.status)
          Text(viewModel.localized(viewModel.state.syncState.detailLocalizationKey))
            .font(.subheadline)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier(AccessibilityID.Dashboard.statusDetail)
          if let syncDetail = viewModel.state.syncDetail {
            Text(syncDetail)
              .font(.caption)
              .foregroundStyle(.secondary)
              .accessibilityIdentifier(AccessibilityID.Dashboard.syncDetail)
          }
          SyncNowControl(viewModel: viewModel)
          SignInNotice(viewModel: viewModel)
          RestartRequiredNotice(viewModel: viewModel)
          UserActionNotice(viewModel: viewModel)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
      }

      if let configurationIssuePath = viewModel.state.configurationIssuePath,
        let configurationIssueReason = viewModel.state.configurationIssueReason
      {
        configurationIssueView(
          configurationIssuePath: configurationIssuePath,
          configurationIssueReason: configurationIssueReason
        )
      }

      Toggle(viewModel.localized("settings_start_at_login"), isOn: autoLaunchBinding)
        .accessibilityIdentifier(AccessibilityID.Dashboard.autoLaunch)
      loginItemApprovalHint

      Text(viewModel.localized("runtime_directory_format", viewModel.state.vaporDirectoryPath))
        .font(.caption)
        .foregroundStyle(.secondary)
        .accessibilityIdentifier(AccessibilityID.Dashboard.runtimeDirectory)
    }
    .padding(24)
    .frame(minWidth: 460, minHeight: 320)
  }

  /// Shown when macOS blocks the login item: the toggle alone would
  /// claim "starts at login" while the app never launches. Follows the
  /// HIG guidance pattern: a short explanation plus a direct path to
  /// the System Settings pane that resolves it.
  @ViewBuilder
  private var loginItemApprovalHint: some View {
    if viewModel.state.loginItemRequiresApproval {
      VStack(alignment: .leading, spacing: 4) {
        Text(viewModel.localized("login_item_requires_approval_detail"))
          .font(.caption)
          .foregroundStyle(Color.vaporPrimary)
          .accessibilityIdentifier(AccessibilityID.Notice.loginItemApproval)
        Button(viewModel.localized("login_item_open_settings")) {
          viewModel.openLoginItemSettings()
        }
        .controlSize(.small)
        .accessibilityIdentifier(AccessibilityID.Notice.openLoginItemSettings)
      }
    }
  }

  private var autoLaunchBinding: Binding<Bool> {
    Binding(
      get: { viewModel.state.autoLaunchEnabled },
      set: { _ in viewModel.toggleAutoLaunch() }
    )
  }

  @ViewBuilder
  private func configurationIssueView(
    configurationIssuePath: String,
    configurationIssueReason: String
  ) -> some View {
    GroupBox(viewModel.localized("config_issue_group")) {
      VStack(alignment: .leading, spacing: 6) {
        Text(viewModel.localized("config_issue_preserved_file_format", configurationIssuePath))
          .font(.subheadline)
          .accessibilityIdentifier(AccessibilityID.Dashboard.configurationIssue)
        Text(configurationIssueReason)
          .font(.caption)
          .foregroundStyle(.secondary)
        Text(viewModel.localized("config_issue_using_defaults"))
          .font(.caption)
          .foregroundStyle(.secondary)
      }
      .frame(maxWidth: .infinity, alignment: .leading)
    }
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
      if viewModel.state.crashLoopPaused {
        Text(viewModel.localized("crash_loop_paused_title"))
          .font(.headline)
        Text(viewModel.localized("crash_loop_paused_detail"))
          .font(.subheadline)
          .foregroundStyle(.secondary)
      } else {
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
        SignInNotice(viewModel: viewModel)
        UserActionNotice(viewModel: viewModel)
      }
      Divider()
      Button(viewModel.localized("menubar_open_vapor")) {
        openVaporAction()
        openWindow(id: mainWindowID)
      }
      if viewModel.state.crashLoopPaused {
        Button(viewModel.localized("menubar_acknowledge_crash_loop_pause")) {
          Task { @MainActor in await viewModel.acknowledgeCrashLoopPause() }
        }
      } else {
        Button(viewModel.localized("menubar_toggle_auto_launch")) {
          viewModel.toggleAutoLaunch()
        }
      }
      Button(viewModel.localized("menubar_sync_now")) {
        viewModel.syncNow()
      }
      .disabled(!viewModel.state.daemonIsReachable)
      .help(viewModel.localized("sync_now_hint"))
      Button(viewModel.localized("menubar_restart_daemon")) {
        viewModel.restartDaemon()
      }
      .disabled(!viewModel.state.autoLaunchEnabled)
      .help(viewModel.localized("restart_needs_auto_launch_hint"))
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
      if let configurationIssuePath = viewModel.state.configurationIssuePath,
        let configurationIssueReason = viewModel.state.configurationIssueReason
      {
        Section(viewModel.localized("config_issue_group")) {
          Text(viewModel.localized("config_issue_preserved_file_format", configurationIssuePath))
          Text(configurationIssueReason)
            .font(.caption)
            .foregroundStyle(.secondary)
          Text(viewModel.localized("config_issue_using_defaults"))
            .font(.caption)
            .foregroundStyle(.secondary)
        }
      }

      Toggle(viewModel.localized("settings_start_at_login"), isOn: autoLaunchBinding)
        .accessibilityIdentifier(AccessibilityID.Settings.autoLaunch)
      loginItemApprovalHint
      Toggle(viewModel.localized("settings_use_gitignore"), isOn: useGitignoreBinding)
        .accessibilityIdentifier(AccessibilityID.Settings.useGitignore)
      Toggle(viewModel.localized("settings_use_vaporignore"), isOn: useVaporignoreBinding)
        .accessibilityIdentifier(AccessibilityID.Settings.useVaporignore)

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
            .accessibilityIdentifier(AccessibilityID.Settings.preIgnoreRules)
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
            .accessibilityIdentifier(AccessibilityID.Settings.postIgnoreRules)
        }

        Button(viewModel.localized("settings_save_ignore_rules")) {
          viewModel.saveIgnoreRuleSettings()
        }
        .disabled(!viewModel.hasPendingIgnoreRuleChanges)
        .accessibilityIdentifier(AccessibilityID.Settings.saveIgnoreRules)

        Text(viewModel.localized("settings_ignore_rules_apply_note"))
          .font(.caption)
          .foregroundStyle(.secondary)
      }

      Picker(viewModel.localized("settings_language"), selection: languageBinding) {
        ForEach(viewModel.availableLanguageCodes, id: \.self) { code in
          Text(code).tag(code)
        }
      }
      .accessibilityIdentifier(AccessibilityID.Settings.language)

      Text(viewModel.localized("runtime_directory_format", viewModel.state.vaporDirectoryPath))
        .font(.caption)
        .foregroundStyle(.secondary)

      RestartRequiredNotice(viewModel: viewModel)

      Button(viewModel.localized("settings_disable_auto_launch_and_stop_now")) {
        viewModel.disableAutoLaunchAndStopNow()
      }
      .disabled(!viewModel.state.autoLaunchEnabled)
      .accessibilityIdentifier(AccessibilityID.Settings.stopNow)
    }
    .padding(24)
    .frame(width: 420)
  }

  /// Shown when macOS blocks the login item: the toggle alone would
  /// claim "starts at login" while the app never launches. Follows the
  /// HIG guidance pattern: a short explanation plus a direct path to
  /// the System Settings pane that resolves it.
  @ViewBuilder
  private var loginItemApprovalHint: some View {
    if viewModel.state.loginItemRequiresApproval {
      VStack(alignment: .leading, spacing: 4) {
        Text(viewModel.localized("login_item_requires_approval_detail"))
          .font(.caption)
          .foregroundStyle(Color.vaporPrimary)
          .accessibilityIdentifier(AccessibilityID.Notice.loginItemApproval)
        Button(viewModel.localized("login_item_open_settings")) {
          viewModel.openLoginItemSettings()
        }
        .controlSize(.small)
        .accessibilityIdentifier(AccessibilityID.Notice.openLoginItemSettings)
      }
    }
  }

  private var autoLaunchBinding: Binding<Bool> {
    Binding(
      get: { viewModel.state.autoLaunchEnabled },
      set: { _ in viewModel.toggleAutoLaunch() }
    )
  }

  private var useGitignoreBinding: Binding<Bool> {
    Binding(
      get: { viewModel.state.useGitignore },
      set: { viewModel.setUseGitignore($0) }
    )
  }

  private var useVaporignoreBinding: Binding<Bool> {
    Binding(
      get: { viewModel.state.useVaporignore },
      set: { viewModel.setUseVaporignore($0) }
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

  private var languageBinding: Binding<String> {
    Binding(
      get: { viewModel.state.language },
      set: { viewModel.setLanguage($0) }
    )
  }
}

/// Shown while the daemon reports that a setting needing a restart
/// changed, with the action that applies it. Empty otherwise.
struct RestartRequiredNotice: View {
  @ObservedObject var viewModel: AppShellViewModel

  var body: some View {
    if let notice = viewModel.state.restartRequired {
      VStack(alignment: .leading, spacing: 4) {
        Text(viewModel.localized("settings_restart_required_format", notice))
          .font(.caption)
          .foregroundStyle(Color.vaporPrimary)
          .accessibilityIdentifier(AccessibilityID.Notice.restartRequired)
        Button(viewModel.localized("settings_restart_daemon")) {
          viewModel.restartDaemon()
        }
        .controlSize(.small)
        .disabled(!viewModel.state.autoLaunchEnabled)
        .help(viewModel.localized("restart_needs_auto_launch_hint"))
        .accessibilityIdentifier(AccessibilityID.Notice.restart)
      }
    }
  }
}

/// Shown while a provider refused the sign-in: what happened, that
/// nothing is lost, and the button that opens the consent page. The
/// daemon notices the new sign-in by itself, so the notice clears on a
/// later status read rather than when the button returns.
struct SignInNotice: View {
  @ObservedObject var viewModel: AppShellViewModel

  var body: some View {
    if let providerName = viewModel.state.signInProviderName {
      VStack(alignment: .leading, spacing: 4) {
        Text(viewModel.localized("sign_in_required_format", providerName))
          .font(.caption)
          .foregroundStyle(Color.vaporPrimary)
          .accessibilityIdentifier(AccessibilityID.Notice.signIn)
        Text(viewModel.localized("sign_in_required_detail"))
          .font(.caption2)
          .foregroundStyle(.secondary)
        Button(
          viewModel.localized(
            viewModel.state.signInInProgress ? "sign_in_waiting_for_browser" : "sign_in_button"
          )
        ) {
          viewModel.signIn()
        }
        .controlSize(.small)
        .disabled(viewModel.state.signInInProgress)
        .help(viewModel.localized("sign_in_hint"))
        .accessibilityIdentifier(AccessibilityID.Notice.signInButton)
      }
    }
  }
}

/// The questions waiting on the user (conflicts included), as counted
/// by the daemon's status. Empty when nothing is waiting. The count is
/// the signal; the CLI is the place to answer until the app has a pane.
struct UserActionNotice: View {
  @ObservedObject var viewModel: AppShellViewModel

  var body: some View {
    let decisions = viewModel.state.decisions
    if decisions > 0 {
      VStack(alignment: .leading, spacing: 4) {
        Text(
          decisions == 1
            ? viewModel.localized("attention_decision_one")
            : viewModel.localized("attention_decisions_format", Int64(decisions))
        )
        .font(.caption)
        .foregroundStyle(Color.vaporPrimary)
        .accessibilityIdentifier(AccessibilityID.Notice.decisions)
        Text(viewModel.localized("attention_cli_hint"))
          .font(.caption2)
          .foregroundStyle(.secondary)
      }
    }
  }
}

/// The on-demand sync on the Dashboard: a button, and while the scan
/// is held by the throttle, one line saying the button is the way past
/// it. Hidden while the daemon is not reachable, since there is nothing
/// to ask.
struct SyncNowControl: View {
  @ObservedObject var viewModel: AppShellViewModel

  var body: some View {
    if viewModel.state.daemonIsReachable {
      VStack(alignment: .leading, spacing: 4) {
        if viewModel.state.scanIsWaiting {
          Text(viewModel.localized("scan_waiting_hint"))
            .font(.caption)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier(AccessibilityID.Dashboard.scanWaiting)
        }
        Button(viewModel.localized("menubar_sync_now")) {
          viewModel.syncNow()
        }
        .controlSize(.small)
        .disabled(viewModel.state.scanIsRunning)
        .help(viewModel.localized("sync_now_hint"))
        .accessibilityIdentifier(AccessibilityID.Dashboard.syncNow)
      }
    }
  }
}
