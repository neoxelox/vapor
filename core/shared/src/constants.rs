/// Names of the variables a build also reads at compile time.
/// `option_env!` accepts only a literal or a macro that expands to one,
/// so these macros hold the literal and `env::*` expands them.
#[macro_export]
macro_rules! gdrive_client_id_env {
    () => {
        "VAPOR_GDRIVE_CLIENT_ID"
    };
}

#[macro_export]
macro_rules! gdrive_client_secret_env {
    () => {
        "VAPOR_GDRIVE_CLIENT_SECRET"
    };
}

pub mod env {
    pub const VAPOR_DIR: &str = "VAPOR_DIR";
    pub const VAPOR_ENV: &str = "VAPOR_ENV";
    pub const VAPOR_LOG_LEVEL: &str = "VAPOR_LOG_LEVEL";
    /// OAuth client credentials for the Google Drive provider
    /// (installed-app PKCE). Read twice: at compile time, so a build
    /// carries the client it was built with, and at run time, where a
    /// set value overrides the built-in client. See
    /// `docs/operations/provider-auth-operations.md`.
    pub const VAPOR_GDRIVE_CLIENT_ID: &str = crate::gdrive_client_id_env!();
    pub const VAPOR_GDRIVE_CLIENT_SECRET: &str = crate::gdrive_client_secret_env!();
    /// Where the daemon's throttle inputs come from: `host` (default)
    /// reads the native sampler and idle clock; `static` uses the
    /// neutral defaults and zero idle time; `file:<path>` re-reads a
    /// JSON document every sample so a driver can script the inputs.
    /// Test harnesses set `static` so a run is not shaped by whoever is
    /// typing on the machine; the soak driver uses `file:`.
    pub const VAPOR_THROTTLE_INPUTS: &str = "VAPOR_THROTTLE_INPUTS";
    /// An external secret manager on Linux (`pass`, a vault CLI, an
    /// `age` wrapper): the program the secret store shells out to with
    /// the verbs in `secrets::COMMAND_VERB_*`. Takes precedence over the
    /// desktop Secret Service when set.
    pub const VAPOR_SECRETS_COMMAND: &str = "VAPOR_SECRETS_COMMAND";
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
    /// Under the state directory: one directory per named profile.
    pub const PROFILES_DIRECTORY_NAME: &str = "profiles";
    /// The managed trash: `vapor_dir/trash/<profile>/<entry>/`, one
    /// directory per discarded item holding the payload under its
    /// original name and a `meta.json` next to it. A payload that has
    /// that name itself is kept as `<name>~payload`.
    pub const TRASH_DIRECTORY_NAME: &str = "trash";
    /// Where `vapor report` writes by default: `vapor_dir/reports/`.
    pub const REPORTS_DIRECTORY_NAME: &str = "reports";
    pub const TRASH_ENTRY_META_FILE_NAME: &str = "meta.json";
    pub const CONFIGURATION_FILE_NAME: &str = "vapor.json";
    pub const SQLITE_DATABASE_FILE_NAME: &str = "vapor.sqlite";
    pub const APP_LOG_FILE_NAME: &str = "vapor.log";
    pub const DAEMON_LOG_FILE_NAME: &str = "vapord.log";
    /// launchd / service-manager stdout & stderr redirection targets for
    /// the daemon, under `vapor_dir/logs/`. Referenced by every surface
    /// that writes a service definition (macOS app, `vapor` CLI) per
    /// `docs/operations/macos/launchagent-policy.md`.
    pub const DAEMON_STDOUT_LOG_FILE_NAME: &str = "vapord.stdout.log";
    pub const DAEMON_STDERR_LOG_FILE_NAME: &str = "vapord.stderr.log";
    /// The headless supervisor's output under the service manager.
    pub const SUPERVISOR_LOG_FILE_NAME: &str = "vapor-supervisor.log";
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
    /// A queued intent that failed this many times in a row is blocked,
    /// not queued work: something about the file stops it (it cannot be
    /// read, its target is locked), the tries are minutes apart, and
    /// the user has to be told. `vapor status` counts it apart.
    pub const BLOCKED_AFTER_ATTEMPTS: u32 = 5;
    /// Blocked files `vapor status` names with their reason.
    pub const STATUS_BLOCKED_FILES_MAX: usize = 10;
    /// A new event for a path makes a retry of that path due at once,
    /// but only when the last try is at least this old: an event that
    /// follows a failed try by seconds is most likely made by the try.
    pub const RETRY_WAKE_GRACE_MILLIS: u64 = 10_000;
    /// The root identities a profile adopted, checked at every start
    /// and on a cadence so a replaced root is never mirrored blindly.
    pub const LOCAL_ROOT_IDENTITY_KEY: &str = "root_identity.local";
    pub const CLOUD_ROOT_IDENTITY_KEY: &str = "root_identity.cloud";
    /// The one-way mode and the pair of roots the user acknowledged.
    pub const ONE_WAY_ACKNOWLEDGED_KEY: &str = "one_way.acknowledged";
    /// Set by a `reattach` answer: the next whole-scope reconcile merges
    /// the two roots without propagating any deletion.
    pub const MERGE_WITHOUT_DELETIONS_KEY: &str = "reconcile.merge_without_deletions";
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
    pub const KEY_USE_GITIGNORE: &str = "useGitignore";
    pub const KEY_USE_VAPORIGNORE: &str = "useVaporignore";
    /// The local sync root: the folder on this device that Vapor keeps
    /// in step with `cloudRoot`.
    pub const KEY_LOCAL_ROOT: &str = "localRoot";
    /// The cloud sync root: the folder on the provider side (a local
    /// directory when the provider is `filesystem`).
    pub const KEY_CLOUD_ROOT: &str = "cloudRoot";
    pub const KEY_PRE_IGNORE_RULES: &str = "preIgnoreRules";
    pub const KEY_POST_IGNORE_RULES: &str = "postIgnoreRules";
    pub const KEY_LANGUAGE: &str = "language";
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
    /// Trash group; object with the `trash::KEY_*` fields.
    pub const KEY_TRASH: &str = "trash";

    /// Keys a running daemon applies within one poll interval of the
    /// file changing, without a restart. The CLI tells the user which
    /// class a key falls in after `config set`.
    pub const LIVE_RELOAD_KEYS: &[&str] = &[
        KEY_USE_GITIGNORE,
        KEY_USE_VAPORIGNORE,
        KEY_PRE_IGNORE_RULES,
        KEY_POST_IGNORE_RULES,
        KEY_TIMELINE_LIMIT,
        KEY_RESOURCE_LIMITS,
        KEY_IDLE_BOOST,
        KEY_SAFEGUARDS,
        KEY_TRASH,
    ];

    /// Keys that reshape the pipeline of a profile that runs (roots,
    /// provider, direction, its entry in the profile set) and therefore
    /// take effect on the next daemon start; a running daemon reports
    /// them as `restartRequired` in status until it is restarted. A
    /// change of these keys that only adds, removes, enables, disables,
    /// or renames a profile applies at once.
    pub const RESTART_REQUIRED_KEYS: &[&str] = &[
        KEY_LOCAL_ROOT,
        KEY_CLOUD_ROOT,
        KEY_PROVIDER,
        KEY_SYNC_MODE,
        KEY_PROFILES,
        KEY_DEVICE_ID,
    ];

