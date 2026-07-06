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
  }
}
