//! Durable profile model + override resolution.
//!
//! A profile is one sync pairing: a provider, a local root, a cloud
//! root, and a sync mode. A profile exists only when the user set its
//! two roots: a configuration that names none composes no profile, the
//! daemon syncs nothing and creates no folder, and status says so.
//! Without a `profiles` array the two top-level roots, once both are
//! set, give one implicit profile (`default`).
//!
//! Override semantics are **categorical** for the fields resolved here
//! (provider, directories, syncMode): a profile's value replaces the
//! top-level value outright. MIN-lowering resolution applies only to
//! the resource-budget groups, which are daemon-global runtime
//! inputs rather than per-profile scopes.

use std::collections::BTreeSet;

use vapor_shared::config::{VaporConfig, VaporConfigLoadResult};
use vapor_shared::constants;

use crate::logging;
use crate::sync_directories::{self, SyncScope};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedProfile {
    /// Stable filesystem-safe slug: names the per-profile state
    /// directory and namespaces secrets.
    pub id: String,
    pub name: String,
    /// Provider selection for this profile (`filesystem` /
    /// `gdrive`).
    pub provider_kind: String,
    pub scope: SyncScope,
    pub enabled: bool,
    /// Why this profile cannot run as it is written: a root that is
    /// not set, a sync mode the daemon does not know. Such a profile is
    /// listed in status as suspended with this reason, and nothing of
    /// it syncs.
    pub invalid: Option<String>,
}

/// The profiles a configuration gives, and what stops it from giving
/// the ones it means to.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProfileSet {
    pub profiles: Vec<ResolvedProfile>,
    /// What to fix in `vapor.json`, in words for the user. Empty for a
    /// healthy file, and for one that names no sync scope yet.
    pub problems: Vec<String>,
}

impl ProfileSet {
    /// Nothing is set up and nothing is wrong: a fresh install.
    pub fn is_unconfigured(&self) -> bool {
        self.profiles.is_empty() && self.problems.is_empty()
    }
}

/// Resolves the profile set from a loaded `vapor.json`. A file that
/// cannot be read, or whose `profiles` value has the wrong shape, gives
/// no profile at all: running the profiles that happen to parse, or a
/// default one in their place, would sync folders the user did not
/// choose.
pub fn resolve_profile_set(load: &VaporConfigLoadResult) -> ProfileSet {
    let problems = unusable_configuration(load);
    if !problems.is_empty() {
        return ProfileSet {
            profiles: Vec::new(),
            problems,
        };
    }
    resolve_profile_set_from(&load.config)
}

/// Why a loaded `vapor.json` names no profile at all: the file cannot
/// be read, or a key that decides the profiles has a value of the wrong
/// shape. Empty when the file can name profiles. A surface that edits
/// or removes profiles reads this first: with such a file, "the profile
/// is not there" is not known.
pub fn unusable_configuration(load: &VaporConfigLoadResult) -> Vec<String> {
    use constants::config as keys;
    if load.unreadable {
        return vec![format!(
            "vapor.json cannot be read: {}",
            load.load_issue.as_deref().unwrap_or("unknown error")
        )];
    }
    [
        keys::KEY_PROFILES,
        keys::KEY_LOCAL_ROOT,
        keys::KEY_CLOUD_ROOT,
        keys::KEY_PROVIDER,
        keys::KEY_SYNC_MODE,
    ]
    .into_iter()
    .filter(|key| load.invalid_keys.contains(key))
    .map(|key| format!("`{key}` in vapor.json has a value of the wrong shape"))
    .collect()
}

/// The profiles of `config`, valid or not. For callers that hold a
/// configuration and need the profiles it names (the CLI).
pub fn resolve_profiles(config: &VaporConfig) -> Vec<ResolvedProfile> {
    resolve_profile_set_from(config).profiles
}

