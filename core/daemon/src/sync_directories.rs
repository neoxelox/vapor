use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use vapor_shared::{SyncMode, constants, runtime_paths};

use crate::logging;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncScope {
    pub local_sync_directory: Option<PathBuf>,
    pub cloud_sync_directory: String,
    /// Sync direction for this scope. Carried on the scope so
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
    resolve_scope_with_env(config, true, true)
}

/// Per-profile scope resolution. A process-wide
/// `VAPOR_LOCAL/CLOUD_SYNC_DIRECTORY` env var must NOT override a
/// directory the profile set explicitly: doing so would collapse every
/// profile onto one root, and for a one-way mirror profile that means
/// strict-mirroring a cloud root over a directory the user never chose.
/// The env layer therefore applies only to a field the profile did not
/// set; when an env var is present but overridden by an explicit profile
/// field, it is ignored with a loud warning rather than silently winning.
pub fn resolve_profile_scope(
    config: &vapor_shared::config::VaporConfig,
    profile_sets_local: bool,
    profile_sets_cloud: bool,
) -> SyncScope {
    if profile_sets_local && env::var_os(constants::env::VAPOR_LOCAL_SYNC_DIRECTORY).is_some() {
        logging::warning(
            "Ignoring VAPOR_LOCAL_SYNC_DIRECTORY for a profile that sets localSyncDirectory explicitly",
            &[],
        );
    }
    if profile_sets_cloud && env::var_os(constants::env::VAPOR_CLOUD_SYNC_DIRECTORY).is_some() {
        logging::warning(
            "Ignoring VAPOR_CLOUD_SYNC_DIRECTORY for a profile that sets cloudSyncDirectory explicitly",
            &[],
        );
    }
    resolve_scope_with_env(config, !profile_sets_local, !profile_sets_cloud)
}

fn resolve_scope_with_env(
    config: &vapor_shared::config::VaporConfig,
    allow_env_local: bool,
    allow_env_cloud: bool,
) -> SyncScope {
    let env_local = if allow_env_local {
        env::var(constants::env::VAPOR_LOCAL_SYNC_DIRECTORY).ok()
    } else {
        None
    };
    let env_cloud = if allow_env_cloud {
        env::var(constants::env::VAPOR_CLOUD_SYNC_DIRECTORY).ok()
    } else {
        None
    };
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
        &config.provider,
        current_directory,
        home_directory,
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
/// mode the user did not spell exactly (one-way is explicit
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

fn resolve_cloud_directory(
    raw: &str,
    provider_kind: &str,
    current_directory: &Path,
    home_directory: Option<&Path>,
) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return constants::filtering::DEFAULT_CLOUD_SYNC_DIRECTORY.to_string();
    }

    // The filesystem provider's "cloud" root is a local directory:
    // resolve it exactly like the local root (tilde expansion,
    // cwd-anchored relative paths). Blindly prefixing `/` turned
    // `~/x` into the unusable literal `/~/x`. Real cloud providers
    // keep the root-relative remote-path semantics below.
    if provider_kind.trim() == constants::provider::FILESYSTEM
        && let Some(path) = resolve_path(trimmed, current_directory, home_directory)
    {
        return path.to_string_lossy().into_owned();
    }

    if trimmed.starts_with('/') {
        return trimmed.to_string();
    }

    format!("/{trimmed}")
}

/// Overlap guard for filesystem-backed profiles. The provider's
/// "cloud" root is a local directory; if it equals — or nests inside
/// or around — the watched local root, the engine ingests its own
/// provider writes as fresh local changes (self-sustaining churn, and
/// potentially destructive under a pull-only strict mirror). Returns
/// the actionable refusal reason when the roots overlap.
pub fn filesystem_roots_overlap(
    local_sync_directory: &Path,
    cloud_sync_directory: &str,
) -> Option<String> {
    let local = canonicalize_for_overlap(local_sync_directory);
    let cloud = canonicalize_for_overlap(Path::new(cloud_sync_directory));
    if local == cloud {
        Some(format!(
            "localSyncDirectory and cloudSyncDirectory resolve to the same directory ({})",
            local.display()
        ))
    } else if cloud.starts_with(&local) {
        Some(format!(
            "cloudSyncDirectory {} is inside localSyncDirectory {}",
            cloud.display(),
            local.display()
        ))
    } else if local.starts_with(&cloud) {
        Some(format!(
            "localSyncDirectory {} is inside cloudSyncDirectory {}",
            local.display(),
            cloud.display()
        ))
    } else {
        None
    }
}

