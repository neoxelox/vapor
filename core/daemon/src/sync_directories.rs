use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use vapor_shared::{SyncMode, constants, runtime_paths};

use crate::logging;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncScope {
    pub local_sync_directory: Option<PathBuf>,
    pub cloud_sync_directory: String,
    /// Sync direction for this scope (C8-59). Carried on the scope so
    /// every pipeline gate (ingest, remote apply, reconcile, executor)
    /// consults the same value — no operation can bypass it.
    pub sync_mode: SyncMode,
}

/// Resolves the sync scope from environment variables layered over
/// compiled defaults only. Prefer [`resolve_with_config`] in daemon
/// composition so persisted `vapor.json` settings take effect.
pub fn resolve_from_process_environment() -> SyncScope {
    resolve_with_config(&vapor_shared::config::VaporConfig::default())
}

/// Resolves the sync scope with the canonical precedence per field:
/// `VAPOR_*` environment variable → `vapor.json` value → compiled
/// default. Explicitly empty values (empty or whitespace-only strings)
/// are treated as unset at every layer, so an empty env var falls back
/// to the config file rather than silently disabling sync.
pub fn resolve_with_config(config: &vapor_shared::config::VaporConfig) -> SyncScope {
    let env_local = env::var(constants::env::VAPOR_LOCAL_SYNC_DIRECTORY).ok();
    let env_cloud = env::var(constants::env::VAPOR_CLOUD_SYNC_DIRECTORY).ok();
    let current_directory = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home_directory = home_directory();

    resolve_scope(
        env_local.as_deref(),
        env_cloud.as_deref(),
        config,
        &current_directory,
        home_directory.as_deref(),
    )
}

fn resolve_scope(
    env_local: Option<&str>,
    env_cloud: Option<&str>,
    config: &vapor_shared::config::VaporConfig,
    current_directory: &Path,
    home_directory: Option<&Path>,
) -> SyncScope {
    let local_raw = first_non_empty(&[
        env_local,
        Some(config.local_sync_directory.as_str()),
        Some(constants::filtering::DEFAULT_LOCAL_SYNC_DIRECTORY),
    ]);
    let cloud_raw = first_non_empty(&[
        env_cloud,
        Some(config.cloud_sync_directory.as_str()),
        Some(constants::filtering::DEFAULT_CLOUD_SYNC_DIRECTORY),
    ]);

    let local_sync_directory =
        local_raw.and_then(|raw| resolve_local_directory(raw, current_directory, home_directory));
    let cloud_sync_directory = resolve_cloud_directory(
        cloud_raw.unwrap_or(constants::filtering::DEFAULT_CLOUD_SYNC_DIRECTORY),
    );

    SyncScope {
        local_sync_directory,
        cloud_sync_directory,
        sync_mode: resolve_sync_mode(&config.sync_mode),
    }
}

/// Resolves the configured `syncMode` string. Unknown values fall back
/// to the safe default (`two-way` never deletes or overwrites to
/// converge) with a loud warning — a typo must not silently activate a
/// destructive strict-mirror mode, and equally must not activate any
/// mode the user did not spell exactly (C8-63: one-way is explicit
/// opt-in, never inferred).
fn resolve_sync_mode(raw: &str) -> SyncMode {
    match SyncMode::from_config_value(raw) {
        Some(mode) => mode,
        None => {
            logging::warning(
                "Unknown syncMode value; using the safe two-way default",
                &[
                    ("configured_sync_mode", raw.to_string()),
                    ("accepted", constants::sync_mode::ALL.join(", ")),
                ],
            );
            SyncMode::TwoWay
        }
    }
}

