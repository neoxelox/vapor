import Foundation

public enum VaporConstants {
  public enum Environment {
    public static let vaporDirectory = "VAPOR_DIR"
    public static let vaporEnvironment = "VAPOR_ENV"
    public static let vaporLogLevel = "VAPOR_LOG_LEVEL"
    public static let gdriveClientId = "VAPOR_GDRIVE_CLIENT_ID"
    public static let gdriveClientSecret = "VAPOR_GDRIVE_CLIENT_SECRET"
    public static let throttleInputs = "VAPOR_THROTTLE_INPUTS"
    public static let secretsCommand = "VAPOR_SECRETS_COMMAND"
  }

  public enum Runtime {
    public static let logsDirectoryName = "logs"
    public static let stateDirectoryName = "state"
    public static let configurationFileName = "vapor.json"
    public static let sqliteDatabaseFileName = "vapor.sqlite"
    public static let appLogFileName = "vapor.log"
    public static let daemonLogFileName = "vapord.log"
    public static let daemonStdoutLogFileName = "vapord.stdout.log"
    public static let daemonStderrLogFileName = "vapord.stderr.log"
    public static let vaporDirectoryName = ".vapor"
  }

  public enum Defaults {
    public static let autoLaunch = true
    public static let useGitignore = true
    public static let useVaporignore = true
    public static let localRoot = "~/Vapor"
    /// The cloud root a remote provider uses while `cloudRoot`
    /// is unset. Mirrors `constants.rs::filtering::DEFAULT_CLOUD_ROOT`.
    public static let cloudRoot = "/Vapor"
    /// The cloud root the filesystem provider uses while the key is
    /// unset. Mirrors `DEFAULT_FILESYSTEM_CLOUD_ROOT`; the
    /// daemon picks between the two by provider, which is why the app
    /// never writes a cloud root it did not read.
    public static let filesystemCloudRoot = "~/cloud/Vapor"
    public static let postIgnoreRules = ""
    public static let languageCode = VaporConstants.Localization.defaultLanguageCode
    public static let timelineLimit = 1000
    /// Mirrors `constants.rs::provider::DEFAULT` / `sync_mode::DEFAULT`.
    public static let provider = "filesystem"
    public static let syncMode = "two-way"

    public static let preIgnoreRuleLines: [String] = [
      // OS junk
      ".DS_Store",
      "Thumbs.db",
      "desktop.ini",
      // What an OS keeps at the root of a volume, for a sync root that
      // is a whole external drive
      ".fseventsd/",
      ".Spotlight-V100/",
      ".Trashes/",
      ".TemporaryItems/",
      ".DocumentRevisions-V100/",
      "System Volume Information/",
      "$RECYCLE.BIN/",
      "lost+found/",
      ".Trash-*/",
      // Editor swap / temp / partial files
      "*.tmp",
      "*.temp",
      "*.swp",
      "*.swo",
      "*~",
      "*.bak",
      "*.part",
      "*.crdownload",
      "*.log",
      // Generic build & cache outputs
      "dist/",
      "build/",
      "out/",
      "coverage/",
      ".cache/",
      // JavaScript / TypeScript
      "node_modules/",
      ".pnpm-store/",
      ".yarn/cache/",
      ".yarn/unplugged/",
      ".npm/",
      ".next/",
      ".nuxt/",
      ".svelte-kit/",
      ".astro/",
      ".angular/",
      ".expo/",
      ".turbo/",
      ".vite/",
      ".parcel-cache/",
      "storybook-static/",
      "*.tsbuildinfo",
      ".eslintcache",
      // Rust / JVM build trees
      "target/",
      ".gradle/",
      "*.class",
      // Python
      "__pycache__/",
      "*.pyc",
      ".venv/",
      "venv/",
      ".tox/",
      ".mypy_cache/",
      ".pytest_cache/",
      ".ruff_cache/",
      ".ipynb_checkpoints/",
      "*.egg-info/",
      ".eggs/",
      // Vendored dependencies (Go / PHP / Ruby)
      "vendor/",
      ".bundle/",
      // .NET intermediate output
      "obj/",
      // Elixir
      "_build/",
      "deps/",
      // Swift / Xcode / CocoaPods
      "DerivedData/",
      ".build/",
      "Pods/",
      // Haskell
      ".stack-work/",
      "dist-newstyle/",
      // C / C++ objects & CMake trees
      "*.o",
      "CMakeFiles/",
      "cmake-build-*/",
      // Dart / Flutter
      ".dart_tool/",
      // Zig
      "zig-cache/",
      "zig-out/",
      // Terraform
      ".terraform/",
    ]

    public static let preIgnoreRules = preIgnoreRuleLines.joined(separator: "\n")
  }

  public enum Localization {
    public static let localesSubdirectory = "locales"
    public static let defaultLanguageCode = "en"
  }