    /// Every recognized key in one slice. Kept in lockstep with the
    /// `KEY_*` constants above; the CLI uses this for `validate_key`.
    pub const ALL_KEYS: &[&str] = &[
        KEY_AUTO_LAUNCH,
        KEY_USE_GITIGNORE,
        KEY_USE_VAPORIGNORE,
        KEY_LOCAL_ROOT,
        KEY_CLOUD_ROOT,
        KEY_PRE_IGNORE_RULES,
        KEY_POST_IGNORE_RULES,
        KEY_LANGUAGE,
        KEY_TIMELINE_LIMIT,
        KEY_PROVIDER,
        KEY_SYNC_MODE,
        KEY_DEVICE_ID,
        KEY_PROFILES,
        KEY_RESOURCE_LIMITS,
        KEY_IDLE_BOOST,
        KEY_SAFEGUARDS,
        KEY_TRASH,
    ];

    /// Default values for the config keys whose defaults are not already
    /// hosted by another constants module (`filtering::*` owns the
    /// directory + ignore-rule defaults). Mirrored by the Swift
    /// `VaporConstants.Defaults` per AGENTS.md §8.6: the Rust side is
    /// the source of truth.
    pub const DEFAULT_AUTO_LAUNCH: bool = true;
    pub const DEFAULT_USE_GITIGNORE: bool = true;
    pub const DEFAULT_USE_VAPORIGNORE: bool = true;
    pub const DEFAULT_LANGUAGE: &str = "en";
    pub const DEFAULT_TIMELINE_LIMIT: i64 = 1_000;
    /// The most timeline entries the daemon keeps. The whole timeline
    /// answers `vapor timeline` in one IPC frame, and a frame has a
    /// size limit: a larger buffer could not be read back.
    pub const MAX_TIMELINE_LIMIT: i64 = 10_000;
}

pub mod provider {
    /// Bytes one transfer step moves unless the provider chooses its
    /// own chunk size. The engine checks the throttle gates and the
    /// bandwidth bucket between two steps.
    pub const TRANSFER_STEP_BYTES: u64 = 8 * 1024 * 1024;
    /// Accepted `provider` config values. `filesystem` is the pre-GA
    /// default; when selected, `cloudRoot` is
    /// reinterpreted as an absolute local directory that plays the role
    /// of the cloud side. `gdrive` selects the real cloud provider.
    pub const FILESYSTEM: &str = "filesystem";
    pub const GDRIVE: &str = "gdrive";
    pub const DEFAULT: &str = FILESYSTEM;
    pub const ALL: &[&str] = &[FILESYSTEM, GDRIVE];
    /// Providers whose profiles sign in (`vapor auth login`); the rest
    /// need no credentials.
    pub const SIGN_IN_REQUIRED: &[&str] = &[GDRIVE];

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
    /// The root identity marker written once into a sync root when a
    /// profile adopts it (`docs/architecture/data-flow.md` §Root
    /// identity). Hidden from enumeration, feeds, and local ingest
    /// like the other internal names; never synced.
    pub const ROOT_MARKER_FILE_NAME: &str = ".vapor-root";
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
    /// Per-socket read/write timeout for the native HTTP transport. A
    /// black-holed connection (Wi-Fi switch, dropped NAT flow) must not
    /// hold a provider worker for ever: a stalled read surfaces as a
    /// transient error the retry machinery handles.
    pub const HTTP_SOCKET_TIMEOUT_SECONDS: u64 = 120;
    /// Connect timeout for the native HTTP transport.
    pub const HTTP_CONNECT_TIMEOUT_SECONDS: u64 = 30;
    /// How long the HTTP transport goes on with the proxy it last read
    /// before it reads the proxy settings again. A device that moves to
    /// another network follows within this time.
    pub const HTTP_PROXY_RECHECK_SECONDS: u64 = 30;
    /// Hard cap on a single HTTP response body. Reading one extra byte
    /// past this and erroring (rather than silently truncating) keeps a
    /// mis-ranged full-file download from completing as a corrupt file.
    pub const MAX_HTTP_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
    /// How long `vapor auth login` waits for the browser consent before
    /// giving up: long enough for a person who was called away from
    /// the consent page. The app's Sign in button waits a little longer
    /// than this for the CLI to answer.
    pub const BROWSER_SIGN_IN_TIMEOUT_SECONDS: u64 = 30 * 60;
    /// How long the address of a sign-in that timed out still answers
    /// the browser, with a page that says to sign in again. The consent
    /// page of the provider stays open in the browser and looks alive
    /// long after nobody waits for it.
    pub const EXPIRED_SIGN_IN_PAGE_SECONDS: u64 = 24 * 60 * 60;
}

pub mod profile {
    /// Keys of each object in the top-level `profiles` array.
    /// A profile inherits any unset override-capable field from the
    /// top-level configuration.
    pub const KEY_ID: &str = "id";
    pub const KEY_NAME: &str = "name";
    pub const KEY_PROVIDER: &str = "provider";
    pub const KEY_LOCAL_ROOT: &str = "localRoot";
    pub const KEY_CLOUD_ROOT: &str = "cloudRoot";
    pub const KEY_SYNC_MODE: &str = "syncMode";
    pub const KEY_ENABLED: &str = "enabled";
    /// Partial overrides of the top-level groups of the same name.
    pub const KEY_RESOURCE_LIMITS: &str = super::config::KEY_RESOURCE_LIMITS;
    pub const KEY_IDLE_BOOST: &str = super::config::KEY_IDLE_BOOST;
    pub const ALL_KEYS: &[&str] = &[
        KEY_ID,
        KEY_NAME,
        KEY_PROVIDER,
        KEY_LOCAL_ROOT,
        KEY_CLOUD_ROOT,
        KEY_SYNC_MODE,
        KEY_ENABLED,
        KEY_RESOURCE_LIMITS,
        KEY_IDLE_BOOST,
    ];
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
    pub const KEY_CONCURRENT_TRANSFERS: &str = "concurrentTransfers";
    pub const ALL_KEYS: &[&str] = &[
        KEY_CPU_PERCENT,
        KEY_MEMORY_PERCENT,
        KEY_BANDWIDTH_PERCENT,
        KEY_CONCURRENT_TRANSFERS,
    ];
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
    /// headroom expansion while the device is verifiably idle. The
    /// three percentages are the raised ceilings that replace the
    /// `resourceLimits` ones while boost is active.
    pub const KEY_ENABLED: &str = "enabled";
    pub const KEY_MIN_IDLE_SECONDS: &str = "minIdleSeconds";
    pub const KEY_CPU_PERCENT: &str = "cpuPercent";
    pub const KEY_MEMORY_PERCENT: &str = "memoryPercent";
    pub const KEY_BANDWIDTH_PERCENT: &str = "bandwidthPercent";
    pub const KEY_HEADROOM_CPU_PERCENT: &str = "headroomCpuPercent";
    pub const KEY_RAMP_UP_SECONDS: &str = "rampUpSeconds";
    pub const KEY_RAMP_DOWN_SECONDS: &str = "rampDownSeconds";
    pub const ALL_KEYS: &[&str] = &[
        KEY_ENABLED,
        KEY_MIN_IDLE_SECONDS,
        KEY_CPU_PERCENT,
        KEY_MEMORY_PERCENT,
        KEY_BANDWIDTH_PERCENT,
        KEY_HEADROOM_CPU_PERCENT,
        KEY_RAMP_UP_SECONDS,
        KEY_RAMP_DOWN_SECONDS,
    ];
    pub const DEFAULT_ENABLED: bool = true;
    pub const DEFAULT_MIN_IDLE_SECONDS: u64 = 300;
    pub const DEFAULT_CPU_PERCENT: u8 = 50;
    pub const DEFAULT_MEMORY_PERCENT: u8 = 20;
    pub const DEFAULT_BANDWIDTH_PERCENT: u8 = 80;
    /// Non-Vapor utilization must stay at or below this for boost to
    /// engage (per-resource headroom gate).
    pub const DEFAULT_HEADROOM_CPU_PERCENT: u8 = 30;
    pub const DEFAULT_RAMP_UP_SECONDS: u64 = 30;
    /// Must stay <= ramp-up so activity resumption is non-invasive
    /// (`data-flow.md §User resource budgets`).
    pub const DEFAULT_RAMP_DOWN_SECONDS: u64 = 10;
}

