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
}
