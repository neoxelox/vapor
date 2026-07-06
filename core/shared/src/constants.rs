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
}

pub mod runtime {
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
}

pub mod state {
    pub const RETRY_SLOWDOWN_UNTIL_KEY: &str = "queue.retry_slowdown_until_ms";
    /// Tombstones older than this are pruned at daemon startup: after a
    /// month, a divergent replica reconciles through content comparison
    /// anyway, and unbounded tombstone growth would violate the memory
    /// and storage bounds (C8-16).
    pub const TOMBSTONE_RETENTION_MILLIS: u64 = 30 * 24 * 60 * 60 * 1_000;
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
    pub const KEY_TIMELINE_EVENT_LIMIT: &str = "timelineEventLimit";
    /// Provider selection (C8-2): `filesystem` (default pre-GA) or
    /// `google_drive`. See `provider::*` for the accepted values.
    pub const KEY_PROVIDER: &str = "provider";
    /// Sync direction selector (C8-59): `two-way` (default),
    /// `pull-only`, `push-only`. See `sync_mode::*` and
    /// `docs/architecture/sync-modes.md`.
    pub const KEY_SYNC_MODE: &str = "syncMode";
    /// Stable per-device identifier used by the keep-both conflict
    /// suffix (C8-15). Derived from the hostname at first run, persisted
    /// here, and never silently regenerated.
    pub const KEY_DEVICE_ID: &str = "deviceId";
    /// Profile array (C8-19). Each entry is an object with the
    /// `profile::KEY_*` fields; absent means the single implicit
    /// profile assembled from the top-level settings.
    pub const KEY_PROFILES: &str = "profiles";
    /// User resource-budget group (C8-32); object with the
    /// `resource_limits::KEY_*` fields.
    pub const KEY_RESOURCE_LIMITS: &str = "resourceLimits";
    /// Idle-boost group (C8-33); object with the `idle_boost::KEY_*`
    /// fields.
    pub const KEY_IDLE_BOOST: &str = "idleBoost";

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
        KEY_TIMELINE_EVENT_LIMIT,
        KEY_PROVIDER,
        KEY_SYNC_MODE,
        KEY_DEVICE_ID,
        KEY_PROFILES,
        KEY_RESOURCE_LIMITS,
        KEY_IDLE_BOOST,
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
    pub const DEFAULT_TIMELINE_EVENT_LIMIT: i64 = 1_000;
}

pub mod provider {
    /// Accepted `provider` config values. `filesystem` is the pre-GA
    /// default (C8-2); when selected, `cloudSyncDirectory` is
    /// reinterpreted as an absolute local directory that plays the role
    /// of the cloud side. `google_drive` selects the real cloud
    /// provider once C8-54 flips it selectable.
    pub const FILESYSTEM: &str = "filesystem";
    pub const GOOGLE_DRIVE: &str = "google_drive";
    pub const DEFAULT: &str = FILESYSTEM;
    pub const ALL: &[&str] = &[FILESYSTEM, GOOGLE_DRIVE];

    /// Extended-attribute name carrying the daemon's operation id on
    /// files the daemon itself wrote (self-write loop prevention, C8-7).
    /// NTFS ADS stream name on Windows once Wave 12 lands.
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
    /// Bounded in-memory ring size of the filesystem provider's changes
    /// feed. A cursor older than the ring floor reports `CursorExpired`,
    /// which forces a reconcile instead of silently missing changes.
    pub const CHANGES_FEED_RING_MAX_EVENTS: usize = 8_192;
}

pub mod profile {
    /// Keys of each object in the top-level `profiles` array (C8-19).
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

pub mod sync_mode {
    /// Accepted `syncMode` config values (C8-59). The names describe the
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
    pub const DEFAULT_PRE_IGNORE_RULES: &[&str] = &[
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
    pub const DEFAULT_DEBOUNCE_WINDOW_MILLIS: u64 = 4_000;
    pub const THROTTLE_SAMPLE_INTERVAL_MILLIS: u64 = 1_000;
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
    pub const IDLE_DRAIN_PLANNER_WORKERS: usize = 4;
    pub const IDLE_DRAIN_HASH_WORKERS: usize = 4;
    pub const IDLE_DRAIN_READ_TOKENS: usize = 2;
    pub const IDLE_DRAIN_UPLOAD_CONCURRENCY: usize = 4;
    pub const IDLE_DRAIN_DOWNLOAD_CONCURRENCY: usize = 4;
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
    /// Byte budget one upload/download transfer session may consume per
    /// runtime tick. The bandwidth shaper (C8-38) lowers the effective
    /// budget further when a user bandwidth ceiling applies.
    pub const TRANSFER_STAGE_STEP_BYTES: u64 = 8 * 1024 * 1024;
    /// Remote changes-feed poll cadence per throttle state (C8-52 uses
    /// the same discipline for Google Drive). Suspended never polls.
    pub const REMOTE_POLL_IDLE_DRAIN_SECONDS: u64 = 5;
    pub const REMOTE_POLL_LIGHT_SECONDS: u64 = 15;
    pub const REMOTE_POLL_THROTTLED_SECONDS: u64 = 60;
    /// Maximum remote changes consumed per poll page.
    pub const REMOTE_CHANGES_PAGE_MAX: usize = 256;
    /// Retry cadence for ensuring the provider-side sync root when the
    /// initial attempt failed (C8-50). Sync work stays blocked (and
    /// intents accumulate durably) between attempts.
    pub const CLOUD_ROOT_ENSURE_RETRY_SECONDS: u64 = 60;
    /// Directories the reconcile comparison walk processes per runtime
    /// tick while a reconcile slice is active. Bounds per-tick I/O so
    /// the slice checkpoints keep their interruptibility guarantee.
    pub const RECONCILE_DIRS_PER_CHECKPOINT: usize = 8;
    pub const RETRY_BASE_DELAY_MILLIS: u64 = 2_000;
    pub const RETRY_RATE_LIMIT_BASE_DELAY_MILLIS: u64 = 15_000;
    pub const RETRY_MAX_DELAY_MILLIS: u64 = 900_000;
    pub const RETRY_JITTER_PERCENT: u8 = 20;
    pub const STORM_WINDOW_MILLIS: u64 = 2_000;
    pub const STORM_DIRECTORY_UNIQUE_PATHS_THRESHOLD: usize = 200;
    pub const STORM_DIRECTORY_EVENT_COUNT_THRESHOLD: usize = 600;
    pub const STORM_GLOBAL_PENDING_EVENT_COUNT_THRESHOLD: usize = 5_000;
    pub const DEFERRED_RECONCILE_DELAY_MILLIS: u64 = 30_000;
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
    /// state stable under oscillating CPU / network samples (C2-4).
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