pub mod safeguards {
    /// Keys of the `safeguards` config group. Each safeguard is its own
    /// object inside the group.
    pub const KEY_MASS_DELETE: &str = "massDelete";
    pub const ALL_KEYS: &[&str] = &[KEY_MASS_DELETE];

    /// Keys of `safeguards.massDelete`. The guard holds a burst of
    /// deletes (in either direction) behind a decision when the burst
    /// reaches the threshold or the ratio inside a rolling window; the
    /// rest of the sync keeps flowing. The ransomware / bulk-mistake
    /// backstop, configurable because a workflow that legitimately
    /// unlinks many files (`rm -rf` of large trees, big build cleans)
    /// may need a higher threshold.
    pub const KEY_ENABLED: &str = "enabled";
    /// Deletes inside the window that trip the guard.
    pub const KEY_THRESHOLD: &str = "threshold";
    pub const KEY_WINDOW_SECONDS: &str = "windowSeconds";
    /// Share of the synced files (percent) whose deletion inside the
    /// window is held for a decision even when the threshold is not
    /// reached, so a small tree is protected too.
    pub const KEY_RATIO_PERCENT: &str = "ratioPercent";
    pub const MASS_DELETE_KEYS: &[&str] = &[
        KEY_ENABLED,
        KEY_THRESHOLD,
        KEY_WINDOW_SECONDS,
        KEY_RATIO_PERCENT,
    ];
    pub const DEFAULT_MASS_DELETE_ENABLED: bool = true;
    pub const DEFAULT_MASS_DELETE_THRESHOLD: usize = 1_000;
    pub const DEFAULT_MASS_DELETE_WINDOW_SECONDS: u64 = 60;
    pub const DEFAULT_MASS_DELETE_RATIO_PERCENT: u8 = 25;
    /// Floor clamps: a threshold/window too low would trip the guard on
    /// ordinary work and train users to blind-approve it.
    pub const MIN_MASS_DELETE_THRESHOLD: usize = 10;
    pub const MIN_MASS_DELETE_WINDOW_SECONDS: u64 = 5;
    /// The ratio rule never holds fewer deletes than this, so a tree of
    /// three files does not prompt on every second delete.
    pub const MIN_MASS_DELETE_RATIO_COUNT: usize = 10;
}

pub mod trash {
    /// Keys of the `trash` config group. Every file Vapor itself
    /// removes on this device (a delete that arrived from the cloud,
    /// a mirror removal in `pull-only`) goes to a trash instead of
    /// being unlinked, so a wrong deletion can be undone here without
    /// the cloud's own trash.
    pub const KEY_ENABLED: &str = "enabled";
    /// Days a managed-trash entry is kept before the daemon purges it.
    pub const KEY_RETENTION_DAYS: &str = "retentionDays";
    /// Try the user's own trash (the Finder's Trash on macOS) first and
    /// fall back to the managed trash when that fails.
    pub const KEY_USE_SYSTEM: &str = "useSystem";
    pub const ALL_KEYS: &[&str] = &[KEY_ENABLED, KEY_RETENTION_DAYS, KEY_USE_SYSTEM];
    pub const DEFAULT_ENABLED: bool = true;
    pub const DEFAULT_RETENTION_DAYS: u32 = 7;
    pub const DEFAULT_USE_SYSTEM: bool = false;
    /// How often a running daemon looks for expired entries.
    pub const PURGE_INTERVAL_SECONDS: u64 = 1_800;
    /// An entry with its `meta.json` and no payload is what a discard
    /// leaves when it is cut off between its two steps. It is removed
    /// once it is this old, so a discard in flight is never taken for
    /// one.
    pub const ENTRY_WITHOUT_PAYLOAD_SECONDS: u64 = 3_600;
    /// Why an item is in the trash; recorded in `meta.json` and shown by
    /// `vapor trash list`. A `*-delete` name says where the delete came
    /// from: a delete made in the cloud, or a mirror removing what its
    /// source does not have.
    pub const REASON_CLOUD_DELETE: &str = "cloud-delete";
    pub const REASON_MIRROR_DELETE: &str = "mirror-delete";
    /// A version of a conflicted file the user did not keep.
    pub const REASON_CONFLICT: &str = "conflict";
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

/// The names every user-facing surface prints: values in `--json`
/// output, in the IPC status, in the timeline, and in the options a
/// command accepts. Every value is kebab-case and every JSON key is
/// camelCase, on every surface; the Swift mirror in
/// `apps/macos/Sources/VaporCore/VaporConstants.swift` repeats the ones
/// the app reads.
pub mod run_state {
    pub const STARTING: &str = "starting";
    pub const RUNNING: &str = "running";
    pub const PAUSED: &str = "paused";
    pub const ERROR: &str = "error";
    /// No sync profile exists yet: the daemon is up and syncs nothing.
    pub const UNCONFIGURED: &str = "unconfigured";
}

pub mod throttle_state {
    /// The device is idle: queued work drains at full width.
    pub const IDLE: &str = "idle";
    /// Some load, battery power, or mild heat: reduced width.
    pub const LIGHT: &str = "light";
    /// Heavy load, a user at the keyboard, serious heat, Low Power
    /// Mode, or an almost full disk: the narrowest width that still
    /// syncs. Scans run.
    pub const THROTTLED: &str = "throttled";
    /// Critical heat: transfers and hashing stop; changes are still
    /// recorded.
    pub const SUSPENDED: &str = "suspended";
}

/// What the whole-scope reconcile is doing (`reconcile.state` in status).
pub mod reconcile_state {
    pub const IDLE: &str = "idle";
    pub const WAITING: &str = "waiting";
    pub const RUNNING: &str = "running";
}

/// Where idle boost is (`idleBoostState`).
pub mod idle_boost_state {
    pub const OFF: &str = "off";
    pub const RAMPING_UP: &str = "ramping-up";
    pub const ACTIVE: &str = "active";
    pub const RAMPING_DOWN: &str = "ramping-down";
}

/// The work items `vapor diagnostics` lists: what each one does
/// (`action`) and where it is (`stage`). A `*-delete` action names the
/// side the delete was made on: `local-delete` carries a delete made on
/// this device to the cloud, `cloud-delete` applies a delete made in the
/// cloud to this device.
pub mod intent {
    pub const ACTION_UPLOAD: &str = "upload";
    pub const ACTION_DOWNLOAD: &str = "download";
    pub const ACTION_RENAME: &str = "rename";
    pub const ACTION_LOCAL_DELETE: &str = "local-delete";
    pub const ACTION_CLOUD_DELETE: &str = "cloud-delete";
    pub const ACTION_RECONCILE: &str = "reconcile";

