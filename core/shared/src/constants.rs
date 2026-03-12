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
}

pub mod filtering {
    pub const GIT_IGNORE_FILE_NAME: &str = ".gitignore";
    pub const VAPOR_IGNORE_FILE_NAME: &str = ".vaporignore";
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
    pub const LIGHT_PLANNER_WORKERS: usize = 2;
    pub const LIGHT_HASH_WORKERS: usize = 2;
    pub const LIGHT_READ_TOKENS: usize = 1;
    pub const LIGHT_UPLOAD_CONCURRENCY: usize = 2;
    pub const THROTTLED_PLANNER_WORKERS: usize = 1;
    pub const THROTTLED_HASH_WORKERS: usize = 1;
    pub const THROTTLED_READ_TOKENS: usize = 1;
    pub const THROTTLED_UPLOAD_CONCURRENCY: usize = 1;
}