/// Canonicalizes the deepest existing ancestor and re-appends the
/// missing tail (the cloud root may not exist yet at composition
/// time); falls back to the input when nothing canonicalizes. Keeps
/// symlinked and real spellings from evading the overlap check.
fn canonicalize_for_overlap(path: &Path) -> PathBuf {
    let mut missing_tail: Vec<std::ffi::OsString> = Vec::new();
    let mut ancestor = path;
    loop {
        if let Ok(canonical) = vapor_shared::paths::canonicalize(ancestor) {
            let mut resolved = canonical;
            for name in missing_tail.iter().rev() {
                resolved.push(name);
            }
            return resolved;
        }
        match (ancestor.parent(), ancestor.file_name()) {
            (Some(parent), Some(name)) => {
                missing_tail.push(name.to_os_string());
                ancestor = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
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
    fn filesystem_cloud_directory_expands_tilde_like_the_local_root() {
        let (_guard, home) = create_test_directory();
        let config = VaporConfig {
            cloud_sync_directory: "~/Desktop/VaporCloud".to_string(),
            ..VaporConfig::default()
        };

        let scope = resolve_scope(None, None, &config, Path::new("/tmp"), Some(home.as_path()));

        assert_eq!(
            scope.cloud_sync_directory,
            home.join("Desktop/VaporCloud").to_string_lossy(),
            "the filesystem provider's cloud root is a local path and must expand ~"
        );
    }

    #[test]
    fn non_filesystem_providers_keep_root_relative_cloud_semantics() {
        let (_guard, home) = create_test_directory();
        let config = VaporConfig {
            provider: "gdrive".to_string(),
            cloud_sync_directory: "Backups/Vapor".to_string(),
            ..VaporConfig::default()
        };

        let scope = resolve_scope(None, None, &config, Path::new("/tmp"), Some(home.as_path()));

        assert_eq!(scope.cloud_sync_directory, "/Backups/Vapor");
    }

    #[test]
    fn overlapping_filesystem_roots_are_detected_in_both_nesting_directions() {
        let temp = TempDir::new().expect("temp dir");
        let base = vapor_shared::paths::canonicalize(temp.path()).expect("canonical base");
        let local = base.join("local");
        std::fs::create_dir_all(&local).expect("local");

        let same = filesystem_roots_overlap(&local, &local.to_string_lossy());
        assert!(same.is_some(), "equal roots must be refused");

        let nested_cloud = local.join("cloud");
        assert!(
            filesystem_roots_overlap(&local, &nested_cloud.to_string_lossy()).is_some(),
            "a cloud root inside the local root must be refused"
        );

        let outer_cloud = base.clone();
        assert!(
            filesystem_roots_overlap(&local, &outer_cloud.to_string_lossy()).is_some(),
            "a cloud root containing the local root must be refused"
        );

        let sibling = base.join("sibling-cloud");
        assert!(
            filesystem_roots_overlap(&local, &sibling.to_string_lossy()).is_none(),
            "disjoint sibling roots are fine"
        );
    }

    #[cfg(unix)]
    #[test]
    fn overlap_detection_sees_through_symlinked_spellings() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new().expect("temp dir");
        let base = vapor_shared::paths::canonicalize(temp.path()).expect("canonical base");
        let local = base.join("local");
        std::fs::create_dir_all(&local).expect("local");
        symlink(&local, base.join("local-alias")).expect("symlink");

        let via_alias = base.join("local-alias/cloud");
        assert!(
            filesystem_roots_overlap(&local, &via_alias.to_string_lossy()).is_some(),
            "a symlinked spelling must not evade the overlap guard"
        );
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
        // The default provider is filesystem-backed: its cloud root is a
        // local path and resolves against the current directory like the
        // local root does.
        assert_eq!(
            scope.cloud_sync_directory,
            root.join("cloud-folder").to_string_lossy()
        );
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
        // One-way modes are never inferred. Only the
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
