pub mod env {
    pub const VAPOR_DIR: &str = "VAPOR_DIR";
    pub const VAPOR_ENV: &str = "VAPOR_ENV";
    pub const VAPOR_LOG_LEVEL: &str = "VAPOR_LOG_LEVEL";
    pub const VAPOR_USE_GITIGNORE: &str = "VAPOR_USE_GITIGNORE";
    pub const VAPOR_USE_VAPORIGNORE: &str = "VAPOR_USE_VAPORIGNORE";
    pub const VAPOR_LOCAL_SYNC_DIRECTORY: &str = "VAPOR_LOCAL_SYNC_DIRECTORY";
    pub const VAPOR_CLOUD_SYNC_DIRECTORY: &str = "VAPOR_CLOUD_SYNC_DIRECTORY";
    pub const VAPOR_PRE_IGNORE_RULES: &str = "VAPOR_PRE_IGNORE_RULES";
    pub const VAPOR_POST_IGNORE_RULES: &str = "VAPOR_POST_IGNORE_RULES";
    /// OAuth client credentials for the Google Drive provider
    /// (installed-app PKCE; per-deployment, never baked into the
    /// binary). See `docs/operations/provider-auth-operations.md`.
    pub const VAPOR_GDRIVE_CLIENT_ID: &str = "VAPOR_GDRIVE_CLIENT_ID";
    pub const VAPOR_GDRIVE_CLIENT_SECRET: &str = "VAPOR_GDRIVE_CLIENT_SECRET";
    /// Where the daemon's throttle inputs come from: `host` (default)
    /// reads the native sampler and idle clock; `static` uses the
    /// neutral defaults and zero idle time; `file:<path>` re-reads a
    /// JSON document every sample so a driver can script the inputs.
    /// Test harnesses set `static` so a run is not shaped by whoever is
    /// typing on the machine; the soak driver uses `file:`.
    pub const VAPOR_THROTTLE_INPUTS: &str = "VAPOR_THROTTLE_INPUTS";
}

pub mod runtime {
    /// Executable names. On macOS the CLI ships at
    /// `Contents/Helpers/vapor` and the daemon at `Contents/MacOS/vapord`;
    /// in a build directory they sit side by side.
    pub const CLI_BINARY_NAME: &str = "vapor";
    pub const DAEMON_BINARY_NAME: &str = "vapord";
    pub const VAPOR_DIRECTORY_NAME: &str = ".vapor";
    pub const LOGS_DIRECTORY_NAME: &str = "logs";
    pub const STATE_DIRECTORY_NAME: &str = "state";
    pub const CONFIGURATION_FILE_NAME: &str = "vapor.json";
    pub const SQLITE_DATABASE_FILE_NAME: &str = "vapor.sqlite";
    pub const APP_LOG_FILE_NAME: &str = "vapor.logs";
    pub const DAEMON_LOG_FILE_NAME: &str = "vapord.logs";
    /// launchd / service-manager stdout & stderr redirection targets for
    /// the daemon, under `vapor_dir/logs/`. Referenced by every surface
    /// that writes a service definition (macOS app, `vapor` CLI) per
    /// `docs/operations/macos/launchagent-policy.md`.
    pub const DAEMON_STDOUT_LOG_FILE_NAME: &str = "vapord.stdout.log";
    pub const DAEMON_STDERR_LOG_FILE_NAME: &str = "vapord.stderr.log";
    /// Advisory lock file inside `vapor_dir` that enforces the
    /// one-daemon-per-vapor-dir invariant. Held (via OS file locking)
    /// for the lifetime of the daemon process.
    pub const DAEMON_LOCK_FILE_NAME: &str = "vapord.lock";
    /// Durable daemon-lifecycle side-file under `vapor_dir/state/`.
    /// Persists crash-loop bookkeeping (`consecutive_crashes`,
    /// `last_crash_at_ms`, pause flag) plus supervision expectations so
    /// backoff survives process restarts and is shared by every surface
    /// that drives the lifecycle (`vapor` CLI, macOS app shim).
    pub const LIFECYCLE_STATE_FILE_NAME: &str = "lifecycle.json";
    pub const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
    pub const PRIVATE_FILE_MODE: u32 = 0o600;
    /// Size cap for a single structured-log file before it is rotated.
    /// An always-on daemon logging at Debug under storms would otherwise
    /// grow its log without bound, violating the low-device-impact goal.
    pub const LOG_FILE_MAX_BYTES: u64 = 8 * 1024 * 1024;
    /// How many rotated generations (`<name>.1` … `<name>.N`) are kept
    /// alongside the live file; older generations are dropped.
    pub const LOG_FILE_GENERATIONS: u32 = 3;
}

