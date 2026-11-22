use std::path::{Path, PathBuf};

use vapor_shared::{SyncMode, constants, runtime_paths};

use crate::logging;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncScope {
    pub local_root: Option<PathBuf>,
    pub cloud_root: String,
    /// Sync direction for this scope. Carried on the scope so
    /// every pipeline gate (ingest, remote apply, reconcile, executor)
    /// consults the same value, no operation can bypass it.
    pub sync_mode: SyncMode,
}

/// Resolves the sync scope from `vapor.json`. A root the user did not
/// set stays unset: there is no compiled default, and the caller
/// composes no profile for such a scope.
pub fn resolve_with_config(config: &vapor_shared::config::VaporConfig) -> SyncScope {
    let home_directory = home_directory();
    resolve_scope(config, home_directory.as_deref())
}

fn resolve_scope(
    config: &vapor_shared::config::VaporConfig,
    home_directory: Option<&Path>,
) -> SyncScope {
    let local_root = config
        .configured_local_root()
        .and_then(|raw| resolve_local_directory(raw, home_directory));
    let cloud_root = config
        .configured_cloud_root()
        .map(|raw| resolve_cloud_directory(raw, &config.provider, home_directory))
        .unwrap_or_default();

    SyncScope {
        local_root,
        cloud_root,
        // A value this function cannot read is the caller's to refuse
        // (`parse_sync_mode`); two-way is the one mode that never
        // deletes or overwrites to converge.
        sync_mode: parse_sync_mode(&config.sync_mode).unwrap_or(SyncMode::TwoWay),
    }
}

/// Reads a `syncMode` value. An unknown value is an error that names
/// it, never a fallback: a typo of `pull-only` that ran as two-way
/// would send local edits and deletions to the cloud folder the user
/// meant to protect.
pub fn parse_sync_mode(raw: &str) -> Result<SyncMode, String> {
    SyncMode::from_config_value(raw).ok_or_else(|| {
        format!(
            "unknown syncMode {raw:?}; use one of {}",
            constants::sync_mode::ALL.join(", ")
        )
    })
}

/// Why a root that is set cannot be used as it is written, if it
/// cannot. A relative path has no fixed meaning for a daemon: its
/// working directory is `/` under launchd and the terminal's directory
/// under `vapor run`, so the same `vapor.json` would sync two different
/// folders.
pub fn unusable_root(config: &vapor_shared::config::VaporConfig) -> Option<String> {
    use constants::config as keys;
    let problem = |key: &str, raw: &str| {
        format!("`{key}` is a relative path ({raw}); write the full path, or start it with `~/`")
    };
    if let Some(local) = config.configured_local_root()
        && is_relative(local)
    {
        return Some(problem(keys::KEY_LOCAL_ROOT, local));
    }
    // Only the filesystem provider's cloud root is a path on this
    // device; another provider's is a path inside the cloud.
    if config.provider.trim() == constants::provider::FILESYSTEM
        && let Some(cloud) = config.configured_cloud_root()
        && is_relative(cloud)
    {
        return Some(problem(keys::KEY_CLOUD_ROOT, cloud));
    }
    None
}

fn is_relative(raw: &str) -> bool {
    let trimmed = raw.trim();
    !(trimmed == "~" || trimmed.starts_with("~/") || Path::new(trimmed).is_absolute())
}