  /// Accepted `provider` values. Mirrors
  /// `core/shared/src/constants.rs::provider::*` per AGENTS.md §8.6.
  public enum Provider {
    public static let filesystem = "filesystem"
    public static let gdrive = "gdrive"
    public static let all = [filesystem, gdrive]
    public static let defaultKind = filesystem

    /// Mirrors `core/shared/src/constants.rs::provider::BROWSER_SIGN_IN_TIMEOUT_SECONDS`
    /// per AGENTS.md §8.6: how long `vapor auth login` waits for the
    /// browser consent.
    public static let browserSignInTimeoutSeconds = 300

    /// User-facing display name for a `provider` config value. Unknown
    /// values render verbatim so a misconfiguration stays visible.
    public static func displayName(forKind kind: String) -> String {
      switch kind.trimmingCharacters(in: .whitespaces) {
      case gdrive:
        return "Google Drive"
      case filesystem, "":
        return "Filesystem"
      case let other:
        return other
      }
    }
  }

  /// Vapor's primary colour, Aerospace International Orange. The value
  /// is documented in `assets/README.md` (Identity) and every brand
  /// raster under `assets/` shades around it; this is the copy code
  /// consumes. Use it wherever a surface needs a brand or accent colour:
  /// the app accent colour set (`assets/macos/Vapor.xcassets`), the
  /// menu bar mark while Vapor is waiting on the user, and any highlight
  /// that should read as Vapor's own.
  public enum Brand {
    public static let primaryColorHex = "#FF4F00"
    /// sRGB components of `primaryColorHex`, each in 0...1.
    public static let primaryColorRed: Double = 1.0
    public static let primaryColorGreen: Double = 79.0 / 255.0
    public static let primaryColorBlue: Double = 0.0
  }

  public enum Daemon {
    public static let launchAgentLabel = "sh.arn.vapor.daemon"
    /// Mirrors `core/shared/src/constants.rs::service::HEALTH_TICK_INTERVAL_SECONDS`
    /// per AGENTS.md §8.6.
    public static let healthTickIntervalSeconds: TimeInterval = 30
  }

  /// Top-level keys recognized in `vapor.json`. Mirrors
  /// `core/shared/src/constants.rs::config::*` per AGENTS.md §8.6.
  public enum ConfigKeys {
    public static let autoLaunch = "autoLaunch"
    public static let useGitignore = "useGitignore"
    public static let useVaporignore = "useVaporignore"
    public static let localRoot = "localRoot"
    public static let cloudRoot = "cloudRoot"
    public static let preIgnoreRules = "preIgnoreRules"
    public static let postIgnoreRules = "postIgnoreRules"
    public static let languageCode = "languageCode"
    public static let timelineLimit = "timelineLimit"
    public static let provider = "provider"
    public static let syncMode = "syncMode"
    public static let deviceId = "deviceId"
    public static let profiles = "profiles"
    public static let resourceLimits = "resourceLimits"
    public static let idleBoost = "idleBoost"
    public static let safeguards = "safeguards"
    public static let trash = "trash"
  }

  /// Values `vapor … --json` prints. Every value is kebab-case and every
  /// key camelCase. Mirrors the `run_state`, `throttle_state`,
  /// `reconcile_state`, and `service` modules of
  /// `core/shared/src/constants.rs` per AGENTS.md §8.6.
  public enum RunState {
    public static let starting = "starting"
    public static let running = "running"
    public static let paused = "paused"
    public static let error = "error"
  }

  public enum ThrottleState {
    public static let idleDrain = "idle-drain"
    public static let light = "light"
    public static let throttled = "throttled"
    public static let suspended = "suspended"
  }

  public enum ReconcileState {
    public static let idle = "idle"
    public static let waiting = "waiting"
    public static let running = "running"
  }

  public enum ServiceStatus {
    public static let notInstalled = "not-installed"
    public static let stopped = "stopped"
    public static let running = "running"
    public static let crashLoopPaused = "crash-loop-paused"
  }

  public enum ServiceResult {
    public static let unchanged = "unchanged"
    public static let started = "started"
    public static let stopped = "stopped"
    public static let notInstalled = "not-installed"
    public static let crashLoopPaused = "crash-loop-paused"
    public static let relaunchDeferred = "relaunch-deferred"
    public static let acknowledged = "acknowledged"
  }

  public enum ServiceHealth {
    public static let running = "running"
    public static let notInstalled = "not-installed"
    public static let stoppedExpected = "stopped-expected"
    public static let restartedAfterCrash = "restarted-after-crash"
    public static let restartDeferred = "restart-deferred"
    public static let crashLoopPaused = "crash-loop-paused"
  }

  /// Accepted `syncMode` values. Mirrors
  /// `core/shared/src/constants.rs::sync_mode::*` per AGENTS.md §8.6.
  public enum SyncModes {
    public static let twoWay = "two-way"
    public static let pullOnly = "pull-only"
    public static let pushOnly = "push-only"
    public static let all = [twoWay, pullOnly, pushOnly]
  }
}