    pub const STAGE_QUEUED: &str = "queued";
    pub const STAGE_RETRYING: &str = "retrying";
    /// Held behind an open decision.
    pub const STAGE_HELD: &str = "held";
    pub const STAGE_PLANNING: &str = "planning";
    pub const STAGE_WAITING_TO_HASH: &str = "waiting-to-hash";
    pub const STAGE_HASHING: &str = "hashing";
    pub const STAGE_WAITING_TO_UPLOAD: &str = "waiting-to-upload";
    pub const STAGE_UPLOADING: &str = "uploading";
    pub const STAGE_WAITING_TO_DOWNLOAD: &str = "waiting-to-download";
    pub const STAGE_DOWNLOADING: &str = "downloading";
}

/// Decisions: the questions the daemon parks (`vapor decisions`). Each
/// kind lists the option keys `vapor decisions resolve --choose`
/// accepts for it.
pub mod decision {
    /// A burst of deletes reached the `safeguards.massDelete` limits.
    pub const KIND_MASS_DELETE: &str = "mass-delete";
    /// A sync root is gone.
    pub const KIND_ROOT_MISSING: &str = "root-missing";
    /// A sync root is present but is not the folder the profile adopted.
    pub const KIND_ROOT_REPLACED: &str = "root-replaced";
    /// A file on one side is a folder on the other.
    pub const KIND_TYPE_MISMATCH: &str = "type-mismatch";
    /// A local name that cannot be a sync path (not valid UTF-8, or
    /// with a backslash in it).
    pub const KIND_UNSYNCABLE_NAME: &str = "unsyncable-name";
    /// A file changed in more than one place before the changes synced;
    /// Vapor kept every version, and one has to stay.
    pub const KIND_CONFLICT: &str = "conflict";
    /// A profile is set to a one-way mode, which overwrites and removes
    /// on one side; it holds until the user says yes to that.
    pub const KIND_ONE_WAY_MODE: &str = "one-way-mode";

    /// An answer that the daemon could not apply is tried again after
    /// these waits; the last one repeats. The cause is outside the
    /// daemon (a file it cannot move, a provider it cannot reach), so a
    /// try on every tick only fills the log.
    pub const APPLY_RETRY_SECONDS: [u64; 3] = [5, 60, 10 * 60];

    /// `mass-delete`: let the deletes through, or put the files back.
    pub const OPTION_APPLY: &str = "apply";
    pub const OPTION_DISCARD: &str = "discard";
    /// `root-missing`: create the root again, empty, and let the other
    /// side fill it.
    pub const OPTION_RECREATE: &str = "recreate";
    /// `root-replaced`: adopt the folder that is there now and merge it
    /// without propagating any delete.
    pub const OPTION_REATTACH: &str = "reattach";
    /// `type-mismatch`: keep both under different names, or let one side
    /// win.
    pub const OPTION_KEEP_BOTH: &str = "keep-both";
    pub const OPTION_PREFER_LOCAL: &str = "prefer-local";
    pub const OPTION_PREFER_CLOUD: &str = "prefer-cloud";
    /// `unsyncable-name`: leave the file out of sync.
    pub const OPTION_SKIP: &str = "skip";
    /// `one-way-mode`: run the mirror for this mode and these two roots.
    pub const OPTION_ENABLE: &str = "enable";
    // A `conflict` has one option per version, keyed by the version's
    // file name, so it has no fixed option keys.

    /// What a decision covers.
    pub const SCOPE_PATH: &str = "path";
    pub const SCOPE_BATCH: &str = "batch";
    pub const SCOPE_PROFILE: &str = "profile";
}

/// Keep-both conflicts. The original is the file at the path both sides
/// edited; the copy is the other version, saved next to it as
/// `{stem}~conflict-{deviceId}-{ms}{ext}` and holding the edit of the
/// device it names. Each copy is a `conflict` decision.
pub mod conflict {
    pub const COPY_MARKER: &str = "~conflict-";
    /// Stands where the device id goes in the name of a copy that no
    /// device chose to make: the version an upload replaced without
    /// having seen it, because two uploads of one file landed in the
    /// same moment. A device id always has a `-` in it, so this is
    /// never one.
    pub const REPLACED_VERSION: &str = "replaced";
    /// How often a running daemon matches the open `conflict` decisions
    /// to the copies on disk: a file with copies gets a question, one
    /// whose versions changed gets the question rewritten, and one with
    /// no copy left has its question closed.
    pub const DECISION_SYNC_SECONDS: u64 = 5;
    /// `vapor decisions resolve` applies a conflict answer itself, in
    /// milliseconds; one still answered but unapplied after this long
    /// belongs to a command that died, and the daemon opens the
    /// question again rather than touching the files.
    pub const ANSWER_GRACE_SECONDS: u64 = 60;
}

/// Kinds of the entries in the activity timeline (`vapor timeline`).
pub mod timeline {
    pub const KIND_RUN_STATE: &str = "run-state";
    pub const KIND_THROTTLE: &str = "throttle";
    pub const KIND_PROFILE: &str = "profile";
    pub const KIND_CONFIG: &str = "config";
    pub const KIND_DECISION: &str = "decision";
    pub const KIND_CONFLICT: &str = "conflict";
    /// A download discarded because its name would collide with a
    /// differently-cased local file.
    pub const KIND_COLLISION: &str = "collision";
    pub const KIND_MIRROR: &str = "mirror";
    /// Vapor removed a file or a folder from the cloud.
    pub const KIND_CLOUD_DELETE: &str = "cloud-delete";
    /// Work that failed for good (retries exhausted or a permanent error).
    pub const KIND_FAILURE: &str = "failure";
    pub const KIND_RECONCILE: &str = "reconcile";
    /// The cloud changes feed.
    pub const KIND_FEED: &str = "feed";
    pub const KIND_CLOUD_ROOT: &str = "cloud-root";
    /// A profile's provider refused the sign-in, or a new sign-in ended
    /// the hold.
    pub const KIND_SIGN_IN: &str = "sign-in";
    /// The cloud has no room for an upload; uploads wait.
    pub const KIND_CLOUD_FULL: &str = "cloud-full";
}

/// `vapor init`: the steps of a first run, and the state of each.
pub mod init {
    pub const STEP_PROFILE: &str = "profile";
    pub const STEP_SIGN_IN: &str = "sign-in";
    pub const STEP_SERVICE: &str = "service";