/// The profile set of a configuration that parsed.
pub fn resolve_profile_set_from(config: &VaporConfig) -> ProfileSet {
    if config.profiles.is_empty() {
        return ProfileSet {
            profiles: implicit_default_profile(config).into_iter().collect(),
            problems: Vec::new(),
        };
    }

    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    let mut set = ProfileSet::default();
    for (position, profile) in config.profiles.iter().enumerate() {
        let id = profile.id.trim();
        if !is_valid_profile_id(id) {
            let problem = format!(
                "profile {} has no valid id (`{}`): lowercase letters, digits, `-` and `_`",
                position + 1,
                id
            );
            logging::error("Skipping a profile", &[("problem", problem.clone())]);
            set.problems.push(problem);
            continue;
        }
        if !seen_ids.insert(id.to_string()) {
            let problem = format!("two profiles have the id `{id}`; the second one is skipped");
            logging::error("Skipping a profile", &[("problem", problem.clone())]);
            set.problems.push(problem);
            continue;
        }

        // Categorical override resolution: the profile's value wins
        // outright; unset fields inherit the top-level configuration.
        let mut effective = config.clone();
        effective.profiles = Vec::new();
        if let Some(provider) = &profile.provider {
            effective.provider = provider.clone();
        }
        if let Some(local) = &profile.local_root {
            effective.local_root = Some(local.clone());
        }
        if let Some(cloud) = &profile.cloud_root {
            effective.cloud_root = Some(cloud.clone());
        }
        if let Some(sync_mode) = &profile.sync_mode {
            effective.sync_mode = sync_mode.clone();
        }

        set.profiles.push(ResolvedProfile {
            id: id.to_string(),
            name: profile
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| id.to_string()),
            provider_kind: effective.provider.clone(),
            invalid: scope_problem(&effective),
            scope: sync_directories::resolve_with_config(&effective),
            enabled: profile.enabled.unwrap_or(true),
        });
    }
    refuse_nested_local_roots(&mut set.profiles);
    set
}

/// Why `local_root` cannot be the local folder of one more profile next
/// to `profiles`: it lies inside the local folder of one that runs, or
/// around it. What one of the two downloads the other would upload,
/// and a mirror in one could delete through the other. The same folder
/// is another thing: two profiles may share it, and each sends it to
/// its own cloud.
pub fn nested_local_root_problem(
    profiles: &[ResolvedProfile],
    local_root: &std::path::Path,
) -> Option<String> {
    profiles
        .iter()
        .filter(|profile| profile.enabled && profile.invalid.is_none())
        .find_map(|profile| {
            let other = profile.scope.local_root.as_deref()?;
            let reason = sync_directories::local_roots_nest(other, local_root)?;
            Some(format!(
                "its local folder overlaps the one of profile `{}`: {reason}. Give the two \
                 profiles folders that are apart, or the same folder",
                profile.id
            ))
        })
}

/// The later profile of a pair whose local folders nest is the one
/// that does not run.
fn refuse_nested_local_roots(profiles: &mut [ResolvedProfile]) {
    for index in 0..profiles.len() {
        let (earlier, rest) = profiles.split_at_mut(index);
        let profile = &mut rest[0];
        if !profile.enabled || profile.invalid.is_some() {
            continue;
        }
        let Some(local_root) = profile.scope.local_root.clone() else {
            continue;
        };
        if let Some(problem) = nested_local_root_problem(earlier, &local_root) {
            logging::error(
                "A profile cannot run next to another one",
                &[
                    ("profile_id", profile.id.clone()),
                    ("problem", problem.clone()),
                ],
            );
            profile.invalid = Some(problem);
        }
    }
}

/// What stops `config`'s scope from being a profile, if anything: a
/// root that is not set, a root that is a relative path, or a sync mode
/// that is not one.
pub fn scope_problem(config: &VaporConfig) -> Option<String> {
    use constants::config as keys;
    let missing = match (
        config.configured_local_root(),
        config.configured_cloud_root(),
    ) {
        (Some(_), Some(_)) => None,
        (None, Some(_)) => Some(format!("`{}` is not set", keys::KEY_LOCAL_ROOT)),
        (Some(_), None) => Some(format!("`{}` is not set", keys::KEY_CLOUD_ROOT)),
        (None, None) => Some(format!(
            "`{}` and `{}` are not set",
            keys::KEY_LOCAL_ROOT,
            keys::KEY_CLOUD_ROOT
        )),
    };
    missing
        .or_else(|| sync_directories::unusable_root(config))
        .or_else(|| sync_directories::parse_sync_mode(&config.sync_mode).err())
}