fn resolve_local_directory(raw: &str, home_directory: Option<&Path>) -> Option<PathBuf> {
    let Some(path) = resolve_path(raw, home_directory) else {
        logging::warning(
            "The local root cannot be resolved to a full path; the profile does not run",
            &[("local_root", raw.to_string())],
        );
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
    home_directory: Option<&Path>,
) -> String {
    let trimmed = raw.trim();

    // The filesystem provider's "cloud" root is a local directory:
    // resolve it exactly like the local root (tilde expansion). A real
    // cloud provider keeps the root-relative remote-path semantics
    // below.
    if provider_kind.trim() == constants::provider::FILESYSTEM {
        return match resolve_path(trimmed, home_directory) {
            Some(path) => path.to_string_lossy().into_owned(),
            // A path that does not resolve (relative, or `~` with no
            // home to expand against) stays as written; the profile
            // check reports it instead of a manufactured path.
            None => trimmed.to_string(),
        };
    }

    if trimmed.starts_with('/') {
        return trimmed.to_string();
    }

    format!("/{trimmed}")
}

/// Overlap guard for filesystem-backed profiles. The provider's
/// "cloud" root is a local directory; if it equals, or nests inside
/// or around: the watched local root, the engine ingests its own
/// provider writes as fresh local changes (self-sustaining churn, and
/// potentially destructive under a pull-only strict mirror). Returns
/// the actionable refusal reason when the roots overlap.
pub fn filesystem_roots_overlap(local_root: &Path, cloud_root: &str) -> Option<String> {
    roots_overlap(
        local_root,
        Path::new(cloud_root),
        &crate::name_collision::folds_case_at,
    )
}

/// Whether the local roots of two profiles nest: one lies inside the
/// other. Returns the reason to refuse the pair. The same folder for
/// both is no nesting: two profiles may share a local root, and each
/// sends it to its own cloud.
pub fn local_roots_nest(first: &Path, second: &Path) -> Option<String> {
    roots_nest(first, second, &crate::name_collision::folds_case_at)
}

fn roots_nest(first: &Path, second: &Path, folds_case: &dyn Fn(&Path) -> bool) -> Option<String> {
    let first = OverlapPath::of(first, folds_case);
    let second = OverlapPath::of(second, folds_case);
    if first.compared == second.compared {
        None
    } else if second.compared.starts_with(&first.compared) {
        Some(format!(
            "{} is inside {}",
            second.shown.display(),
            first.shown.display()
        ))
    } else if first.compared.starts_with(&second.compared) {
        Some(format!(
            "{} is inside {}",
            first.shown.display(),
            second.shown.display()
        ))
    } else {
        None
    }
}

/// `folds_case` says whether the volume of a folder that exists folds
/// names; it is asked about the deepest ancestor that exists.
fn roots_overlap(
    local_root: &Path,
    cloud_root: &Path,
    folds_case: &dyn Fn(&Path) -> bool,
) -> Option<String> {
    let local = OverlapPath::of(local_root, folds_case);
    let cloud = OverlapPath::of(cloud_root, folds_case);
    if local.compared == cloud.compared {
        Some(format!(
            "localRoot and cloudRoot resolve to the same directory ({})",
            local.shown.display()
        ))
    } else if cloud.compared.starts_with(&local.compared) {
        Some(format!(
            "cloudRoot {} is inside localRoot {}",
            cloud.shown.display(),
            local.shown.display()
        ))
    } else if local.compared.starts_with(&cloud.compared) {
        Some(format!(
            "localRoot {} is inside cloudRoot {}",
            local.shown.display(),
            cloud.shown.display()
        ))
    } else {
        None
    }
}

/// A root as the overlap check sees it: the deepest ancestor that
/// exists, canonical (so a symbolic link and its target compare
/// equal), with the names that do not exist yet appended.
struct OverlapPath {
    /// The path as messages show it.
    shown: PathBuf,
    /// The path as it is compared. A name that exists has its true
    /// spelling from the filesystem. A name that does not exist yet has
    /// none: on a volume that ignores case, `Sync` and `sync` will be
    /// one folder, so those names are compared folded.
    compared: PathBuf,
}

impl OverlapPath {
    fn of(path: &Path, folds_case: &dyn Fn(&Path) -> bool) -> Self {
        let mut missing_tail: Vec<std::ffi::OsString> = Vec::new();
        let mut ancestor = path;
        loop {
            if let Ok(canonical) = vapor_shared::paths::canonicalize(ancestor) {
                let fold_missing_names = !missing_tail.is_empty() && folds_case(&canonical);
                let mut shown = canonical.clone();
                let mut compared = canonical;
                for name in missing_tail.iter().rev() {
                    shown.push(name);
                    match name.to_str() {
                        Some(text) if fold_missing_names => {
                            compared.push(vapor_shared::paths::fold_name(text));
                        }
                        _ => compared.push(name),
                    }
                }
                return Self { shown, compared };
            }
            match (ancestor.parent(), ancestor.file_name()) {
                (Some(parent), Some(name)) => {
                    missing_tail.push(name.to_os_string());
                    ancestor = parent;
                }
                _ => {
                    // A `..` after a folder that does not exist yet has
                    // no name to carry over. Once the folder is made,
                    // `..` leads back out of it, so the path is read
                    // that way.
                    let plain = without_dot_components(path);
                    if plain != path {
                        return Self::of(&plain, folds_case);
                    }
                    return Self {
                        shown: path.to_path_buf(),
                        compared: path.to_path_buf(),
                    };
                }
            }
        }
    }
}

/// `path` with each `.` dropped and each `..` taken with the name
/// before it, by the text alone.
fn without_dot_components(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut plain = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                plain.pop();
            }
            other => plain.push(other.as_os_str()),
        }
    }
    plain
}

