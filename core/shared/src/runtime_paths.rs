use std::env;
use std::path::PathBuf;

use crate::constants;

pub fn vapor_directory() -> PathBuf {
    if let Some(configured) = env::var_os(constants::env::VAPOR_DIR) {
        return PathBuf::from(configured);
    }

    if (env::var_os("CI").is_some()
        || env::var(constants::env::VAPOR_ENV)
            .ok()
            .map(|value| value.eq_ignore_ascii_case("dev"))
            .unwrap_or(false))
        && let Ok(current_directory) = env::current_dir()
    {
        return current_directory.join(constants::runtime::VAPOR_DIRECTORY_NAME);
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(constants::runtime::VAPOR_DIRECTORY_NAME);
    }

    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(constants::runtime::VAPOR_DIRECTORY_NAME)
}

pub fn logs_directory() -> PathBuf {
    vapor_directory().join(constants::runtime::LOGS_DIRECTORY_NAME)
}

pub fn state_directory() -> PathBuf {
    vapor_directory().join(constants::runtime::STATE_DIRECTORY_NAME)
}

pub fn sqlite_database_path() -> PathBuf {
    state_directory().join(constants::runtime::SQLITE_DATABASE_FILE_NAME)
}