    pub const STATE_DONE: &str = "done";
    /// The provider has no account, so there is nothing to sign in to.
    pub const STATE_NOT_NEEDED: &str = "not-needed";
    /// A person has to act: sign in, or answer a question.
    pub const STATE_WAITS: &str = "waits";
    pub const STATE_FAILED: &str = "failed";
    /// The user chose not to start Vapor at login.
    pub const STATE_OFF: &str = "off";
    /// The platform has no service manager that Vapor drives yet.
    pub const STATE_UNAVAILABLE: &str = "unavailable";

    /// The states that leave nothing for `vapor init` to do.
    pub const SETTLED_STATES: &[&str] =
        &[STATE_DONE, STATE_NOT_NEEDED, STATE_OFF, STATE_UNAVAILABLE];
}

/// A profile's sign-in state (`vapor auth status`).
pub mod auth {
    pub const STATE_SIGNED_IN: &str = "signed-in";
    pub const STATE_SIGNED_OUT: &str = "signed-out";
    /// The profile's provider has nothing to sign in to.
    pub const STATE_NOT_NEEDED: &str = "not-needed";
}

/// `vapor doctor` check names and outcomes.
/// What to tell a user whose sync folder the OS will not let Vapor
/// read. macOS asks for consent before a background process reads
/// Documents, Desktop, Downloads, iCloud Drive, or a removable or
/// network volume; a refusal reads as "permission denied" on every
/// file under the folder.
pub const FOLDER_ACCESS_HINT: &str = "on macOS, allow Vapor to read the folder in System Settings > \
     Privacy & Security > Files and Folders (or Full Disk Access)";

pub mod doctor {
    pub const CHECK_VAPOR_DIR: &str = "vapor-dir";
    /// The profile set as a whole: none yet, or what stops the file
    /// from giving one.
    pub const CHECK_PROFILES: &str = "profiles";
    /// One check for each profile, named `profile:<id>`.
    pub const CHECK_PROFILE_PREFIX: &str = "profile:";
    pub const CHECK_IPC_SOCKET: &str = "ipc-socket";
    pub const CHECK_VAPORD_BINARY: &str = "vapord-binary";
    pub const CHECK_SECRET_STORE: &str = "secret-store";
    pub const CHECK_THROTTLE_INPUTS: &str = "throttle-inputs";
    pub const CHECK_LAUNCH_AGENT: &str = "launch-agent";
    pub const CHECK_SYSTEMD_UNIT: &str = "systemd-unit";
    /// Which Google Drive OAuth client a sign-in would use: built in,
    /// from the environment, or none.
    pub const CHECK_GDRIVE_OAUTH_CLIENT: &str = "gdrive-oauth-client";
    pub const STATUS_OK: &str = "ok";
    pub const STATUS_WARNING: &str = "warning";
    pub const STATUS_FAILURE: &str = "failure";
}

pub mod service {
    /// Reverse-DNS identifier used by every Vapor surface that talks to
    /// the OS service manager (macOS LaunchAgent, Linux systemd unit,
    /// Windows Task Scheduler task). Mirrored by the Swift constants
    /// file under `apps/macos/Sources/VaporCore/VaporConstants.swift`
    /// per AGENTS.md §8.6.
    pub const DAEMON_LABEL: &str = "sh.arn.vapor.daemon";
    /// Cadence at which the supervisor runs `vapor service check` to
    /// detect unexpected daemon exits and route them through the
    /// crash-loop guard, and at which an app reads the result. Mirrored
    /// by the Swift constants file per AGENTS.md §8.6.
    pub const HEALTH_TICK_INTERVAL_SECONDS: u64 = 30;
    /// How often the supervisor asks, between two ticks, whether the
    /// daemon still answers on its socket. An answer that stops is the
    /// sign of an exit, and the tick runs at once: a crashed daemon is
    /// back in about a second, not at the next tick.
    pub const SUPERVISOR_WATCH_MILLIS: u64 = 500;
    /// A daemon whose process is there and which gives no answer on its
    /// socket for this long is ended by the supervisor, and started
    /// again under the crash-loop guard: it holds its job and syncs
    /// nothing. Long enough for a daemon that starts, and for one that
    /// stops.
    pub const UNRESPONSIVE_KILL_SECONDS: u64 = 30;
    /// The same for a daemon that has not answered since it started:
    /// a start may take long (a large state database, a volume that
    /// wakes up), and it is not a hang. After one such daemon was
    /// ended, the next is left alone until it answers.
    pub const UNRESPONSIVE_START_SECONDS: u64 = 300;
    /// How long a daemon that was told to stop waits for its worker
    /// threads. A worker inside a call that does not return (a read of
    /// a mount that hangs) must not keep the process alive: the state
    /// is durable, so the process ends without it.
    pub const SHUTDOWN_JOIN_LIMIT_SECONDS: u64 = 5;
    /// The crash-loop policy. Crashes inside the window count together.
    /// The first restarts at once; from the second on, the restart
    /// waits a delay that doubles from the base up to the maximum; the
    /// fifth pauses restarts until the user acknowledges.
    pub const CRASH_LOOP_WINDOW_SECONDS: u64 = 600;
    pub const CRASH_LOOP_BASE_DELAY_SECONDS: u64 = 2;
    pub const CRASH_LOOP_MAX_DELAY_SECONDS: u64 = 120;
    pub const CRASH_LOOP_FREE_RESTARTS: u32 = 1;
    pub const CRASH_LOOP_PAUSE_AFTER_CRASHES: u32 = 5;
    /// How long after a stop request a daemon that still runs counts as
    /// "on its way out". A worker inside a network call can hold the
    /// exit for as long as its socket timeout.
    pub const STOP_GRACE_SECONDS: u64 = 180;
    /// The supervisor: a second service definition, registered by
    /// every install, that runs `vapor service check --loop`. It is the
    /// one thing that restarts a crashed daemon, under the crash-loop
    /// guard's budget. Kept alive by the service manager.
    pub const SUPERVISOR_LABEL: &str = "sh.arn.vapor.supervisor";

    /// `vapor service status --json` `status`.
    pub const STATUS_NOT_INSTALLED: &str = "not-installed";
    pub const STATUS_STOPPED: &str = "stopped";
    pub const STATUS_RUNNING: &str = "running";
    pub const STATUS_CRASH_LOOP_PAUSED: &str = "crash-loop-paused";