pub mod state {
    pub const RETRY_SLOWDOWN_UNTIL_KEY: &str = "queue.retry_slowdown_until_ms";
    /// Tombstones older than this are pruned at daemon startup: after a
    /// month, a divergent replica reconciles through content comparison
    /// anyway, and unbounded tombstone growth would violate the memory
    /// and storage bounds.
    pub const TOMBSTONE_RETENTION_MILLIS: u64 = 30 * 24 * 60 * 60 * 1_000;
    /// Terminally-failed intent records older than this are pruned at
    /// daemon startup: a single auth outage can finalize thousands of
    /// rows, and the diagnostics surface only needs recent failures.
    /// Unbounded growth would violate the low-device-impact storage bound.
    pub const FAILED_INTENT_RETENTION_MILLIS: u64 = 30 * 24 * 60 * 60 * 1_000;
    /// Hard cap on retained terminally-failed rows regardless of age, so a
    /// single massive incident cannot bloat the durable DB.
    pub const MAX_FAILED_INTENTS_RETAINED: usize = 10_000;
    pub const MAX_ATTEMPT_COUNT: u32 = 10_000;
    pub const MAX_DIAGNOSTIC_TEXT_LENGTH: usize = 1_024;
    pub const MAX_STATE_KEY_LENGTH: usize = 128;
    pub const MAX_STATE_VALUE_LENGTH: usize = 4_096;
    pub const MAX_TIMESTAMP_MILLIS: i64 = 32_503_680_000_000;
}

pub mod config {
    /// Top-level keys recognized in `vapor.json`. Mirrors the Swift
    /// `VaporConfigurationStore` keyspace; AGENTS.md §8.6 mandates
    /// centralization so the CLI and the macOS app cannot drift.
    pub const KEY_AUTO_LAUNCH: &str = "autoLaunch";
    pub const KEY_USE_GIT_IGNORE: &str = "useGitIgnore";
    pub const KEY_USE_VAPOR_IGNORE: &str = "useVaporIgnore";
    pub const KEY_LOCAL_SYNC_DIRECTORY: &str = "localSyncDirectory";
    pub const KEY_CLOUD_SYNC_DIRECTORY: &str = "cloudSyncDirectory";
    pub const KEY_PRE_IGNORE_RULES: &str = "preIgnoreRules";
    pub const KEY_POST_IGNORE_RULES: &str = "postIgnoreRules";
    pub const KEY_LANGUAGE_CODE: &str = "languageCode";
    pub const KEY_TIMELINE_LIMIT: &str = "timelineLimit";
    /// Provider selection: `filesystem` (default pre-GA) or
    /// `gdrive`. See `provider::*` for the accepted values.
    pub const KEY_PROVIDER: &str = "provider";
    /// Sync direction selector: `two-way` (default),
    /// `pull-only`, `push-only`. See `sync_mode::*` and
    /// `docs/architecture/sync-modes.md`.
    pub const KEY_SYNC_MODE: &str = "syncMode";
    /// Stable per-device identifier used by the keep-both conflict
    /// suffix. Derived from the hostname at first run, persisted
    /// here, and never silently regenerated.
    pub const KEY_DEVICE_ID: &str = "deviceId";
    /// Profile array. Each entry is an object with the
    /// `profile::KEY_*` fields; absent means the single implicit
    /// profile assembled from the top-level settings.
    pub const KEY_PROFILES: &str = "profiles";
    /// User resource-budget group; object with the
    /// `resource_limits::KEY_*` fields.
    pub const KEY_RESOURCE_LIMITS: &str = "resourceLimits";
    /// Idle-boost group; object with the `idle_boost::KEY_*`
    /// fields.
    pub const KEY_IDLE_BOOST: &str = "idleBoost";
    /// Safeguards group; object with the `safeguards::KEY_*` fields.
    pub const KEY_SAFEGUARDS: &str = "safeguards";

    /// Keys a running daemon applies within one poll interval of the
    /// file changing, without a restart. The CLI tells the user which
    /// class a key falls in after `config set`.
    pub const LIVE_RELOAD_KEYS: &[&str] = &[
        KEY_USE_GIT_IGNORE,
        KEY_USE_VAPOR_IGNORE,
        KEY_PRE_IGNORE_RULES,
        KEY_POST_IGNORE_RULES,
        KEY_TIMELINE_LIMIT,
        KEY_RESOURCE_LIMITS,
        KEY_IDLE_BOOST,
        KEY_SAFEGUARDS,
    ];

    /// Keys that reshape the pipeline (roots, provider, direction,
    /// profile set) and therefore take effect on the next daemon start;
    /// a running daemon reports them as `config_restart_required` in
    /// status until it is restarted.
    pub const RESTART_REQUIRED_KEYS: &[&str] = &[
        KEY_LOCAL_SYNC_DIRECTORY,
        KEY_CLOUD_SYNC_DIRECTORY,
        KEY_PROVIDER,
        KEY_SYNC_MODE,
        KEY_PROFILES,
        KEY_DEVICE_ID,
    ];

    /// Every recognized key in one slice. Kept in lockstep with the
    /// `KEY_*` constants above; the CLI uses this for `validate_key`.
    pub const ALL_KEYS: &[&str] = &[
        KEY_AUTO_LAUNCH,
        KEY_USE_GIT_IGNORE,
        KEY_USE_VAPOR_IGNORE,
        KEY_LOCAL_SYNC_DIRECTORY,
        KEY_CLOUD_SYNC_DIRECTORY,
        KEY_PRE_IGNORE_RULES,
        KEY_POST_IGNORE_RULES,
        KEY_LANGUAGE_CODE,
        KEY_TIMELINE_LIMIT,
        KEY_PROVIDER,
        KEY_SYNC_MODE,
        KEY_DEVICE_ID,
        KEY_PROFILES,
        KEY_RESOURCE_LIMITS,
        KEY_IDLE_BOOST,
        KEY_SAFEGUARDS,
    ];