/// The profile the top-level keys give when there is no `profiles`
/// array. `None` while neither root is set: nothing is configured.
fn implicit_default_profile(config: &VaporConfig) -> Option<ResolvedProfile> {
    if config.configured_local_root().is_none() && config.configured_cloud_root().is_none() {
        return None;
    }
    Some(ResolvedProfile {
        id: constants::profile::DEFAULT_PROFILE_ID.to_string(),
        name: constants::profile::DEFAULT_PROFILE_ID.to_string(),
        provider_kind: config.provider.clone(),
        invalid: scope_problem(config),
        scope: sync_directories::resolve_with_config(config),
        enabled: true,
    })
}

/// Profile ids name state directories and secret entries: short,
/// filesystem-safe slugs only.
pub fn is_valid_profile_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= constants::profile::MAX_PROFILE_ID_LENGTH
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Deletes what Vapor keeps about a profile: its sync state and its
/// credentials. The user's folders and the profile's trash are never
/// touched. The daemon must not be running: it holds the state open.
///
/// A named profile owns the directory `state/profiles/<id>/`. The
/// `default` profile's database sits in `state/` itself, next to files
/// that belong to no profile, so only the database and the files
/// derived from its name go.
pub fn forget_profile(
    state_directory: &std::path::Path,
    profile_id: &str,
    secret_store: Option<&dyn vapor_platform::SecretStore>,
) -> std::io::Result<()> {
    if !is_valid_profile_id(profile_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("'{profile_id}' is not a profile id"),
        ));
    }

    let database =
        vapor_shared::runtime_paths::profile_database_path_in(state_directory, profile_id);
    if profile_id == constants::profile::DEFAULT_PROFILE_ID {
        remove_database_files(&database)?;
    } else if let Some(profile_directory) = database.parent() {
        match std::fs::remove_dir_all(profile_directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    let prefix = format!("auth.{profile_id}.");
    if let Some(secret_store) = secret_store
        && let Ok(names) = secret_store.list()
    {
        for name in names {
            if name.starts_with(&prefix)
                && let Err(error) = secret_store.delete(&name)
            {
                logging::warning(
                    "Could not delete a profile's secret",
                    &[("name", name.clone()), ("error", error.to_string())],
                );
            }
        }
    }
    logging::info(
        "Deleted a profile's sync state and credentials",
        &[("profile_id", profile_id.to_string())],
    );
    Ok(())
}

/// Removes a database and every file named after it: the write-ahead
/// log, the shared-memory file, and the copies set aside after a
/// corruption or a schema change.
fn remove_database_files(database: &std::path::Path) -> std::io::Result<()> {
    let (Some(directory), Some(name)) = (
        database.parent(),
        database.file_name().and_then(|name| name.to_str()),
    ) else {
        return Ok(());
    };
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let is_database_file = entry
            .file_name()
            .to_str()
            .is_some_and(|file| file.starts_with(name));
        if is_database_file && entry.file_type()?.is_file() {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// The secret-store namespace for a profile's provider credentials
///: `auth.{profile_id}.{provider}.{item}`.
pub fn secret_key(profile_id: &str, provider: &str, item: &str) -> String {
    format!("auth.{profile_id}.{provider}.{item}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vapor_shared::SyncMode;
    use vapor_shared::config::ProfileConfig;

    /// A fixture path that is absolute on this host: `/tmp/x` names no
    /// drive on Windows, where it is a relative path.
    fn rooted(path: &str) -> String {
        if cfg!(windows) {
            format!("C:{path}")
        } else {
            path.to_string()
        }
    }

    fn base_config() -> VaporConfig {
        VaporConfig {
            local_root: Some(rooted("/tmp/vapor-top-local")),
            cloud_root: Some(rooted("/tmp/vapor-top-cloud")),
            sync_mode: "two-way".to_string(),
            ..VaporConfig::default()
        }
    }

    #[test]
    fn empty_profiles_resolve_to_the_implicit_default() {
        let profiles = resolve_profiles(&base_config());
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "default");
        assert!(profiles[0].enabled);
        assert_eq!(profiles[0].provider_kind, "filesystem");
        assert_eq!(profiles[0].scope.sync_mode, SyncMode::TwoWay);
    }

    #[test]
    fn profile_overrides_win_outright_and_unset_fields_inherit() {
        let mut config = base_config();
        config.profiles = vec![
            ProfileConfig {
                id: "mirror".to_string(),
                name: Some("Cloud Mirror".to_string()),
                local_root: Some(rooted("/tmp/vapor-mirror")),
                sync_mode: Some("pull-only".to_string()),
                ..ProfileConfig::default()
            },
            ProfileConfig {
                id: "docs".to_string(),
                ..ProfileConfig::default()
            },
        ];

        let profiles = resolve_profiles(&config);
        assert_eq!(profiles.len(), 2);
        let mirror = &profiles[0];
        assert_eq!(mirror.name, "Cloud Mirror");
        assert_eq!(mirror.scope.sync_mode, SyncMode::PullOnly, "override wins");
        let mirror_local = mirror
            .scope
            .local_root
            .as_deref()
            .expect("override resolves a local root");
        assert!(mirror_local.is_absolute());
        assert!(
            mirror_local.ends_with("tmp/vapor-mirror"),
            "override wins: {mirror_local:?}"
        );
        // Same drive-letter caveat for the filesystem provider's cloud root,
        // which resolves like a local path.
        let mirror_cloud = std::path::Path::new(mirror.scope.cloud_root.as_str());
        assert!(mirror_cloud.is_absolute());
        assert!(
            mirror_cloud.ends_with("tmp/vapor-top-cloud"),
            "unset fields inherit the top level: {mirror_cloud:?}"
        );
        let docs = &profiles[1];
        assert_eq!(docs.scope.sync_mode, SyncMode::TwoWay, "inherits default");
        assert_eq!(docs.name, "docs", "name defaults to the id");
    }

    #[test]
    fn invalid_and_duplicate_profile_ids_are_skipped_and_named() {
        let mut config = base_config();
        config.profiles = vec![
            ProfileConfig {
                id: "".to_string(),
                ..ProfileConfig::default()
            },
            ProfileConfig {
                id: "Not A Slug!".to_string(),
                ..ProfileConfig::default()
            },
            ProfileConfig {
                id: "good".to_string(),
                ..ProfileConfig::default()
            },
            ProfileConfig {
                id: "good".to_string(),
                ..ProfileConfig::default()
            },
        ];

        let set = resolve_profile_set_from(&config);
        assert_eq!(set.profiles.len(), 1);
        assert_eq!(set.profiles[0].id, "good");
        assert_eq!(set.problems.len(), 3, "{:?}", set.problems);
        assert!(set.problems[1].contains("Not A Slug!"));
        assert!(set.problems[2].contains("`good`"));
    }

    #[test]
    fn a_configuration_with_no_valid_profile_gives_none_and_never_a_default() {
        let mut config = base_config();
        config.profiles = vec![ProfileConfig {
            id: "***".to_string(),
            ..ProfileConfig::default()
        }];
        let set = resolve_profile_set_from(&config);
        assert!(
            set.profiles.is_empty(),
            "the top-level roots must not run in place of the profiles the user wrote"
        );
        assert_eq!(set.problems.len(), 1);
        assert!(!set.is_unconfigured());
    }

    #[test]
    fn a_profile_whose_local_folder_nests_with_an_earlier_one_does_not_run() {
        let profile = |id: &str, local: &str, enabled: bool| ProfileConfig {
            id: id.to_string(),
            local_root: Some(rooted(local)),
            cloud_root: Some(rooted(&format!("/tmp/vapor-nest-cloud/{id}"))),
            enabled: Some(enabled),
            ..ProfileConfig::default()
        };
        let config = VaporConfig {
            profiles: vec![
                profile("docs", "/tmp/vapor-nest/docs", true),
                profile("inside", "/tmp/vapor-nest/docs/work", true),
                profile("around", "/tmp/vapor-nest", true),
                profile("same", "/tmp/vapor-nest/docs", true),
                profile("apart", "/tmp/vapor-nest-other", true),
                profile("alike", "/tmp/vapor-nest/docs-old", true),
            ],
            ..VaporConfig::default()
        };
        let profiles = resolve_profiles(&config);
        let problem = |id: &str| {
            profiles
                .iter()
                .find(|profile| profile.id == id)
                .expect("the profile is listed")
                .invalid
                .clone()
        };

        assert_eq!(problem("docs"), None, "the earlier profile of a pair runs");
        for later in ["inside", "around"] {
            let reason = problem(later).expect("a nested folder is refused");
            assert!(reason.contains("profile `docs`"), "{reason}");
            assert!(reason.contains("is inside"), "{reason}");
        }
        assert_eq!(problem("same"), None, "two profiles may share one folder");
        assert_eq!(problem("apart"), None);
        assert_eq!(
            problem("alike"),
            None,
            "a name that starts the same way is another folder, and a refused profile holds none"
        );

        // A profile that is turned off holds no folder.
        let config = VaporConfig {
            profiles: vec![
                profile("off", "/tmp/vapor-nest/docs", false),
                profile("inside", "/tmp/vapor-nest/docs/work", true),
            ],
            ..VaporConfig::default()
        };
        assert_eq!(resolve_profiles(&config)[1].invalid, None);
    }

    #[test]
    fn nothing_set_is_unconfigured_and_one_root_set_names_the_other() {
        let fresh = resolve_profile_set_from(&VaporConfig::default());
        assert!(fresh.is_unconfigured(), "a fresh install has no profile");

        let half = VaporConfig {
            local_root: Some(rooted("/tmp/vapor-top-local")),
            ..VaporConfig::default()
        };
        let set = resolve_profile_set_from(&half);
        assert_eq!(set.profiles.len(), 1);
        assert_eq!(
            set.profiles[0].invalid.as_deref(),
            Some("`cloudRoot` is not set")
        );

        // A profile entry needs both roots too, its own or the top level's.
        let partial = VaporConfig {
            profiles: vec![ProfileConfig {
                id: "work".to_string(),
                local_root: Some(rooted("/tmp/vapor-work")),
                ..ProfileConfig::default()
            }],
            ..VaporConfig::default()
        };
        let set = resolve_profile_set_from(&partial);
        assert_eq!(
            set.profiles[0].invalid.as_deref(),
            Some("`cloudRoot` is not set")
        );
    }

    #[test]
    fn an_unknown_sync_mode_stops_the_profile_and_names_the_value() {
        let mut config = base_config();
        config.profiles = vec![
            ProfileConfig {
                id: "sloppy".to_string(),
                sync_mode: Some("Pull-Only".to_string()),
                ..ProfileConfig::default()
            },
            ProfileConfig {
                id: "fine".to_string(),
                sync_mode: Some("pull-only".to_string()),
                ..ProfileConfig::default()
            },
        ];
        let profiles = resolve_profiles(&config);
        let reason = profiles[0].invalid.as_deref().expect("the typo is refused");
        assert!(reason.contains("\"Pull-Only\""), "{reason}");
        assert!(reason.contains("pull-only"), "{reason}");
        assert_eq!(profiles[1].invalid, None);
        assert_eq!(profiles[1].scope.sync_mode, SyncMode::PullOnly);

        // The same for the top-level value of the implicit profile.
        let top = VaporConfig {
            sync_mode: "mirror".to_string(),
            ..base_config()
        };
        assert!(resolve_profiles(&top)[0].invalid.is_some());
    }

    #[test]
    fn a_file_that_cannot_be_read_or_has_a_broken_scope_key_gives_no_profile() {
        let temp = tempfile::TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");

        std::fs::write(&path, "{ not json").expect("seed");
        let set = resolve_profile_set(&vapor_shared::config::load_from(&path));
        assert!(set.profiles.is_empty());
        assert!(set.problems[0].starts_with("vapor.json cannot be read"));

        // One bad field makes the whole `profiles` value unreadable;
        // the top-level roots must not run instead.
        std::fs::write(
            &path,
            format!(
                r#"{{ "localRoot": "{}", "cloudRoot": "{}",
                 "profiles": [{{ "id": "work", "enabled": "yes" }}] }}"#,
                rooted("/tmp/a"),
                rooted("/tmp/b")
            ),
        )
        .expect("seed");
        let set = resolve_profile_set(&vapor_shared::config::load_from(&path));
        assert!(set.profiles.is_empty(), "{set:?}");
        assert_eq!(
            set.problems,
            vec!["`profiles` in vapor.json has a value of the wrong shape".to_string()]
        );

        std::fs::write(
            &path,
            format!(
                r#"{{ "localRoot": "{}", "cloudRoot": "{}" }}"#,
                rooted("/tmp/a"),
                rooted("/tmp/b")
            ),
        )
        .expect("seed");
        let set = resolve_profile_set(&vapor_shared::config::load_from(&path));
        assert_eq!(set.profiles.len(), 1);
        assert_eq!(set.profiles[0].invalid, None);
        assert!(set.problems.is_empty());
    }

    #[test]
    fn forgetting_a_profile_takes_its_state_and_secrets_and_leaves_the_others() {
        use vapor_platform::SecretStore;
        let temp = tempfile::TempDir::new().expect("temp");
        let state = temp.path();
        let store = vapor_platform::InMemorySecretStore::new();
        for profile in ["work", "home", "default"] {
            store
                .set(&secret_key(profile, "gdrive", "token"), "secret")
                .expect("seed");
            let database = vapor_shared::runtime_paths::profile_database_path_in(state, profile);
            std::fs::create_dir_all(database.parent().expect("parent")).expect("dir");
            for suffix in ["", "-wal", ".corrupt-1000"] {
                let mut name = database.clone().into_os_string();
                name.push(suffix);
                std::fs::write(name, b"x").expect("seed");
            }
        }
        std::fs::write(state.join("lifecycle.json"), b"{}").expect("seed");
        let has_state = |profile: &str| {
            vapor_shared::runtime_paths::profile_database_path_in(state, profile).exists()
        };
        let signed_in = |profile: &str| store.get(&secret_key(profile, "gdrive", "token")).is_ok();

        forget_profile(state, "work", Some(&store)).expect("forget");
        assert!(!has_state("work") && !signed_in("work"));
        assert!(!state.join("profiles").join("work").exists());
        assert!(has_state("home") && signed_in("home"));
        assert!(has_state("default") && signed_in("default"));

        // The default profile's database sits next to files that belong
        // to no profile: only what is named after the database goes.
        forget_profile(state, "default", Some(&store)).expect("forget");
        assert!(!has_state("default") && !signed_in("default"));
        let mut left: Vec<String> = std::fs::read_dir(state)
            .expect("read")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        left.sort();
        assert_eq!(left, vec!["lifecycle.json", "profiles"]);
        assert!(has_state("home") && signed_in("home"));

        assert!(forget_profile(state, "Not Valid!", Some(&store)).is_err());
    }

    #[test]
    fn secret_keys_are_namespaced_by_profile_and_provider() {
        assert_eq!(
            secret_key("work", "gdrive", "token"),
            "auth.work.gdrive.token"
        );
    }
}