    /// `result` of `bootstrap`, `install`, `uninstall`, `start`, `stop`,
    /// `restart`, and `acknowledge`.
    pub const RESULT_UNCHANGED: &str = "unchanged";
    pub const RESULT_STARTED: &str = "started";
    pub const RESULT_STOPPED: &str = "stopped";
    pub const RESULT_NOT_INSTALLED: &str = "not-installed";
    pub const RESULT_CRASH_LOOP_PAUSED: &str = "crash-loop-paused";
    pub const RESULT_RELAUNCH_DEFERRED: &str = "relaunch-deferred";
    pub const RESULT_ACKNOWLEDGED: &str = "acknowledged";

    /// `health` of `vapor service check`.
    pub const HEALTH_RUNNING: &str = "running";
    pub const HEALTH_NOT_INSTALLED: &str = "not-installed";
    pub const HEALTH_STOPPED_EXPECTED: &str = "stopped-expected";
    pub const HEALTH_RESTARTED_AFTER_CRASH: &str = "restarted-after-crash";
    pub const HEALTH_RESTART_DEFERRED: &str = "restart-deferred";
    pub const HEALTH_CRASH_LOOP_PAUSED: &str = "crash-loop-paused";
}

pub mod secrets {
    /// Namespace under which every native secret store files Vapor's
    /// entries: the Keychain service name on macOS, the credential
    /// target prefix on Windows, the Secret Service attribute on Linux.
    /// Secret names (`auth.<profile>.<provider>.token`) are the account
    /// inside that namespace, so a user can find and remove every Vapor
    /// item in the OS keychain UI by this one string.
    pub const STORE_NAMESPACE: &str = "sh.arn.vapor";
    /// The command shim's verbs, for a Linux host where an external
    /// secret manager holds the tokens: `<command> get <name>` prints
    /// the secret, `<command> set <name>` reads it on stdin,
    /// `<command> delete <name>`, `<command> list` prints one name per
    /// line. Exit 0 on success, 1 when a secret is not found.
    pub const COMMAND_VERB_GET: &str = "get";
    pub const COMMAND_VERB_SET: &str = "set";
    pub const COMMAND_VERB_DELETE: &str = "delete";
    pub const COMMAND_VERB_LIST: &str = "list";
}

pub mod ipc {
    /// Current schema version emitted by every Vapor surface that
    /// participates in the IPC handshake (`vapor` CLI, future macOS /
    /// Windows / Linux apps, the daemon). Bump on any backwards-
    /// incompatible payload shape change. Pre-GA the sliding tolerance
    /// is `|N - M| <= 1`; see `docs/architecture/ipc-contracts.md`.
    pub const SCHEMA_VERSION_CURRENT: u32 = 5;
    /// Minimum peer schema version this build can interoperate with.
    /// Together with [`SCHEMA_VERSION_CURRENT`] this defines the local
    /// support window; the handshake fails when both sides cannot find
    /// an overlapping version. Version 5 lets a control request name
    /// one profile. A version 4 daemon would not read the name and
    /// would pause, resume, or scan every profile, so the window
    /// starts at 5.
    pub const SCHEMA_VERSION_MIN_SUPPORTED: u32 = 5;
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
    /// Internal artifacts the engine must always ignore on the local
    /// side, independent of user-configurable ignore rules: in-flight
    /// atomic-write temp files, op-id side-files, and the root marker.
    /// Loop prevention depends on these never becoming intents, so they
    /// are enforced in the path filter itself rather than the editable
    /// rule set. The names come from the provider constants, which
    /// define them. The marker is matched as a whole name: a user file
    /// named `.vapor-roots.txt` syncs.
    pub const INTERNAL_IGNORE_FILE_PREFIXES: &[&str] = &[super::provider::TEMP_FILE_PREFIX];
    pub const INTERNAL_IGNORE_FILE_SUFFIXES: &[&str] = &[super::provider::OP_ID_SIDE_FILE_SUFFIX];
    pub const INTERNAL_IGNORE_FILE_NAMES: &[&str] = &[super::provider::ROOT_MARKER_FILE_NAME];
    /// Directory names that are Vapor's own state wherever they sit
    /// and never sync, with everything under them: the runtime
    /// directory (`~/.vapor`, a `VAPOR_DIR` inside a sync root, a dev
    /// checkout's `./.vapor` with its logs and e2e sandboxes) and the
    /// trash a sync root's volume carries at `<volume root>/.vapor/trash/`.
    /// Matched as a whole name only, so `.vaporignore` still syncs.
    pub const INTERNAL_IGNORE_DIRECTORY_NAMES: &[&str] = &[".vapor"];
    /// The local root that setup offers (`vapor profiles add`, the
    /// app's first run). A suggestion: no profile exists until the user
    /// accepts or changes it.
    pub const DEFAULT_LOCAL_ROOT: &str = "~/Vapor";
    /// The cloud root that setup offers for a remote provider: a
    /// folder at the account root.
    pub const DEFAULT_CLOUD_ROOT: &str = "/Vapor";
    /// The cloud root that setup offers for the filesystem provider.
    /// It plays the cloud role from a local folder, so the suggestion
    /// is a folder under the home directory that the daemon can
    /// create; a path at the filesystem root would never be writable
    /// on a stock macOS install.
    pub const DEFAULT_FILESYSTEM_CLOUD_ROOT: &str = "~/cloud/Vapor";