    /// Default values for the config keys whose defaults are not already
    /// hosted by another constants module (`filtering::*` owns the
    /// directory + ignore-rule defaults). Mirrored by the Swift
    /// `VaporConstants.Defaults` per AGENTS.md §8.6 — the Rust side is
    /// the source of truth.
    pub const DEFAULT_AUTO_LAUNCH: bool = true;
    pub const DEFAULT_USE_GIT_IGNORE: bool = true;
    pub const DEFAULT_USE_VAPOR_IGNORE: bool = true;
    pub const DEFAULT_LANGUAGE_CODE: &str = "en";
    pub const DEFAULT_TIMELINE_LIMIT: i64 = 1_000;
}

pub mod provider {
    /// Accepted `provider` config values. `filesystem` is the pre-GA
    /// default; when selected, `cloudSyncDirectory` is
    /// reinterpreted as an absolute local directory that plays the role
    /// of the cloud side. `gdrive` selects the real cloud provider.
    pub const FILESYSTEM: &str = "filesystem";
    pub const GDRIVE: &str = "gdrive";
    pub const DEFAULT: &str = FILESYSTEM;
    pub const ALL: &[&str] = &[FILESYSTEM, GDRIVE];

    /// Extended-attribute name carrying the daemon's operation id on
    /// files the daemon itself wrote (self-write loop prevention).
    /// Doubles as the NTFS ADS stream name once Windows support lands.
    pub const OP_ID_XATTR_NAME: &str = "sh.arn.vapor.op-id";
    /// Side-file suffix used when xattr writes are unavailable
    /// (`ENOTSUP`/`EACCES`/`EROFS`) per `data-flow.md §Loop prevention`:
    /// the fallback for `{path}` is `{path}.vapor-meta.json`. Providers
    /// must hide these from enumeration and changes feeds.
    pub const OP_ID_SIDE_FILE_SUFFIX: &str = ".vapor-meta.json";
    /// Durable state key prefix for provider changes-feed cursors; the
    /// profile id is appended (`provider.changes_cursor.<profile_id>`).
    pub const CHANGES_CURSOR_STATE_KEY_PREFIX: &str = "provider.changes_cursor.";
    /// Prefix of the hidden temp files the filesystem provider (and the
    /// engine's local apply path) write before an atomic rename. Both
    /// enumeration and every changes feed hide these; the local ingest
    /// path filter drops them unconditionally.
    pub const TEMP_FILE_PREFIX: &str = ".vapor-tmp-";
    /// A hidden `TEMP_FILE_PREFIX` staging file older than this is
    /// orphaned crash residue (an interrupted upload/download stage), not
    /// an in-flight transfer, and is reaped so it cannot accumulate in the
    /// user's folder across repeated unclean shutdowns. Conservative so a
    /// legitimately long, throttle-paused transfer's temp is never reaped.
    pub const STALE_TEMP_FILE_MAX_AGE_MILLIS: u64 = 24 * 60 * 60 * 1_000;
    /// Bounded in-memory ring size of the filesystem provider's changes
    /// feed. A cursor older than the ring floor reports `CursorExpired`,
    /// which forces a reconcile instead of silently missing changes.
    pub const CHANGES_FEED_RING_MAX_EVENTS: usize = 8_192;
    /// Profile id used by profile-agnostic provider selection calls.
    pub const DEFAULT_PROFILE_FALLBACK: &str = "default";
    /// Per-socket read/write timeout for the native HTTP transport. A
    /// black-holed connection (Wi-Fi switch, dropped NAT flow) must not
    /// wedge the synchronous provider stack — and therefore the tick loop
    /// — forever; a stalled read surfaces as a transient error the retry
    /// machinery handles.
    pub const HTTP_SOCKET_TIMEOUT_SECONDS: u64 = 120;
    /// Connect timeout for the native HTTP transport.
    pub const HTTP_CONNECT_TIMEOUT_SECONDS: u64 = 30;
    /// Hard cap on a single HTTP response body. Reading one extra byte
    /// past this and erroring (rather than silently truncating) keeps a
    /// mis-ranged full-file download from completing as a corrupt file.
    pub const MAX_HTTP_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
}

pub mod profile {
    /// Keys of each object in the top-level `profiles` array.
    /// A profile inherits any unset override-capable field from the
    /// top-level configuration.
    pub const KEY_ID: &str = "id";
    pub const KEY_NAME: &str = "name";
    pub const KEY_PROVIDER: &str = "provider";
    pub const KEY_LOCAL_SYNC_DIRECTORY: &str = "localSyncDirectory";
    pub const KEY_CLOUD_SYNC_DIRECTORY: &str = "cloudSyncDirectory";
    pub const KEY_SYNC_MODE: &str = "syncMode";
    pub const KEY_ENABLED: &str = "enabled";
    /// The implicit profile id used when no `profiles` array is
    /// configured (single-scope setups; the pre-profile behavior).
    pub const DEFAULT_PROFILE_ID: &str = "default";
    /// Profile ids must be short filesystem-safe slugs: they name the
    /// per-profile durable state directory and namespace secrets.
    pub const MAX_PROFILE_ID_LENGTH: usize = 32;
}

