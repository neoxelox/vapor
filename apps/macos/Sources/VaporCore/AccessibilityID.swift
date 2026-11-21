/// Accessibility identifiers of the app's controls and status texts.
///
/// An identifier is the stable name of one control. Assistive tools and
/// the tests that drive the installed app find a control by it, so it
/// must not change with the copy or with the language. A control that
/// more than one window shows keeps one identifier; a reader narrows
/// the search to the window.
///
/// The menu of the menu bar item has none: macOS builds its items from
/// the SwiftUI views and drops the identifier, so a reader finds an
/// item by its title.
public enum AccessibilityID {
  /// The scene identifier of the main window. macOS reports it as the
  /// window's accessibility identifier.
  public static let mainWindow = "main-window"

  public enum Sidebar {
    public static let dashboard = "sidebar.dashboard"
    public static let diagnostics = "sidebar.diagnostics"
  }

  public enum Dashboard {
    public static let status = "dashboard.status"
    public static let statusDetail = "dashboard.status-detail"
    public static let syncDetail = "dashboard.sync-detail"
    public static let syncNow = "dashboard.sync-now"
    public static let scanWaiting = "dashboard.scan-waiting"
    public static let autoLaunch = "dashboard.auto-launch"
    public static let runtimeDirectory = "dashboard.runtime-directory"
    public static let configurationIssue = "dashboard.configuration-issue"
  }

  public enum Diagnostics {
    public static let status = "diagnostics.status"
    public static let statusDetail = "diagnostics.status-detail"
    public static let autoLaunch = "diagnostics.auto-launch"
    public static let provider = "diagnostics.provider"
    public static let version = "diagnostics.version"
    public static let build = "diagnostics.build"
    public static let runtimeDirectory = "diagnostics.runtime-directory"
  }

  public enum Settings {
    public static let autoLaunch = "settings.auto-launch"
    public static let useGitignore = "settings.use-gitignore"
    public static let useVaporignore = "settings.use-vaporignore"
    public static let preIgnoreRules = "settings.pre-ignore-rules"
    public static let postIgnoreRules = "settings.post-ignore-rules"
    public static let saveIgnoreRules = "settings.save-ignore-rules"
    public static let language = "settings.language"
    public static let stopNow = "settings.stop-now"
  }

  /// Notices that the Dashboard, Diagnostics, and Settings share.
  public enum Notice {
    public static let restartRequired = "notice.restart-required"
    public static let restart = "notice.restart"
    public static let signIn = "notice.sign-in"
    public static let signInButton = "notice.sign-in-button"
    public static let decisions = "notice.decisions"
    public static let loginItemApproval = "notice.login-item-approval"
    public static let openLoginItemSettings = "notice.open-login-item-settings"
  }
}
