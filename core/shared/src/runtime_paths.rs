use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crate::constants;

pub fn vapor_directory() -> PathBuf {
    if let Some(configured) = env::var_os(constants::env::VAPOR_DIR)
        && let Some(path) = normalize_override_path(PathBuf::from(configured))
    {
        return path;
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

pub fn ensure_private_directory(path: &std::path::Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(constants::runtime::PRIVATE_DIRECTORY_MODE),
    )
}

pub fn ensure_private_file(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_directory(parent)?;
    }
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(constants::runtime::PRIVATE_FILE_MODE),
    )
}

fn normalize_override_path(path: PathBuf) -> Option<PathBuf> {
    if path.as_os_str().is_empty() {
        return None;
    }

    let candidate = if path.is_absolute() {
        path
    } else {
        env::current_dir().ok()?.join(path)
    };

    normalize_absolute_path(candidate)
}

fn normalize_absolute_path(path: PathBuf) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }

    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::Prefix(_) => return None,
        }
    }

    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn relative_vapor_dir_override_is_normalized_against_current_directory() {
        let current_directory = env::current_dir().expect("current dir");
        let normalized = normalize_override_path(PathBuf::from("./nested/../.vapor-test"))
            .expect("normalized override");

        assert_eq!(normalized, current_directory.join(".vapor-test"));
    }

    #[test]
    fn private_directory_and_file_helpers_apply_restrictive_permissions() {
        let temp_dir = TempDir::new().expect("temp dir");
        let directory = temp_dir.path().join("runtime/state");
        let file = directory.join("vapor.sqlite");

        ensure_private_directory(&directory).expect("ensure private directory");
        ensure_private_file(&file).expect("ensure private file");

        let directory_mode = fs::metadata(&directory)
            .expect("directory metadata")
            .permissions()
            .mode()
            & 0o777;
        let file_mode = fs::metadata(&file)
            .expect("file metadata")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(directory_mode, constants::runtime::PRIVATE_DIRECTORY_MODE);
        assert_eq!(file_mode, constants::runtime::PRIVATE_FILE_MODE);
    }
}