pub mod resource_limits {
    /// Keys of the `resourceLimits` config group: hard user
    /// ceilings on the daemon's device impact, all `1..=100` percent.
    pub const KEY_CPU_PERCENT: &str = "cpuPercent";
    pub const KEY_MEMORY_PERCENT: &str = "memoryPercent";
    pub const KEY_BANDWIDTH_PERCENT: &str = "bandwidthPercent";
    /// Optional hard ceiling on concurrent uploads and on concurrent
    /// downloads (each direction separately). Absent means automatic:
    /// the throttle ladder derives the ceiling from the machine's core
    /// count. Clamped into `1..=16`.
    pub const KEY_MAX_CONCURRENT_TRANSFERS: &str = "maxConcurrentTransfers";
    pub const DEFAULT_CPU_PERCENT: u8 = 15;
    pub const DEFAULT_MEMORY_PERCENT: u8 = 10;
    pub const DEFAULT_BANDWIDTH_PERCENT: u8 = 25;
    pub const MIN_PERCENT: u8 = 1;
    pub const MAX_PERCENT: u8 = 100;
    pub const MIN_CONCURRENT_TRANSFERS: usize = 1;
    pub const MAX_CONCURRENT_TRANSFERS: usize = 16;
}

pub mod idle_boost {
    /// Keys of the `idleBoost` config group: optional dynamic
    /// headroom expansion while the device is verifiably idle.
    pub const KEY_ENABLED: &str = "enabled";
    pub const KEY_MIN_IDLE_SECONDS: &str = "minIdleSeconds";
    pub const KEY_BOOST_CPU_PERCENT: &str = "boostCpuPercent";
    pub const KEY_BOOST_MEMORY_PERCENT: &str = "boostMemoryPercent";
    pub const KEY_BOOST_BANDWIDTH_PERCENT: &str = "boostBandwidthPercent";
    pub const KEY_HEADROOM_CPU_PERCENT: &str = "headroomCpuPercent";
    pub const KEY_RAMP_UP_SECONDS: &str = "rampUpSeconds";
    pub const KEY_RAMP_DOWN_SECONDS: &str = "rampDownSeconds";
    pub const DEFAULT_ENABLED: bool = true;
    pub const DEFAULT_MIN_IDLE_SECONDS: u64 = 300;
    pub const DEFAULT_BOOST_CPU_PERCENT: u8 = 50;
    pub const DEFAULT_BOOST_MEMORY_PERCENT: u8 = 20;
    pub const DEFAULT_BOOST_BANDWIDTH_PERCENT: u8 = 80;
    /// Non-Vapor utilization must stay at or below this for boost to
    /// engage (per-resource headroom gate).
    pub const DEFAULT_HEADROOM_CPU_PERCENT: u8 = 30;
    pub const DEFAULT_RAMP_UP_SECONDS: u64 = 30;
    /// Must stay <= ramp-up so activity resumption is non-invasive
    /// (`data-flow.md §User resource budgets`).
    pub const DEFAULT_RAMP_DOWN_SECONDS: u64 = 10;
}

pub mod safeguards {
    /// Keys of the `safeguards` config group. The mass-delete guard
    /// holds a burst of deletions (in either direction) behind a
    /// decision when the burst reaches the threshold or the ratio
    /// inside a rolling window; the rest of the sync keeps flowing.
    /// The ransomware / bulk-mistake backstop. Configurable because a
    /// workflow that legitimately unlinks many files (`rm -rf` of large
    /// trees, big build cleans) may need a higher threshold.
    pub const KEY_MASS_DELETE_ENABLED: &str = "massDeleteEnabled";
    pub const KEY_MASS_DELETE_THRESHOLD: &str = "massDeleteThreshold";
    pub const KEY_MASS_DELETE_WINDOW_SECONDS: &str = "massDeleteWindowSeconds";
    /// Share of the synced files (percent) whose deletion inside the
    /// window is held for a decision even when the absolute threshold is
    /// not reached, so a small tree is protected too.
    pub const KEY_MASS_DELETE_RATIO_PERCENT: &str = "massDeleteRatioPercent";
    pub const DEFAULT_MASS_DELETE_ENABLED: bool = true;
    /// Floor clamps: a threshold/window too low would trip the guard on
    /// ordinary work and train users to blind-approve it.
    pub const MIN_MASS_DELETE_THRESHOLD: usize = 10;
    pub const MIN_MASS_DELETE_WINDOW_SECONDS: u64 = 5;
    /// The ratio rule never holds fewer deletions than this, so a tree
    /// of three files does not prompt on every second deletion.
    pub const MIN_MASS_DELETE_RATIO_COUNT: usize = 10;
}