    /// The cloud root that setup offers, by provider kind.
    pub fn default_cloud_root(provider_kind: &str) -> &'static str {
        if provider_kind.trim() == super::provider::FILESYSTEM {
            DEFAULT_FILESYSTEM_CLOUD_ROOT
        } else {
            DEFAULT_CLOUD_ROOT
        }
    }
    /// Baseline low-signal exclusions applied before discovered ignore
    /// files: OS junk, editor temp/partial files, and the common
    /// build / cache / dependency directories across ecosystems:
    /// regenerable machine output that would dominate sync traffic.
    /// Deliberately NOT ignored: `.git/` (repositories sync whole) and
    /// dotenv files (a personal sync root is exactly where a backup of
    /// local secrets belongs: project owner decision, 2026-07-13).
    pub const DEFAULT_PRE_IGNORE_RULES: &[&str] = &[
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
    /// Office documents, PDFs, and images: the files ordinary users care
    /// about most. Their atomic-save patterns settle within 1–2s, and the
    /// per-path coalescing map absorbs multi-event bursts, so they need
    /// nowhere near the conservative `Other` window.
    pub const DOCUMENT_DEBOUNCE_WINDOW_MILLIS: u64 = 1_500;
    pub const DEFAULT_DEBOUNCE_WINDOW_MILLIS: u64 = 4_000;
    /// The quiet window of a file with no class of its own whose writes
    /// came in one short burst: it was written in one go (a copy, a
    /// save), so it is complete when the burst is. The longer default
    /// window stays for a file whose writes are spread out, which is
    /// what a file that still grows looks like.
    pub const SETTLED_FILE_DEBOUNCE_WINDOW_MILLIS: u64 = 1_000;
    /// Writes spread over more than this are those of a file that grows.
    pub const GROWING_FILE_SPAN_MILLIS: u64 = 1_000;
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
    /// A profile that the runtime suspended (a panic, repeated tick
    /// errors) is composed again after these waits; the last one
    /// repeats. A full disk or a locked database clears on its own, and
    /// a profile that stays suspended until a manual restart would not
    /// find out.
    pub const SUSPENDED_PROFILE_RETRY_SECONDS: [u64; 3] = [60, 5 * 60, 30 * 60];
    /// A folder the walk could not read (a permission, a transient I/O
    /// error) is walked again after this long.
    pub const RECONCILE_UNREADABLE_RETRY_SECONDS: u64 = 15 * 60;
    /// A running daemon scans the whole scope again this long after the
    /// last whole-scope scan, so a change that no event reported is
    /// found within a day and not at the next restart.
    pub const WHOLE_SCOPE_RECONCILE_INTERVAL_SECONDS: u64 = 24 * 60 * 60;
    pub const LEASE_TIMEOUT_MILLIS: u64 = 15 * 60 * 1_000;
    pub const LIGHT_SYSTEM_CPU_PERCENT: u8 = 35;
    pub const THROTTLED_SYSTEM_CPU_PERCENT: u8 = 60;
    pub const LIGHT_NETWORK_ERROR_RATE_PERCENT: u8 = 10;
    pub const THROTTLED_NETWORK_ERROR_RATE_PERCENT: u8 = 25;
    /// A link that carries this little (in its slower direction) is
    /// a slow link: fewer transfers at a time.
    pub const LIGHT_LINK_CAPACITY_KBPS: u32 = 512;
    pub const THROTTLED_LINK_CAPACITY_KBPS: u32 = 128;
    /// Idle concurrency tier bounds. The effective tier derives
    /// from the machine's available parallelism (half the cores),
    /// clamped into `[MIN, MAX]`: a 16-core desktop drains faster than
    /// a 2-core laptop without oversubscribing either. Light/Throttled
    /// tiers stay fixed: they bound device impact while the user is
    /// active, where core count is not the limit that matters.
    pub const IDLE_CONCURRENCY_MIN: usize = 4;
    pub const IDLE_CONCURRENCY_MAX: usize = 8;
    pub const IDLE_READ_TOKENS: usize = 2;
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
    /// A transfer of at most this many bytes is a small one. What a
    /// small transfer costs is its commit, not its bytes: one flush of
    /// the drive, which the transfers of the same moment share. So on a
    /// provider whose commits are shared, small transfers have a lane
    /// of their own next to the transfer slots of the throttle state.
    pub const SMALL_TRANSFER_MAX_BYTES: u64 = 256 * 1024;
    /// The width of that lane: this many small transfers for each
    /// transfer slot of the throttle state, in each direction. Every
    /// state keeps its ladder (the lane of `Throttled` is the narrowest
    /// and `Suspended` has none), and a flush that carries more files
    /// is fewer flushes for the same work.
    pub const SMALL_TRANSFERS_PER_SLOT: usize = 5;
    /// Byte budget one hashing execution may consume per runtime tick.
    /// Bounds the per-tick CPU/read cost of the hash stage while still
    /// hashing large files at a useful rate (8 MiB * 4 Hz = 32 MiB/s).
    pub const HASH_STAGE_STEP_BYTES: u64 = 8 * 1024 * 1024;
    /// Bounds on worker threads running blocking provider I/O (probes,
    /// transfer sessions, remote deletes) off the runtime tick thread.
    /// The effective cap is twice the Idle concurrency tier with its
    /// lane of small transfers, clamped into `[MIN, MAX]`, so uploads
    /// and downloads can both run at full width. A small transfer holds
    /// its worker while it waits for the flush it shares, so the lane
    /// needs a thread for each. Threads spawn lazily per in-flight job
    /// and sit parked on a channel otherwise; the throttle workgate,
    /// not this cap, bounds how much work is admitted.
    pub const PROVIDER_JOB_WORKERS_MIN: usize = 8;
    pub const PROVIDER_JOB_WORKERS_MAX: usize = 96;
    /// Remote changes-feed poll cadence per throttle state, for a feed
    /// that is a network call. Suspended never polls.
    pub const REMOTE_POLL_IDLE_SECONDS: u64 = 5;
    pub const REMOTE_POLL_LIGHT_SECONDS: u64 = 15;
    pub const REMOTE_POLL_THROTTLED_SECONDS: u64 = 60;
    /// The cadence of a feed that a watcher on this device feeds: a
    /// poll costs no request, so it is read each second in every state
    /// but `Suspended`.
    pub const REMOTE_POLL_WATCHED_SECONDS: u64 = 1;
    /// The cadence while a change is near: for this long after a poll
    /// that brought changes, or after a transfer of this device, the
    /// feed is polled at the `Idle` cadence in every state but
    /// `Suspended`. A device in use would otherwise see the next change
    /// of a working session a minute late.
    pub const REMOTE_POLL_ACTIVE_WINDOW_SECONDS: u64 = 60;
    /// Maximum remote changes consumed per poll page.
    pub const REMOTE_CHANGES_PAGE_MAX: usize = 256;
    /// Retry cadence for ensuring the provider-side sync root when the
    /// initial attempt failed. Sync work stays blocked (and
    /// intents accumulate durably) between attempts.
    pub const CLOUD_ROOT_ENSURE_RETRY_SECONDS: u64 = 60;
    /// A read of the ignore files that failed is tried again after
    /// this long. Until it works, no local change is released: a name
    /// that an ignore file hides must not sync on the word of rules
    /// that are out of date.
    pub const IGNORE_RULES_RETRY_SECONDS: u64 = 60;
    /// How often a running daemon re-checks both sync roots (present,
    /// and carrying the identity the profile adopted).
    pub const ROOT_CHECK_INTERVAL_SECONDS: u64 = 15;
    /// An upload that the cloud had no room for is tried again after
    /// this long. Freeing space takes a person, so a faster retry only
    /// spends requests.
    pub const CLOUD_FULL_RETRY_SECONDS: u64 = 15 * 60;
    /// How the stored error of an upload that waits for room in the
    /// cloud starts. The queue row is the record of the wait, so a
    /// request for sync finds the waiting uploads after a restart too.
    pub const CLOUD_FULL_WAIT_PREFIX: &str = "waits for room in the cloud: ";
    /// After a path-scoped decision is applied, the reconcile walk gives
    /// the answer this long to land before it may ask about the same
    /// path again.
    pub const DECISION_APPLY_GRACE_SECONDS: u64 = 600;
    /// The deletion of a synced file may be half of a rename whose
    /// create is still coming through the debounce. Its first planning
    /// always waits this long, and later ones wait while a transfer of
    /// the same size is queued, at most this many times in all, before
    /// it proceeds as a deletion.
    pub const MOVE_SETTLE_DELAY_SECONDS: u64 = 3;
    pub const MOVE_SETTLE_MAX_DEFERRALS: u32 = 3;
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
    /// Idle). Higher than the checkpoint granularity so a large tree
    /// converges quickly on a fast (filesystem) provider; the per-slice
    /// wall-clock deadline caps the cost when the provider's enumerate is
    /// a slow network call.
    pub const RECONCILE_DIRS_PER_SLICE_IDLE: usize = 64;
    /// Cloud folders a reconcile walk asks for at one time while the
    /// device is idle; fewer in the other throttle states. A listing is
    /// one request to the provider, and a walk that waits for each
    /// answer before it asks for the next folder takes a second a
    /// folder on a cloud API. The comparison still takes the folders
    /// one by one.
    pub const RECONCILE_LISTINGS_IN_FLIGHT: usize = 8;
    /// The link capacity the daemon uses under
    /// `VAPOR_THROTTLE_INPUTS=static`, where nothing is measured: the
    /// test harnesses need the same transfer rate on every host. A
    /// production daemon measures its link and never uses this.
    pub const STATIC_INPUTS_LINK_CAPACITY_KBPS: u32 = 100_000;
    /// The link capacity is the highest rate that a transfer step
    /// with no limiter on it reached inside this window. An estimate
    /// older than this gets one probe.
    pub const LINK_CAPACITY_WINDOW_SECONDS: u64 = 5 * 60;
    /// A transfer step measures the link only when it moved at least
    /// this much over at least this long; anything smaller is timer
    /// noise and request overhead.
    pub const LINK_CAPACITY_SAMPLE_MIN_BYTES: u64 = 128 * 1024;
    pub const LINK_CAPACITY_SAMPLE_MIN_MILLIS: u64 = 10;
    /// Measurements kept for each direction.
    pub const LINK_CAPACITY_SAMPLES_MAX: usize = 64;
    /// The least the one step that runs without the limiter moves
    /// when the estimate is old. On a link measured faster, the probe
    /// moves about one second's worth, so the round trip to the
    /// provider is a small part of what it times.
    pub const LINK_CAPACITY_PROBE_BYTES: u64 = 1024 * 1024;
    /// `vapor status` reports the bandwidth the daemon used over this
    /// many seconds.
    pub const BANDWIDTH_USAGE_WINDOW_SECONDS: u64 = 5;
    /// The least time between two status snapshots while work is in
    /// flight. A snapshot runs SQL for each profile, and under a burst
    /// the tick loop turns over a thousand times a second. A state that
    /// changes (a pause, an error, a question) is published at once.
    pub const STATUS_BUSY_REPUBLISH_MILLIS: u64 = 250;
    /// Fallback cadence for re-publishing the IPC status snapshot while
    /// idle. Control requests and per-profile state flips publish
    /// immediately; this heartbeat only bounds the staleness of
    /// anything those triggers miss, so a fully idle daemon runs the
    /// snapshot's per-profile SQL at this cadence instead of every tick.
    pub const STATUS_REPUBLISH_HEARTBEAT_SECONDS: u64 = 10;
    /// After `vapor reconcile` the runtime releases deferred work
    /// eagerly for this long, so what the scan finds moves at once.
    pub const RECONCILE_BOOST_SECONDS: u64 = 30;
    pub const RETRY_BASE_DELAY_MILLIS: u64 = 2_000;
    pub const RETRY_RATE_LIMIT_BASE_DELAY_MILLIS: u64 = 15_000;
    pub const RETRY_MAX_DELAY_MILLIS: u64 = 900_000;
    /// Ceiling for a server-supplied `Retry-After`: a bogus or absurd
    /// header (garbage seconds, a mistaken epoch timestamp) is clamped to
    /// this so it cannot overflow time arithmetic or park an intent, and
    /// the persisted global rate-limit slowdown, for months. One hour is
    /// well past any legitimate provider backoff.
    pub const RETRY_AFTER_CEILING_MILLIS: u64 = 3_600_000;
    pub const RETRY_JITTER_PERCENT: u8 = 20;
    pub const STORM_WINDOW_MILLIS: u64 = 2_000;
    pub const STORM_DIRECTORY_UNIQUE_PATHS_THRESHOLD: usize = 200;
    pub const STORM_DIRECTORY_EVENT_COUNT_THRESHOLD: usize = 600;
    pub const STORM_GLOBAL_PENDING_EVENT_COUNT_THRESHOLD: usize = 5_000;
    /// How long after a storm ends its deferred reconcile starts, in the
    /// throttle states other than `Idle`: the device is in use or under
    /// a constraint, and the scan must not compete with what made the
    /// storm.
    pub const DEFERRED_RECONCILE_DELAY_MILLIS: u64 = 30_000;
    /// The same wait in `Idle`. The deferral protects a device under
    /// load; on an idle one it only costs the time the user waits for a
    /// clone or an install to reach the cloud folder.
    pub const DEFERRED_RECONCILE_IDLE_QUIET_RELEASE_MILLIS: u64 = 1_500;
    /// A compacted folder still counts as in a storm while it takes in
    /// this many events inside one storm window. Fewer than that is
    /// ordinary work (a save a second), which must not keep pushing the
    /// reconcile of the folder back.
    pub const STORM_ABSORBED_BUSY_EVENTS: usize = 50;
    /// Upper bound on how long a storm-deferred reconcile may keep being
    /// pushed back by a storm that goes on, measured from the moment the
    /// storm was first detected. Once reached, the reconcile starts even
    /// if the folder never goes quiet, so a folder that is busy for good
    /// still converges.
    pub const DEFERRED_RECONCILE_MAX_DELAY_MILLIS: u64 = 600_000;
    /// The same bound in `Idle`: nothing else needs the device, so a
    /// long storm is scanned every half minute while it lasts.
    pub const DEFERRED_RECONCILE_IDLE_MAX_DELAY_MILLIS: u64 = 30_000;
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
    /// How long a removal that needs a copy to the trash of another
    /// volume waits before it looks again, while the throttle is
    /// `Suspended`.
    pub const SUSPENDED_COPY_WAIT_SECONDS: u64 = 30;
    /// How long the removal of a cloud folder waits, at least, while
    /// removals of its entries are still in the queue.
    pub const FOLDER_DELETE_WAIT_SECONDS: u64 = 2;
    /// How long a download waits when a synced folder that the cloud
    /// replaced with a file still holds files here: their removals are
    /// queued and run in this time.
    pub const TYPE_CLASH_WAIT_SECONDS: u64 = 3;
    /// The same wait while a mass-delete question holds those removals:
    /// nothing moves until the user answers, so the look is rare.
    pub const TYPE_CLASH_WAIT_HELD_SECONDS: u64 = 60;
    /// Minimum dwell time before the throttle controller may down-shift to
    /// or out of `Light`. Pairs with `MIN_DWELL_THROTTLED_SECONDS` to keep
    /// state stable under oscillating CPU / network samples.
    pub const MIN_DWELL_LIGHT_SECONDS: u64 = 5;
    /// Minimum dwell time before the throttle controller may down-shift to
    /// or out of `Throttled`. See `MIN_DWELL_LIGHT_SECONDS`.
    pub const MIN_DWELL_THROTTLED_SECONDS: u64 = 5;
    /// Minimum dwell time before the throttle controller may exit
    /// `Suspended`. Shorter than the lower-pressure tiers because
    /// `Suspended` is the safest state: staying suspended for an extra
    /// second is never harmful, but exiting too aggressively could mask
    /// real pressure.
    pub const MIN_DWELL_SUSPENDED_SECONDS: u64 = 1;
}
