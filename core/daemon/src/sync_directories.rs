use std::env;
use std::path::{Path, PathBuf};

use vapor_shared::{SyncMode, constants, runtime_paths};

use crate::logging;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncScope {
    pub local_root: Option<PathBuf>,
    pub cloud_root: String,
    /// Sync direction for this scope. Carried on the scope so
    /// every pipeline gate (ingest, remote apply, reconcile, executor)
    /// consults the same value — no operation can bypass it.
    pub sync_mode: SyncMode,
}

/// Resolves the sync scope from `vapor.json`: the configured value, or
/// the compiled default when the key is unset or blank.
pub fn resolve_with_config(config: &vapor_shared::config::VaporConfig) -> SyncScope {
    let current_directory = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home_directory = home_directory();
    resolve_scope(config, &current_directory, home_directory.as_deref())
}

fn resolve_scope(
    config: &vapor_shared::config::VaporConfig,
    current_directory: &Path,
    home_directory: Option<&Path>,
) -> SyncScope {
    let local_raw = first_non_empty(&[
        Some(config.local_root.as_str()),
        Some(constants::filtering::DEFAULT_LOCAL_ROOT),
    ]);
    let local_root =
        local_raw.and_then(|raw| resolve_local_directory(raw, current_directory, home_directory));
    let cloud_root = resolve_cloud_directory(
        config.effective_cloud_root(),
        &config.provider,
        current_directory,
        home_directory,
    );

    SyncScope {
        local_root,
        cloud_root,
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
        logging::warning("No local root configured; daemon will remain idle", &[]);
        return None;
    };

    // A missing directory is not created here: whether to create it
    // is the root identity check's call (`root_identity.rs`), which
    // knows whether this profile synced there before.
    if path.exists() && !path.is_dir() {
        logging::warning(
            "Skipping local root because path is not a directory",
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
    let trimmed = if trimmed.is_empty() {
        constants::filtering::default_cloud_root(provider_kind)
    } else {
        trimmed
    };

    // The filesystem provider's "cloud" root is a local directory:
    // resolve it exactly like the local root (tilde expansion,
    // cwd-anchored relative paths). Blindly prefixing `/` turned
    // `~/x` into the unusable literal `/~/x`. Real cloud providers
    // keep the root-relative remote-path semantics below.
    if provider_kind.trim() == constants::provider::FILESYSTEM {
        return match resolve_path(trimmed, current_directory, home_directory) {
            Some(path) => path.to_string_lossy().into_owned(),
            // A `~` path with no home to expand against stays as
            // written; the root check reports it instead of a
            // manufactured path.
            None => trimmed.to_string(),
        };
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
pub fn filesystem_roots_overlap(local_root: &Path, cloud_root: &str) -> Option<String> {
    let local = canonicalize_for_overlap(local_root);
    let cloud = canonicalize_for_overlap(Path::new(cloud_root));
    if local == cloud {
        Some(format!(
            "localRoot and cloudRoot resolve to the same directory ({})",
            local.display()
        ))
    } else if cloud.starts_with(&local) {
        Some(format!(
            "cloudRoot {} is inside localRoot {}",
            cloud.display(),
            local.display()
        ))
    } else if local.starts_with(&cloud) {
        Some(format!(
            "localRoot {} is inside cloudRoot {}",
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
    fn defaults_to_two_folders_under_home_for_the_filesystem_provider() {
        let (_guard, home) = create_test_directory();

        let scope = resolve_scope(&default_config(), Path::new("/tmp"), Some(home.as_path()));
        assert_eq!(scope.local_root, Some(home.join("Vapor")));
        assert_eq!(scope.cloud_root, home.join("cloud/Vapor").to_string_lossy());
        assert!(
            filesystem_roots_overlap(&home.join("Vapor"), &scope.cloud_root).is_none(),
            "the two defaults must never nest"
        );
        assert!(
            !home.join("Vapor").exists(),
            "resolution names the root; the root identity check creates it"
        );
    }

    #[test]
    fn defaults_to_the_account_root_folder_for_a_remote_provider() {
        let (_guard, home) = create_test_directory();
        let mut config = default_config();
        config.provider = constants::provider::GDRIVE.to_string();

        let scope = resolve_scope(&config, Path::new("/tmp"), Some(home.as_path()));
        assert_eq!(scope.cloud_root, "/Vapor");
    }

    #[test]
    fn filesystem_cloud_directory_expands_tilde_like_the_local_root() {
        let (_guard, home) = create_test_directory();
        let config = VaporConfig {
            cloud_root: Some("~/Desktop/VaporCloud".to_string()),
            ..VaporConfig::default()
        };

        let scope = resolve_scope(&config, Path::new("/tmp"), Some(home.as_path()));

        assert_eq!(
            scope.cloud_root,
            home.join("Desktop/VaporCloud").to_string_lossy(),
            "the filesystem provider's cloud root is a local path and must expand ~"
        );
    }

    #[test]
    fn non_filesystem_providers_keep_root_relative_cloud_semantics() {
        let (_guard, home) = create_test_directory();
        let config = VaporConfig {
            provider: "gdrive".to_string(),
            cloud_root: Some("Backups/Vapor".to_string()),
            ..VaporConfig::default()
        };

        let scope = resolve_scope(&config, Path::new("/tmp"), Some(home.as_path()));

        assert_eq!(scope.cloud_root, "/Backups/Vapor");
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
    fn config_file_values_are_used() {
        let (_guard, root) = create_test_directory();
        let configured = root.join("from-config");
        let config = VaporConfig {
            local_root: configured.to_string_lossy().into_owned(),
            cloud_root: Some("/FromConfig".to_string()),
            ..VaporConfig::default()
        };

        let scope = resolve_scope(&config, Path::new("/tmp"), None);

        assert_eq!(scope.local_root, Some(configured));
        assert_eq!(scope.cloud_root, "/FromConfig");
    }

    #[test]
    fn blank_values_fall_back_to_the_defaults() {
        let config = VaporConfig {
            local_root: "   ".to_string(),
            cloud_root: Some(String::new()),
            ..VaporConfig::default()
        };

        let scope = resolve_scope(&config, Path::new("/tmp"), None);

        // No home directory to expand `~` against, so both defaults stay
        // as written rather than becoming a bogus path.
        assert_eq!(scope.local_root, None);
        assert_eq!(
            scope.cloud_root,
            constants::filtering::DEFAULT_FILESYSTEM_CLOUD_ROOT
        );
    }

    #[test]
    fn a_missing_local_directory_resolves_without_being_created() {
        let (_guard, root) = create_test_directory();
        let missing = root.join("missing");

        let config = VaporConfig {
            local_root: missing.to_string_lossy().into_owned(),
            cloud_root: Some("/Cloud".to_string()),
            provider: constants::provider::GDRIVE.to_string(),
            ..VaporConfig::default()
        };
        let scope = resolve_scope(&config, Path::new("/tmp"), None);
        assert_eq!(scope.local_root, Some(missing.clone()));
        assert!(!missing.exists());
        assert_eq!(scope.cloud_root, "/Cloud");
    }

    #[test]
    fn returns_none_when_local_sync_path_is_a_file() {
        let (_guard, root) = create_test_directory();
        let file_path = root.join("not-a-directory");
        std::fs::write(&file_path, b"x").expect("failed to create file path");

        let config = VaporConfig {
            local_root: file_path.to_string_lossy().into_owned(),
            cloud_root: Some("/Cloud".to_string()),
            provider: constants::provider::GDRIVE.to_string(),
            ..VaporConfig::default()
        };
        let scope = resolve_scope(&config, Path::new("/tmp"), None);
        assert!(scope.local_root.is_none());
        assert_eq!(scope.cloud_root, "/Cloud");
    }

    #[test]
    fn invalid_local_sync_path_does_not_fall_back_to_current_or_home_directory() {
        let (_guard, root) = create_test_directory();
        let current_directory = root.join("current-directory");
        let home_directory = root.join("home-directory");
        let file_path = root.join("not-a-directory");
        std::fs::create_dir_all(&current_directory).expect("failed to create current directory");
        std::fs::create_dir_all(&home_directory).expect("failed to create home directory");
        std::fs::write(&file_path, b"x").expect("failed to create file path");

        let config = VaporConfig {
            local_root: file_path.to_string_lossy().into_owned(),
            ..VaporConfig::default()
        };
        let scope = resolve_scope(
            &config,
            current_directory.as_path(),
            Some(home_directory.as_path()),
        );

        assert!(scope.local_root.is_none());
        assert_ne!(scope.local_root, Some(current_directory));
        assert_ne!(scope.local_root, Some(home_directory));
    }

    #[test]
    fn resolves_relative_local_path_against_current_directory() {
        let (_guard, root) = create_test_directory();
        let projects = root.join("projects");
        std::fs::create_dir_all(&projects).expect("failed to create projects directory");

        let config = VaporConfig {
            local_root: "projects".to_string(),
            cloud_root: Some("cloud-folder".to_string()),
            ..VaporConfig::default()
        };
        let scope = resolve_scope(&config, &root, None);
        assert_eq!(scope.local_root, Some(projects));
        // The default provider is filesystem-backed: its cloud root is a
        // local path and resolves against the current directory like the
        // local root does.
        assert_eq!(
            scope.cloud_root,
            root.join("cloud-folder").to_string_lossy()
        );
    }

    #[test]
    fn a_configured_local_root_resolves_to_itself_not_the_cwd_or_home() {
        let (_guard, root) = create_test_directory();
        let current_directory = root.join("current-directory");
        let home_directory = root.join("home-directory");
        let configured = root.join("nested").join("sync-root");
        std::fs::create_dir_all(&current_directory).expect("failed to create current directory");
        std::fs::create_dir_all(&home_directory).expect("failed to create home directory");

        let config = VaporConfig {
            local_root: configured.to_string_lossy().into_owned(),
            ..VaporConfig::default()
        };
        let scope = resolve_scope(
            &config,
            current_directory.as_path(),
            Some(home_directory.as_path()),
        );

        assert_eq!(scope.local_root, Some(configured.clone()));
        assert_ne!(scope.local_root, Some(current_directory));
        assert_ne!(scope.local_root, Some(home_directory));
    }

    #[test]
    fn sync_mode_defaults_to_two_way_and_requires_explicit_opt_in() {
        // One-way modes are never inferred. Only the
        // exact configured value activates them; anything else lands on
        // the safe two-way default.
        let scope = resolve_scope(&default_config(), Path::new("/tmp"), None);
        assert_eq!(scope.sync_mode, vapor_shared::SyncMode::TwoWay);

        let pull = VaporConfig {
            sync_mode: "pull-only".to_string(),
            ..VaporConfig::default()
        };
        assert_eq!(
            resolve_scope(&pull, Path::new("/tmp"), None).sync_mode,
            vapor_shared::SyncMode::PullOnly
        );

        let push = VaporConfig {
            sync_mode: "push-only".to_string(),
            ..VaporConfig::default()
        };
        assert_eq!(
            resolve_scope(&push, Path::new("/tmp"), None).sync_mode,
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
                resolve_scope(&config, Path::new("/tmp"), None).sync_mode,
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
