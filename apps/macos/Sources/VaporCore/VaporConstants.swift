import Foundation

public enum VaporConstants {
  public enum Environment {
    public static let vaporDirectory = "VAPOR_DIR"
    public static let vaporEnvironment = "VAPOR_ENV"
    public static let vaporLogLevel = "VAPOR_LOG_LEVEL"
    public static let useGitIgnore = "VAPOR_USE_GITIGNORE"
    public static let useVaporIgnore = "VAPOR_USE_VAPORIGNORE"
    public static let syncDirectories = "VAPOR_SYNC_DIRECTORIES"
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
    public static let vaporDirectoryName = ".vapor"
  }

  public enum Defaults {
    public static let autoLaunchEnabled = true
    public static let useGitIgnore = true
    public static let useVaporIgnore = true
    public static let syncDirectories = ["~/Vapor"]
    public static let postIgnoreRules = ""
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

  public enum Daemon {
    public static let launchAgentLabel = "sh.arn.vapor.daemon"
    public static let processPath = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
  }
}