/// First candidate that is non-empty after trimming.
fn first_non_empty<'a>(candidates: &[Option<&'a str>]) -> Option<&'a str> {
    candidates
        .iter()
        .flatten()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
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
    runtime_paths::home_directory()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vapor_shared::config::VaporConfig;

    fn default_config() -> VaporConfig {
        VaporConfig::default()
    }

    #[test]
    fn defaults_to_home_vapor_and_cloud_vapor_directories() {
        let (_guard, home) = create_test_directory();

        let scope = resolve_scope(
            None,
            None,
            &default_config(),
            Path::new("/tmp"),
            Some(home.as_path()),
        );
        assert_eq!(scope.local_sync_directory, Some(home.join("Vapor")));
        assert_eq!(scope.cloud_sync_directory, "/Vapor");
        assert!(home.join("Vapor").exists());
    }

    #[test]
    fn config_file_values_are_used_when_environment_is_unset() {
        let (_guard, root) = create_test_directory();
        let configured = root.join("from-config");
        let config = VaporConfig {
            local_sync_directory: configured.to_string_lossy().into_owned(),
            cloud_sync_directory: "/FromConfig".to_string(),
            ..VaporConfig::default()
        };

        let scope = resolve_scope(None, None, &config, Path::new("/tmp"), None);

        assert_eq!(scope.local_sync_directory, Some(configured));
        assert_eq!(scope.cloud_sync_directory, "/FromConfig");
    }

    #[test]
    fn environment_overrides_config_file_values() {
        let (_guard, root) = create_test_directory();
        let from_env = root.join("from-env");
        let config = VaporConfig {
            local_sync_directory: root.join("from-config").to_string_lossy().into_owned(),
            cloud_sync_directory: "/FromConfig".to_string(),
            ..VaporConfig::default()
        };

        let scope = resolve_scope(
            Some(from_env.to_string_lossy().as_ref()),
            Some("/FromEnv"),
            &config,
            Path::new("/tmp"),
            None,
        );

        assert_eq!(scope.local_sync_directory, Some(from_env));
        assert_eq!(scope.cloud_sync_directory, "/FromEnv");
    }

    #[test]
    fn empty_environment_values_fall_back_to_config_then_default() {
        // An explicitly-empty env var is "unset", not "disable sync":
        // the same rule the cloud side always had now applies to the
        // local side too.
        let (_guard, root) = create_test_directory();
        let configured = root.join("from-config");
        let config = VaporConfig {
            local_sync_directory: configured.to_string_lossy().into_owned(),
            ..VaporConfig::default()
        };

        let scope = resolve_scope(Some("   "), Some(""), &config, Path::new("/tmp"), None);

        assert_eq!(scope.local_sync_directory, Some(configured));
        assert_eq!(scope.cloud_sync_directory, "/Vapor");
    }

    #[test]
    fn creates_missing_local_directory() {
        let (_guard, root) = create_test_directory();
        let missing = root.join("missing");

        let scope = resolve_scope(
            Some(missing.to_string_lossy().as_ref()),
            Some("/Cloud"),
            &default_config(),
            Path::new("/tmp"),
            None,
        );
        assert_eq!(scope.local_sync_directory, Some(missing.clone()));
        assert!(missing.exists());
        assert_eq!(scope.cloud_sync_directory, "/Cloud");
    }

    #[test]
    fn returns_none_when_local_sync_path_is_a_file() {
        let (_guard, root) = create_test_directory();
        let file_path = root.join("not-a-directory");
        fs::write(&file_path, b"x").expect("failed to create file path");

        let scope = resolve_scope(
            Some(file_path.to_string_lossy().as_ref()),
            Some("/Cloud"),
            &default_config(),
            Path::new("/tmp"),
            None,
        );
        assert!(scope.local_sync_directory.is_none());
        assert_eq!(scope.cloud_sync_directory, "/Cloud");
    }

    #[test]
    fn invalid_local_sync_path_does_not_fall_back_to_current_or_home_directory() {
        let (_guard, root) = create_test_directory();
        let current_directory = root.join("current-directory");
        let home_directory = root.join("home-directory");
        let file_path = root.join("not-a-directory");
        fs::create_dir_all(&current_directory).expect("failed to create current directory");
        fs::create_dir_all(&home_directory).expect("failed to create home directory");
        fs::write(&file_path, b"x").expect("failed to create file path");

        let scope = resolve_scope(
            Some(file_path.to_string_lossy().as_ref()),
            Some("/Cloud"),
            &default_config(),
            current_directory.as_path(),
            Some(home_directory.as_path()),
        );

        assert!(scope.local_sync_directory.is_none());
        assert_ne!(scope.local_sync_directory, Some(current_directory));
        assert_ne!(scope.local_sync_directory, Some(home_directory));
    }

    #[test]
    fn resolves_relative_local_path_against_current_directory() {
        let (_guard, root) = create_test_directory();
        let projects = root.join("projects");
        fs::create_dir_all(&projects).expect("failed to create projects directory");

        let scope = resolve_scope(
            Some("projects"),
            Some("cloud-folder"),
            &default_config(),
            &root,
            None,
        );
        assert_eq!(scope.local_sync_directory, Some(projects));
        assert_eq!(scope.cloud_sync_directory, "/cloud-folder");
    }

    #[test]
    fn missing_local_sync_root_creation_stays_scoped_to_configured_directory() {
        let (_guard, root) = create_test_directory();
        let current_directory = root.join("current-directory");
        let home_directory = root.join("home-directory");
        let configured = root.join("nested").join("sync-root");
        fs::create_dir_all(&current_directory).expect("failed to create current directory");
        fs::create_dir_all(&home_directory).expect("failed to create home directory");

        let scope = resolve_scope(
            Some(configured.to_string_lossy().as_ref()),
            Some("/Cloud"),
            &default_config(),
            current_directory.as_path(),
            Some(home_directory.as_path()),
        );

        assert_eq!(scope.local_sync_directory, Some(configured.clone()));
        assert!(configured.exists());
        assert_ne!(scope.local_sync_directory, Some(current_directory));
        assert_ne!(scope.local_sync_directory, Some(home_directory));
    }

    #[test]
    fn sync_mode_defaults_to_two_way_and_requires_explicit_opt_in() {
        // C8-63 / C8-66: one-way modes are never inferred. Only the
        // exact configured value activates them; anything else lands on
        // the safe two-way default.
        let scope = resolve_scope(None, None, &default_config(), Path::new("/tmp"), None);
        assert_eq!(scope.sync_mode, vapor_shared::SyncMode::TwoWay);

        let pull = VaporConfig {
            sync_mode: "pull-only".to_string(),
            ..VaporConfig::default()
        };
        assert_eq!(
            resolve_scope(None, None, &pull, Path::new("/tmp"), None).sync_mode,
            vapor_shared::SyncMode::PullOnly
        );

        let push = VaporConfig {
            sync_mode: "push-only".to_string(),
            ..VaporConfig::default()
        };
        assert_eq!(
            resolve_scope(None, None, &push, Path::new("/tmp"), None).sync_mode,
            vapor_shared::SyncMode::PushOnly
        );
    }

    #[test]
    fn unknown_sync_mode_values_fall_back_to_the_safe_default() {
        for bogus in ["mirror", "Pull-Only", "pullonly", "one-way", ""] {
            let config = VaporConfig {
                sync_mode: bogus.to_string(),
                ..VaporConfig::default()
            };
            assert_eq!(
                resolve_scope(None, None, &config, Path::new("/tmp"), None).sync_mode,
                vapor_shared::SyncMode::TwoWay,
                "'{bogus}' must not activate any mode"
            );
        }
    }

    fn create_test_directory() -> (TempDir, PathBuf) {
        let guard = TempDir::new().expect("failed to create test directory");
        let root = guard.path().to_path_buf();
        (guard, root)
    }
}