/// The full path a root value names: `~` expands against the home
/// directory, and an absolute path stays. `None` for a blank value, a
/// `~` with no home directory, and a relative path, which is never
/// resolved against the working directory (see [`unusable_root`]).
fn resolve_path(raw: &str, home_directory: Option<&Path>) -> Option<PathBuf> {
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
    path.is_absolute().then_some(path)
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
    fn roots_that_are_not_set_stay_unset() {
        let (_guard, home) = create_test_directory();
        for provider in [constants::provider::FILESYSTEM, constants::provider::GDRIVE] {
            let mut config = default_config();
            config.provider = provider.to_string();
            let scope = resolve_scope(&config, Some(home.as_path()));
            assert_eq!(scope.local_root, None, "{provider}");
            assert_eq!(scope.cloud_root, "", "{provider}");
        }
        assert!(
            !home.join("Vapor").exists() && !home.join("cloud").exists(),
            "nothing is created for a root nobody chose"
        );
    }

    #[test]
    fn the_roots_that_setup_offers_never_nest() {
        let (_guard, home) = create_test_directory();
        let config = VaporConfig {
            local_root: Some(constants::filtering::DEFAULT_LOCAL_ROOT.to_string()),
            cloud_root: Some(
                constants::filtering::default_cloud_root(constants::provider::FILESYSTEM)
                    .to_string(),
            ),
            ..default_config()
        };
        let scope = resolve_scope(&config, Some(home.as_path()));
        assert_eq!(scope.local_root, Some(home.join("Vapor")));
        assert_eq!(scope.cloud_root, home.join("cloud/Vapor").to_string_lossy());
        assert!(filesystem_roots_overlap(&home.join("Vapor"), &scope.cloud_root).is_none());
        assert!(
            !home.join("Vapor").exists(),
            "resolution names the root; the root identity check creates it"
        );
    }

    #[test]
    fn filesystem_cloud_directory_expands_tilde_like_the_local_root() {
        let (_guard, home) = create_test_directory();
        let config = VaporConfig {
            cloud_root: Some("~/Desktop/VaporCloud".to_string()),
            ..VaporConfig::default()
        };

        let scope = resolve_scope(&config, Some(home.as_path()));

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

        let scope = resolve_scope(&config, Some(home.as_path()));

        assert_eq!(scope.cloud_root, "/Backups/Vapor");
    }

    #[test]
    fn local_roots_nest_when_one_is_inside_the_other_and_not_when_they_are_equal_or_apart() {
        let temp = TempDir::new().expect("temp dir");
        let base = vapor_shared::paths::canonicalize(temp.path()).expect("canonical base");
        let docs = base.join("Docs");
        std::fs::create_dir_all(&docs).expect("docs");

        assert!(local_roots_nest(&docs, &docs).is_none(), "one folder");
        assert!(
            local_roots_nest(&docs, &base.join("Docs2")).is_none(),
            "a name that only starts the same is another folder"
        );
        let inside = local_roots_nest(&docs, &docs.join("work")).expect("inside");
        assert!(inside.contains("is inside"), "{inside}");
        assert!(local_roots_nest(&docs.join("work"), &docs).is_some());
        // A `..` after a folder that does not exist yet still leads to
        // the folder it names.
        assert!(
            local_roots_nest(&docs, &base.join("new/../Docs/work")).is_some(),
            "the path names a folder inside Docs"
        );
        assert!(local_roots_nest(&docs, &base.join("new/../Other")).is_none());
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
            local_root: Some(configured.to_string_lossy().into_owned()),
            cloud_root: Some("/FromConfig".to_string()),
            ..VaporConfig::default()
        };

        let scope = resolve_scope(&config, None);

        assert_eq!(scope.local_root, Some(configured));
        assert_eq!(scope.cloud_root, "/FromConfig");
    }

    #[test]
    fn blank_values_are_unset() {
        let config = VaporConfig {
            local_root: Some("   ".to_string()),
            cloud_root: Some(String::new()),
            ..VaporConfig::default()
        };

        let scope = resolve_scope(&config, None);
        assert_eq!(scope.local_root, None);
        assert_eq!(scope.cloud_root, "");
    }

    #[test]
    fn a_missing_local_directory_resolves_without_being_created() {
        let (_guard, root) = create_test_directory();
        let missing = root.join("missing");

        let config = VaporConfig {
            local_root: Some(missing.to_string_lossy().into_owned()),
            cloud_root: Some("/Cloud".to_string()),
            provider: constants::provider::GDRIVE.to_string(),
            ..VaporConfig::default()
        };
        let scope = resolve_scope(&config, None);
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
            local_root: Some(file_path.to_string_lossy().into_owned()),
            cloud_root: Some("/Cloud".to_string()),
            provider: constants::provider::GDRIVE.to_string(),
            ..VaporConfig::default()
        };
        let scope = resolve_scope(&config, None);
        assert!(scope.local_root.is_none());
        assert_eq!(scope.cloud_root, "/Cloud");
    }

    #[test]
    fn a_local_root_that_is_a_file_does_not_fall_back_to_the_home_directory() {
        let (_guard, root) = create_test_directory();
        let home_directory = root.join("home-directory");
        let file_path = root.join("not-a-directory");
        std::fs::create_dir_all(&home_directory).expect("failed to create home directory");
        std::fs::write(&file_path, b"x").expect("failed to create file path");

        let config = VaporConfig {
            local_root: Some(file_path.to_string_lossy().into_owned()),
            ..VaporConfig::default()
        };
        let scope = resolve_scope(&config, Some(home_directory.as_path()));
        assert!(scope.local_root.is_none());
    }

    #[test]
    fn a_relative_root_is_refused_and_never_resolved_against_the_working_directory() {
        let (_guard, root) = create_test_directory();
        // The working directory of the test process holds no `projects`
        // folder; a root resolved against it would name one there.
        let config = VaporConfig {
            local_root: Some("projects".to_string()),
            cloud_root: Some(root.join("cloud").to_string_lossy().into_owned()),
            ..VaporConfig::default()
        };
        let problem = unusable_root(&config).expect("a relative local root is a problem");
        assert!(
            problem.contains("`localRoot` is a relative path (projects)"),
            "{problem}"
        );
        assert_eq!(
            resolve_scope(&config, Some(root.as_path())).local_root,
            None
        );

        // The filesystem provider's cloud root is a path on this device.
        let config = VaporConfig {
            local_root: Some(root.join("local").to_string_lossy().into_owned()),
            cloud_root: Some("cloud-folder".to_string()),
            ..VaporConfig::default()
        };
        let problem = unusable_root(&config).expect("a relative cloud root is a problem");
        assert!(
            problem.contains("`cloudRoot` is a relative path"),
            "{problem}"
        );

        // Another provider's cloud root is a path inside the cloud; the
        // home directory and a full path are fine for the local root.
        for (local, cloud, provider) in [
            ("~/Photos", "Backups/Vapor", constants::provider::GDRIVE),
            ("~", "~/cloud/Vapor", constants::provider::FILESYSTEM),
        ] {
            let config = VaporConfig {
                provider: provider.to_string(),
                local_root: Some(local.to_string()),
                cloud_root: Some(cloud.to_string()),
                ..VaporConfig::default()
            };
            assert_eq!(unusable_root(&config), None, "{local} / {cloud}");
        }
    }

    #[test]
    fn roots_that_do_not_exist_yet_and_differ_only_by_case_overlap_where_case_is_ignored() {
        let temp = TempDir::new().expect("temp dir");
        let base = vapor_shared::paths::canonicalize(temp.path()).expect("canonical base");
        let local = base.join("Sync");
        let cloud = base.join("sync");
        // On a volume that ignores case the two names will be one
        // folder; on one that does not, they are two.
        assert!(roots_overlap(&local, &cloud, &|_| true).is_some());
        assert!(roots_overlap(&local, &cloud.join("cloud"), &|_| true).is_some());
        assert!(roots_overlap(&local, &cloud, &|_| false).is_none());
        // A name that exists keeps its true spelling: folding the names
        // that are missing never joins two folders that both exist.
        assert!(roots_overlap(&local, &base.join("Sync-cloud"), &|_| true).is_none());
    }

    #[test]
    fn sync_mode_defaults_to_two_way_and_requires_explicit_opt_in() {
        // One-way modes are never inferred. Only the
        // exact configured value activates them; anything else lands on
        // the safe two-way default.
        let scope = resolve_scope(&default_config(), None);
        assert_eq!(scope.sync_mode, vapor_shared::SyncMode::TwoWay);

        let pull = VaporConfig {
            sync_mode: "pull-only".to_string(),
            ..VaporConfig::default()
        };
        assert_eq!(
            resolve_scope(&pull, None).sync_mode,
            vapor_shared::SyncMode::PullOnly
        );

        let push = VaporConfig {
            sync_mode: "push-only".to_string(),
            ..VaporConfig::default()
        };
        assert_eq!(
            resolve_scope(&push, None).sync_mode,
            vapor_shared::SyncMode::PushOnly
        );
    }

    #[test]
    fn an_unknown_sync_mode_is_an_error_that_names_the_value() {
        for bogus in ["mirror", "Pull-Only", "pullonly", "one-way", ""] {
            let error = parse_sync_mode(bogus).expect_err("unknown");
            assert!(error.contains(&format!("{bogus:?}")), "{error}");
            assert!(error.contains("pull-only"), "{error}");
            // The scope itself never carries a destructive mode for a
            // value nobody can read.
            let config = VaporConfig {
                sync_mode: bogus.to_string(),
                ..VaporConfig::default()
            };
            assert_eq!(
                resolve_scope(&config, None).sync_mode,
                vapor_shared::SyncMode::TwoWay,
                "'{bogus}' must not activate any mode"
            );
        }
        assert_eq!(
            parse_sync_mode("push-only"),
            Ok(vapor_shared::SyncMode::PushOnly)
        );
    }

    fn create_test_directory() -> (TempDir, PathBuf) {
        let guard = TempDir::new().expect("failed to create test directory");
        let root = guard.path().to_path_buf();
        (guard, root)
    }
}