pub mod sync_mode {
    /// Accepted `syncMode` config values. The names describe the
    /// direction from the local device's perspective; see
    /// `docs/architecture/sync-modes.md`.
    pub const TWO_WAY: &str = "two-way";
    pub const PULL_ONLY: &str = "pull-only";
    pub const PUSH_ONLY: &str = "push-only";
    pub const DEFAULT: &str = TWO_WAY;
    pub const ALL: &[&str] = &[TWO_WAY, PULL_ONLY, PUSH_ONLY];
}

pub mod service {
    /// Reverse-DNS identifier used by every Vapor surface that talks to
    /// the OS service manager (macOS LaunchAgent, Linux systemd unit,
    /// Windows Task Scheduler task). Mirrored by the Swift constants
    /// file under `apps/macos/Sources/VaporCore/VaporConstants.swift`
    /// per AGENTS.md §8.6.
    pub const DAEMON_LABEL: &str = "sh.arn.vapor.daemon";
    /// Cadence at which app surfaces run `vapor service check` to detect
    /// unexpected daemon exits and route them through the crash-loop
    /// guard. Mirrored by the Swift constants file per AGENTS.md §8.6.
    pub const HEALTH_TICK_INTERVAL_SECONDS: u64 = 30;
}

pub mod secrets {
    /// Namespace under which every native secret store files Vapor's
    /// entries: the Keychain service name on macOS, the credential
    /// target prefix on Windows, the Secret Service attribute on Linux.
    /// Secret names (`auth.<profile>.<provider>.token`) are the account
    /// inside that namespace, so a user can find and remove every Vapor
    /// item in the OS keychain UI by this one string.
    pub const STORE_NAMESPACE: &str = "sh.arn.vapor";
}

pub mod ipc {
    /// Current schema version emitted by every Vapor surface that
    /// participates in the IPC handshake (`vapor` CLI, future macOS /
    /// Windows / Linux apps, the daemon). Bump on any backwards-
    /// incompatible payload shape change. Pre-GA the sliding tolerance
    /// is `|N - M| <= 1`; see `docs/architecture/ipc-contracts.md`.
    pub const SCHEMA_VERSION_CURRENT: u32 = 2;
    /// Minimum peer schema version this build can interoperate with.
    /// Together with [`SCHEMA_VERSION_CURRENT`] this defines the local
    /// support window; the handshake fails when both sides cannot find
    /// an overlapping version.
    pub const SCHEMA_VERSION_MIN_SUPPORTED: u32 = 1;
    /// Maximum size of a single IPC frame in bytes. Frames whose
    /// declared length exceeds this cap are rejected before any
    /// deserialization attempt.
    pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
    /// Filename of the Unix-domain-socket endpoint inside `vapor_dir`.
    pub const SOCKET_FILE_NAME: &str = "vapord.sock";
    /// Portable byte budget for a Unix-domain-socket address path.
    /// `sun_path` is 104 bytes on macOS/BSD and 108 on Linux (both
    /// including the NUL terminator); one conservative shared ceiling
    /// keeps a single build behaving identically everywhere. Socket
    /// paths longer than this are relocated to a short deterministic
    /// path under the OS temp directory (see
    /// `runtime_paths::ipc_socket_location`).
    pub const MAX_SOCKET_PATH_BYTES: usize = 100;
    /// Maximum number of concurrently served IPC connections. Excess
    /// connections are dropped at accept time so a runaway local client
    /// cannot park an unbounded number of daemon threads.
    pub const MAX_CONCURRENT_CONNECTIONS: usize = 32;
    /// Per-connection idle read timeout on the daemon side. A client
    /// that connects and then goes silent for longer than this has its
    /// session reaped instead of holding a thread forever.
    pub const CONNECTION_IDLE_TIMEOUT_MILLIS: u64 = 300_000;
}

pub mod self_write_cache {
    pub const DEFAULT_TTL_MILLIS: u64 = 30_000;
    pub const MIN_TTL_MILLIS: u64 = 5_000;
    pub const MAX_ENTRIES: usize = 10_000;
    pub const MIN_ENTRIES: usize = 1_000;
}

pub mod filtering {
    pub const GIT_IGNORE_FILE_NAME: &str = ".gitignore";
    pub const VAPOR_IGNORE_FILE_NAME: &str = ".vaporignore";
    /// Internal artifact patterns the engine must always ignore on the
    /// local side, independent of user-configurable ignore rules:
    /// in-flight atomic-write temp files and op-id side-files. Loop
    /// prevention depends on these never becoming intents, so they are
    /// enforced in the path filter itself rather than the editable
    /// rule set.
    pub const INTERNAL_IGNORE_FILE_PREFIXES: &[&str] = &[".vapor-tmp-"];
    pub const INTERNAL_IGNORE_FILE_SUFFIXES: &[&str] = &[".vapor-meta.json"];
    pub const DEFAULT_LOCAL_SYNC_DIRECTORY: &str = "~/Vapor";
    pub const DEFAULT_CLOUD_SYNC_DIRECTORY: &str = "/Vapor";
    /// Baseline low-signal exclusions applied before discovered ignore
    /// files: OS junk, editor temp/partial files, and the common
    /// build / cache / dependency directories across ecosystems —
    /// regenerable machine output that would dominate sync traffic.
    /// Deliberately NOT ignored: `.git/` (repositories sync whole) and
    /// dotenv files (a personal sync root is exactly where a backup of
    /// local secrets belongs — project owner decision, 2026-07-13).
    pub const DEFAULT_PRE_IGNORE_RULES: &[&str] = &[
        // OS junk
        ".DS_Store",
        "Thumbs.db",
        "desktop.ini",
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
    ];
}

