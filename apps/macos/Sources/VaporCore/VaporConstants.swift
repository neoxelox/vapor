import Foundation

public enum VaporConstants {
  public enum Environment {
    public static let vaporDirectory = "VAPOR_DIR"
    public static let vaporEnvironment = "VAPOR_ENV"
    public static let vaporLogLevel = "VAPOR_LOG_LEVEL"
    public static let useGitIgnore = "VAPOR_USE_GITIGNORE"
    public static let useVaporIgnore = "VAPOR_USE_VAPORIGNORE"
    public static let localSyncDirectory = "VAPOR_LOCAL_SYNC_DIRECTORY"
    public static let cloudSyncDirectory = "VAPOR_CLOUD_SYNC_DIRECTORY"
    public static let preIgnoreRules = "VAPOR_PRE_IGNORE_RULES"
    public static let postIgnoreRules = "VAPOR_POST_IGNORE_RULES"
    public static let gdriveClientId = "VAPOR_GDRIVE_CLIENT_ID"
    public static let gdriveClientSecret = "VAPOR_GDRIVE_CLIENT_SECRET"
  }

  public enum Runtime {
    public static let logsDirectoryName = "logs"
    public static let stateDirectoryName = "state"
    public static let configurationFileName = "vapor.json"
    public static let sqliteDatabaseFileName = "vapor.sqlite"
    public static let appLogFileName = "vapor.logs"
    public static let daemonLogFileName = "vapord.logs"
    public static let daemonStdoutLogFileName = "vapord.stdout.log"
    public static let daemonStderrLogFileName = "vapord.stderr.log"
    public static let vaporDirectoryName = ".vapor"
  }

  public enum Defaults {
    public static let autoLaunch = true
    public static let useGitIgnore = true
    public static let useVaporIgnore = true
    public static let localSyncDirectory = "~/Vapor"
    public static let cloudSyncDirectory = "/Vapor"
    public static let postIgnoreRules = ""
    public static let languageCode = VaporConstants.Localization.defaultLanguageCode
    public static let timelineEventLimit = 1000
    /// Mirrors `constants.rs::provider::DEFAULT` / `sync_mode::DEFAULT`.
    public static let provider = "filesystem"
    public static let syncMode = "two-way"

    public static let preIgnoreRuleLines: [String] = [
      ".git/",
      ".DS_Store",
      "*.tmp",
      "*.temp",
      "*.swp",
      "*.swo",
      "*~",
      "node_modules/",
      ".pnpm-store/",
      ".yarn/cache/",
      ".yarn/unplugged/",
      ".npm/",
      ".next/",
      ".nuxt/",
      ".svelte-kit/",
      "dist/",
      "build/",
      "out/",
      ".turbo/",
      ".vite/",
      ".parcel-cache/",
      "coverage/",
      "storybook-static/",
      "*.tsbuildinfo",
      ".eslintcache",
      "*.log",
      ".env.local",
    ]

    public static let preIgnoreRules = preIgnoreRuleLines.joined(separator: "\n")
  }

  public enum Localization {
    public static let localesSubdirectory = "locales"
    public static let defaultLanguageCode = "en"
  }

  public enum Daemon {
    public static let launchAgentLabel = "sh.arn.vapor.daemon"
    public static let preGADefaultProviderDisplayName = "Filesystem (stub)"
    /// Mirrors `core/shared/src/constants.rs::service::HEALTH_TICK_INTERVAL_SECONDS`
    /// per AGENTS.md §8.6.
    public static let healthTickIntervalSeconds: TimeInterval = 30
  }

  /// Top-level keys recognized in `vapor.json`. Mirrors
  /// `core/shared/src/constants.rs::config::*` per AGENTS.md §8.6.
  public enum ConfigKeys {
    public static let autoLaunch = "autoLaunch"
    public static let useGitIgnore = "useGitIgnore"
    public static let useVaporIgnore = "useVaporIgnore"
    public static let localSyncDirectory = "localSyncDirectory"
    public static let cloudSyncDirectory = "cloudSyncDirectory"
    public static let preIgnoreRules = "preIgnoreRules"
    public static let postIgnoreRules = "postIgnoreRules"
    public static let languageCode = "languageCode"
    public static let timelineEventLimit = "timelineEventLimit"
    public static let provider = "provider"
    public static let syncMode = "syncMode"
    public static let deviceId = "deviceId"
    public static let profiles = "profiles"
    public static let resourceLimits = "resourceLimits"
    public static let idleBoost = "idleBoost"
  }

  /// Accepted `syncMode` values (C8-59). Mirrors
  /// `core/shared/src/constants.rs::sync_mode::*` per AGENTS.md §8.6.
  public enum SyncModes {
    public static let twoWay = "two-way"
    public static let pullOnly = "pull-only"
    public static let pushOnly = "push-only"
    public static let all = [twoWay, pullOnly, pushOnly]
  }

  /// Accepted `provider` values (C8-2 / C8-54). Mirrors
  /// `core/shared/src/constants.rs::provider::*` per AGENTS.md §8.6.
  public enum Providers {
    public static let filesystem = "filesystem"
    public static let gdrive = "gdrive"
    public static let all = [filesystem, gdrive]
  }
}
