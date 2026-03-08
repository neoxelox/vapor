use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use vapor_shared::constants;

use crate::logging;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncScope {
    pub local_sync_directory: Option<PathBuf>,
    pub cloud_sync_directory: String,
}

pub fn resolve_from_process_environment() -> SyncScope {
    let configured_local = env::var(constants::env::VAPOR_LOCAL_SYNC_DIRECTORY).ok();
    let configured_cloud = env::var(constants::env::VAPOR_CLOUD_SYNC_DIRECTORY).ok();
    let current_directory = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home_directory = home_directory();

    resolve_scope(
        configured_local.as_deref(),
        configured_cloud.as_deref(),
        &current_directory,
        home_directory.as_deref(),
    )
}

fn resolve_scope(
    configured_local: Option<&str>,
    configured_cloud: Option<&str>,
    current_directory: &Path,
    home_directory: Option<&Path>,
) -> SyncScope {
    let local_raw = configured_local.unwrap_or(constants::filtering::DEFAULT_LOCAL_SYNC_DIRECTORY);
    let local_sync_directory =
        resolve_local_directory(local_raw, current_directory, home_directory);
    let cloud_sync_directory = resolve_cloud_directory(
        configured_cloud.unwrap_or(constants::filtering::DEFAULT_CLOUD_SYNC_DIRECTORY),
    );

    SyncScope {
        local_sync_directory,
        cloud_sync_directory,
    }
}

fn resolve_local_directory(
    raw: &str,
    current_directory: &Path,
    home_directory: Option<&Path>,
) -> Option<PathBuf> {
    let Some(path) = resolve_path(raw, current_directory, home_directory) else {
        logging::warning(
            "No local sync directory configured; daemon will remain idle",
            &[],
        );
        return None;
    };

    if !path.exists() {
        match fs::create_dir_all(&path) {
            Ok(_) => {
                logging::info(
                    "Created missing local sync directory",
                    &[("path", path.display().to_string())],
                );
            }
            Err(error) => {
                logging::error(
                    "Failed to create missing local sync directory",
                    &[
                        ("path", path.display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
                return None;
            }
        }
    }

    if !path.is_dir() {
        logging::warning(
            "Skipping local sync directory because path is not a directory",
            &[("path", path.display().to_string())],
        );
        return None;
    }

    Some(path)
}

fn resolve_cloud_directory(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return constants::filtering::DEFAULT_CLOUD_SYNC_DIRECTORY.to_string();
    }

    if trimmed.starts_with('/') {
        return trimmed.to_string();
    }

    format!("/{trimmed}")
}

fn resolve_path(
    raw: &str,
    current_directory: &Path,
    home_directory: Option<&Path>,
) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed == "~" {
        return home_directory.map(Path::to_path_buf);
    }

    if let Some(suffix) = trimmed.strip_prefix("~/") {
        return home_directory.map(|home| home.join(suffix));
    }

    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        return Some(path);
    }

    Some(current_directory.join(path))
}

fn home_directory() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn defaults_to_home_vapor_and_cloud_vapor_directories() {
        let home = create_test_directory();

        let scope = resolve_scope(None, None, Path::new("/tmp"), Some(home.as_path()));
        assert_eq!(scope.local_sync_directory, Some(home.join("Vapor")));
        assert_eq!(scope.cloud_sync_directory, "/Vapor");
        assert!(home.join("Vapor").exists());

        remove_test_directory(&home);
    }

    #[test]
    fn creates_missing_local_directory() {
        let root = create_test_directory();
        let missing = root.join("missing");

        let scope = resolve_scope(
            Some(missing.to_string_lossy().as_ref()),
            Some("/Cloud"),
            Path::new("/tmp"),
            None,
        );
        assert_eq!(scope.local_sync_directory, Some(missing.clone()));
        assert!(missing.exists());
        assert_eq!(scope.cloud_sync_directory, "/Cloud");

        remove_test_directory(&root);
    }

    #[test]
    fn returns_none_when_local_sync_path_is_a_file() {
        let root = create_test_directory();
        let file_path = root.join("not-a-directory");
        fs::write(&file_path, b"x").expect("failed to create file path");

        let scope = resolve_scope(
            Some(file_path.to_string_lossy().as_ref()),
            Some("/Cloud"),
            Path::new("/tmp"),
            None,
        );
        assert!(scope.local_sync_directory.is_none());
        assert_eq!(scope.cloud_sync_directory, "/Cloud");

        remove_test_directory(&root);
    }

    #[test]
    fn resolves_relative_local_path_against_current_directory() {
        let root = create_test_directory();
        let projects = root.join("projects");
        fs::create_dir_all(&projects).expect("failed to create projects directory");

        let scope = resolve_scope(Some("projects"), Some("cloud-folder"), &root, None);
        assert_eq!(scope.local_sync_directory, Some(projects));
        assert_eq!(scope.cloud_sync_directory, "/cloud-folder");

        remove_test_directory(&root);
    }

    #[test]
    fn empty_cloud_directory_falls_back_to_default() {
        let root = create_test_directory();
        let sync = root.join("sync");
        fs::create_dir_all(&sync).expect("failed to create sync directory");

        let scope = resolve_scope(
            Some(sync.to_string_lossy().as_ref()),
            Some("   "),
            Path::new("/tmp"),
            None,
        );
        assert_eq!(scope.cloud_sync_directory, "/Vapor");

        remove_test_directory(&root);
    }

    fn create_test_directory() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "vapor-daemon-sync-scope-{}-{}",
            std::process::id(),
            timestamp
        ));
        fs::create_dir_all(&root).expect("failed to create test directory");
        root
    }

    fn remove_test_directory(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }
}