pub mod engine {
    pub const MAX_IN_MEMORY_PENDING_PATHS: usize = 20_000;
    pub const MAX_IN_MEMORY_PENDING_PATHS_PER_SUBTREE: usize = 5_000;
    pub const DEBOUNCE_TICK_MILLIS: u64 = 250;
    pub const MIN_DEBOUNCE_WINDOW_MILLIS: u64 = 500;
    pub const MAX_DEBOUNCE_WINDOW_MILLIS: u64 = 8_000;
    pub const KEY_CONFIG_DEBOUNCE_WINDOW_MILLIS: u64 = 900;
    pub const CODE_TEXT_DEBOUNCE_WINDOW_MILLIS: u64 = 1_200;
    pub const LOCKFILE_DEBOUNCE_WINDOW_MILLIS: u64 = 2_500;
    /// Office documents, PDFs, and images — the files ordinary users care
    /// about most. Their atomic-save patterns settle within 1–2s, and the
    /// per-path coalescing map absorbs multi-event bursts, so they need
    /// nowhere near the conservative `Other` window.
    pub const DOCUMENT_DEBOUNCE_WINDOW_MILLIS: u64 = 1_500;
    pub const DEFAULT_DEBOUNCE_WINDOW_MILLIS: u64 = 4_000;
    pub const THROTTLE_SAMPLE_INTERVAL_MILLIS: u64 = 1_000;
    /// Keyboard or pointer input inside this window marks the user as
    /// active, which holds the throttle at `Throttled` so sync never
    /// competes with someone at the keyboard. Long enough that a pause
    /// between keystrokes does not flip the state every second.
    pub const USER_ACTIVE_INPUT_WINDOW_MILLIS: u64 = 30_000;
    /// Accepted values of `VAPOR_THROTTLE_INPUTS`.
    pub const THROTTLE_INPUTS_HOST: &str = "host";
    pub const THROTTLE_INPUTS_STATIC: &str = "static";
    /// `file:<path>`: every sample re-reads a JSON document at the path
    /// (a `ThrottleInputs` object under `inputs`, plus `idle_seconds`),
    /// so a test driver can walk the daemon through every throttle
    /// state. Unreadable or missing files sample as the static defaults.
    pub const THROTTLE_INPUTS_FILE_PREFIX: &str = "file:";
    /// A transfer session that reports progress without moving a byte
    /// this many times in a row is failed as transient, so a misbehaving
    /// endpoint cannot spin a worker at full speed forever.
    pub const MAX_ZERO_PROGRESS_TRANSFER_STEPS: u32 = 8;
    /// How often the daemon stats `vapor.json` for a change. One stat
    /// per second is the cost of settings that apply without a restart.
    pub const CONFIG_RELOAD_POLL_MILLIS: u64 = 1_000;
    pub const STARTUP_RECONSTRUCTION_BARRIER_DEADLINE_MILLIS: u64 = 60_000;
    pub const LEASE_TIMEOUT_MILLIS: u64 = 15 * 60 * 1_000;
    pub const LIGHT_SYSTEM_CPU_PERCENT: u8 = 35;
    pub const THROTTLED_SYSTEM_CPU_PERCENT: u8 = 60;
    pub const SUSPENDED_SYSTEM_CPU_PERCENT: u8 = 85;
    pub const LIGHT_VAPOR_CPU_PERCENT: u8 = 8;
    pub const THROTTLED_VAPOR_CPU_PERCENT: u8 = 15;
    pub const SUSPENDED_VAPOR_CPU_PERCENT: u8 = 25;
    pub const LIGHT_NETWORK_ERROR_RATE_PERCENT: u8 = 10;
    pub const THROTTLED_NETWORK_ERROR_RATE_PERCENT: u8 = 25;
    pub const LIGHT_NETWORK_THROUGHPUT_KBPS: u32 = 512;
    pub const THROTTLED_NETWORK_THROUGHPUT_KBPS: u32 = 128;
    /// IdleDrain concurrency tier bounds. The effective tier derives
    /// from the machine's available parallelism (half the cores),
    /// clamped into `[MIN, MAX]` — a 16-core desktop drains faster than
    /// a 2-core laptop without oversubscribing either. Light/Throttled
    /// tiers stay fixed: they bound device impact while the user is
    /// active, where core count is not the limit that matters.
    pub const IDLE_DRAIN_CONCURRENCY_MIN: usize = 4;
    pub const IDLE_DRAIN_CONCURRENCY_MAX: usize = 8;
    pub const IDLE_DRAIN_READ_TOKENS: usize = 2;
    pub const LIGHT_PLANNER_WORKERS: usize = 2;
    pub const LIGHT_HASH_WORKERS: usize = 2;
    pub const LIGHT_READ_TOKENS: usize = 1;
    pub const LIGHT_UPLOAD_CONCURRENCY: usize = 2;
    pub const LIGHT_DOWNLOAD_CONCURRENCY: usize = 2;
    pub const THROTTLED_PLANNER_WORKERS: usize = 1;
    pub const THROTTLED_HASH_WORKERS: usize = 1;
    pub const THROTTLED_READ_TOKENS: usize = 1;
    pub const THROTTLED_UPLOAD_CONCURRENCY: usize = 1;
    pub const THROTTLED_DOWNLOAD_CONCURRENCY: usize = 1;
    /// Byte budget one hashing execution may consume per runtime tick.
    /// Bounds the per-tick CPU/read cost of the hash stage while still
    /// hashing large files at a useful rate (8 MiB * 4 Hz = 32 MiB/s).
    pub const HASH_STAGE_STEP_BYTES: u64 = 8 * 1024 * 1024;
    /// Byte budget one upload/download transfer step may consume before
    /// re-checking the throttle gates and bandwidth shaper — the
    /// interruptibility granularity of a transfer session. The shaper
    /// lowers the effective budget further when a user bandwidth
    /// ceiling applies.
    pub const TRANSFER_STAGE_STEP_BYTES: u64 = 8 * 1024 * 1024;
    /// Bounds on worker threads running blocking provider I/O (probes,
    /// transfer sessions, remote deletes) off the runtime tick thread.
    /// The effective cap is twice the IdleDrain concurrency tier,
    /// clamped into `[MIN, MAX]`, so upload + download can both run at
    /// full width. Threads spawn lazily per in-flight job and sit
    /// parked on a channel otherwise; the throttle workgate, not this
    /// cap, bounds how much work is admitted.
    pub const PROVIDER_JOB_WORKERS_MIN: usize = 8;
    pub const PROVIDER_JOB_WORKERS_MAX: usize = 16;
    /// Remote changes-feed poll cadence per throttle state; Google
    /// Drive follows the same discipline. Suspended never polls.
    pub const REMOTE_POLL_IDLE_DRAIN_SECONDS: u64 = 5;
    pub const REMOTE_POLL_LIGHT_SECONDS: u64 = 15;
    pub const REMOTE_POLL_THROTTLED_SECONDS: u64 = 60;
    /// Maximum remote changes consumed per poll page.
    pub const REMOTE_CHANGES_PAGE_MAX: usize = 256;
    /// Retry cadence for ensuring the provider-side sync root when the
    /// initial attempt failed. Sync work stays blocked (and
    /// intents accumulate durably) between attempts.
    pub const CLOUD_ROOT_ENSURE_RETRY_SECONDS: u64 = 60;
    /// Directories the reconcile comparison walk processes per runtime
    /// tick while a reconcile slice is active. Bounds per-tick I/O so
    /// the slice checkpoints keep their interruptibility guarantee.
    pub const RECONCILE_DIRS_PER_CHECKPOINT: usize = 8;
    /// Files a directory that appeared (created, renamed in) is
    /// reported for, one synthesized watcher event each, before the
    /// runtime gives up enumerating and leaves a subtree reconcile
    /// marker instead. Keeps the tick-thread walk bounded.
    pub const SYNTHESIZED_SUBTREE_EVENT_CAP: usize = 10_000;
    /// Per-tick directory budget for the reconcile walk (runs only under
    /// IdleDrain). Higher than the checkpoint granularity so a large tree
    /// converges quickly on a fast (filesystem) provider; the per-slice
    /// wall-clock deadline caps the cost when the provider's enumerate is
    /// a slow network call.
    pub const RECONCILE_DIRS_PER_SLICE_IDLE_DRAIN: usize = 64;
    /// Assumed link capacity when the platform sampler reports no
    /// measured throughput; the bandwidth ceiling applies against this
    /// until a real measurement exists.
    pub const ASSUMED_LINK_CAPACITY_KBPS: u32 = 100_000;
    /// Auto-tuning cadence: one small change per cycle within
    /// the documented 60-120s window.
    pub const AUTO_TUNE_INTERVAL_SECONDS: u64 = 90;
    /// Fallback cadence for re-publishing the IPC status snapshot while
    /// idle. Busy ticks, control requests, and per-profile state flips
    /// publish immediately; this heartbeat only bounds the staleness of
    /// anything those triggers miss, so a fully idle daemon runs the
    /// snapshot's per-profile SQL at this cadence instead of every tick.
    pub const STATUS_REPUBLISH_HEARTBEAT_SECONDS: u64 = 10;
    /// Auto-tuned transfer step budget bounds, as multiples of
    /// `TRANSFER_STAGE_STEP_BYTES` expressed in percent (50% .. 200%).
    pub const AUTO_TUNE_MIN_STEP_PERCENT: u64 = 50;
    pub const AUTO_TUNE_MAX_STEP_PERCENT: u64 = 200;
    /// Active-coding heuristic: this many stabilized code-file
    /// events inside the window treat the user as actively working even
    /// when no HID signal is available.
    pub const ACTIVE_CODING_WINDOW_SECONDS: u64 = 60;
    pub const ACTIVE_CODING_EVENT_THRESHOLD: usize = 5;
    /// Mass-change guard: deletions in either direction above this rate,
    /// or above the ratio of the synced tree, are held behind a
    /// decision instead of propagating what may be ransomware, an
    /// accidental recursive delete, or a bad listing from the cloud.
    pub const MASS_DELETE_WINDOW_SECONDS: u64 = 60;
    pub const MASS_DELETE_THRESHOLD: usize = 1_000;
    pub const MASS_DELETE_RATIO_PERCENT: u8 = 25;
    /// FlushNow boost window: after an explicit flush request
    /// the runtime releases deferred work eagerly for this long.
    pub const FLUSH_BOOST_SECONDS: u64 = 30;
    pub const RETRY_BASE_DELAY_MILLIS: u64 = 2_000;
    pub const RETRY_RATE_LIMIT_BASE_DELAY_MILLIS: u64 = 15_000;
    pub const RETRY_MAX_DELAY_MILLIS: u64 = 900_000;
    /// Ceiling for a server-supplied `Retry-After`: a bogus or absurd
    /// header (garbage seconds, a mistaken epoch timestamp) is clamped to
    /// this so it cannot overflow time arithmetic or park an intent — and
    /// the persisted global rate-limit slowdown — for months. One hour is
    /// well past any legitimate provider backoff.
    pub const RETRY_AFTER_CEILING_MILLIS: u64 = 3_600_000;
    pub const RETRY_JITTER_PERCENT: u8 = 20;
    pub const STORM_WINDOW_MILLIS: u64 = 2_000;
    pub const STORM_DIRECTORY_UNIQUE_PATHS_THRESHOLD: usize = 200;
    pub const STORM_DIRECTORY_EVENT_COUNT_THRESHOLD: usize = 600;
    pub const STORM_GLOBAL_PENDING_EVENT_COUNT_THRESHOLD: usize = 5_000;
    pub const DEFERRED_RECONCILE_DELAY_MILLIS: u64 = 30_000;
    /// Early-release path for a storm-deferred reconcile: once the
    /// device is already idle (`IdleDrain`) and the storm has been
    /// quiet this long, waiting out the full deferral only delays
    /// convergence the user is watching for (a repo clone appearing in
    /// the cloud folder). The full delay still applies while the user
    /// stays active.
    pub const DEFERRED_RECONCILE_IDLE_QUIET_RELEASE_MILLIS: u64 = 5_000;
    /// Upper bound on how long a storm-deferred reconcile may keep being
    /// pushed back by continued churn. Measured from the moment the storm
    /// was first detected; once reached, the reconcile becomes releasable
    /// even if the subtree never goes quiet, so a permanently-busy subtree
    /// cannot starve its own convergence forever.
    pub const DEFERRED_RECONCILE_MAX_DELAY_MILLIS: u64 = 600_000;
    /// Requeue delay for durable intents that could not start because a
    /// workgate permit or reconcile slot was unavailable. Deliberately
    /// coarser than the tick interval so blocked intents do not churn the
    /// durable queue with a lease + requeue write pair on every tick.
    pub const BLOCKED_INTENT_REQUEUE_DELAY_MILLIS: u64 = 1_000;
    /// Cadence of the in-run stale-lease recovery sweep. Complements the
    /// startup `recover_leased` pass so a lease orphaned mid-run (a bug or
    /// a lost execution) is replayed without waiting for a restart.
    pub const STALE_LEASE_SWEEP_INTERVAL_MILLIS: u64 = 60_000;
    /// Tick interval used when the previous tick found no work anywhere:
    /// no pending events, no in-flight executions, nothing stabilizing.
    /// Bounded by the throttle sample interval so state decisions stay
    /// fresh; fs events and IPC requests wake the loop immediately via the
    /// tick waker, so a fully idle daemon polls at 1 Hz instead of 4 Hz.
    pub const IDLE_TICK_MILLIS: u64 = 1_000;
    pub const RECONCILE_SLICE_MILLIS: u64 = 500;
    /// Minimum dwell time before the throttle controller may down-shift to
    /// or out of `Light`. Pairs with `MIN_DWELL_THROTTLED_SECONDS` to keep
    /// state stable under oscillating CPU / network samples.
    pub const MIN_DWELL_LIGHT_SECONDS: u64 = 5;
    /// Minimum dwell time before the throttle controller may down-shift to
    /// or out of `Throttled`. See `MIN_DWELL_LIGHT_SECONDS`.
    pub const MIN_DWELL_THROTTLED_SECONDS: u64 = 5;
    /// Minimum dwell time before the throttle controller may exit
    /// `Suspended`. Shorter than the lower-pressure tiers because
    /// `Suspended` is the safest state — staying suspended for an extra
    /// second is never harmful, but exiting too aggressively could mask
    /// real pressure.
    pub const MIN_DWELL_SUSPENDED_SECONDS: u64 = 1;
}
