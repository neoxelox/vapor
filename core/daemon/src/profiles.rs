//! Durable profile model + override resolution.
//!
//! A profile is one sync pairing: a provider, a local root, a cloud
//! root, and a sync mode. Without a `profiles` array the daemon runs
//! one implicit profile (`default`) assembled from the top-level
//! configuration — the pre-profile behavior, byte-for-byte.
//!
//! Override semantics are **categorical** for the fields resolved here
//! (provider, directories, syncMode): a profile's value replaces the
//! top-level value outright. MIN-lowering resolution applies only to
//! the resource-budget groups, which are daemon-global runtime
//! inputs rather than per-profile scopes.

use std::collections::BTreeSet;

use vapor_shared::config::VaporConfig;
use vapor_shared::{SyncMode, constants};

use crate::logging;
use crate::sync_directories::{self, SyncScope};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedProfile {
    /// Stable filesystem-safe slug: names the per-profile state
    /// directory and namespaces secrets.
    pub id: String,
    pub display_name: String,
    /// Provider selection for this profile (`filesystem` /
    /// `gdrive`).
    pub provider_kind: String,
    pub scope: SyncScope,
    pub enabled: bool,
}

/// Resolves the configured profile set. Invalid entries (missing or
/// duplicate ids, malformed values) are skipped with a loud log rather
/// than aborting the daemon — one broken profile must not take down the
/// others (blast-radius discipline).
pub fn resolve_profiles(config: &VaporConfig) -> Vec<ResolvedProfile> {
    if config.profiles.is_empty() {
        return vec![implicit_default_profile(config)];
    }

    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    let mut resolved = Vec::new();
    for profile in &config.profiles {
        let id = profile.id.trim();
        if !is_valid_profile_id(id) {
            logging::error(
                "Skipping profile with missing or invalid id",
                &[(
                    "id",
                    if id.is_empty() {
                        "<empty>".to_string()
                    } else {
                        id.to_string()
                    },
                )],
            );
            continue;
        }
        if !seen_ids.insert(id.to_string()) {
            logging::error(
                "Skipping profile with duplicate id",
                &[("id", id.to_string())],
            );
            continue;
        }

        // Categorical override resolution: the profile's value wins
        // outright; unset fields inherit the top-level configuration.
        let mut effective = config.clone();
        effective.profiles = Vec::new();
        if let Some(provider) = &profile.provider {
            effective.provider = provider.clone();
        }
        if let Some(local) = &profile.local_sync_directory {
            effective.local_sync_directory = local.clone();
        }
        if let Some(cloud) = &profile.cloud_sync_directory {
            effective.cloud_sync_directory = cloud.clone();
        }
        if let Some(sync_mode) = &profile.sync_mode {
            effective.sync_mode = sync_mode.clone();
        }

        // One-way modes are explicit opt-in per profile: a
        // malformed per-profile value falls back to the *top-level*
        // resolution path inside resolve_with_config, which itself
        // falls back to two-way with a warning.
        if let Some(sync_mode) = &profile.sync_mode
            && SyncMode::from_config_value(sync_mode).is_none()
        {
            logging::warning(
                "Profile has an unknown syncMode; using the safe two-way default",
                &[
                    ("profile_id", id.to_string()),
                    ("configured_sync_mode", sync_mode.clone()),
                ],
            );
        }

        resolved.push(ResolvedProfile {
            id: id.to_string(),
            display_name: profile
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| id.to_string()),
            provider_kind: effective.provider.clone(),
            scope: sync_directories::resolve_with_config(&effective),
            enabled: profile.enabled.unwrap_or(true),
        });
    }

    if resolved.is_empty() {
        logging::error(
            "No valid profiles resolved from configuration; running the implicit default profile",
            &[],
        );
        return vec![implicit_default_profile(config)];
    }
    resolved
}

fn implicit_default_profile(config: &VaporConfig) -> ResolvedProfile {
    ResolvedProfile {
        id: constants::profile::DEFAULT_PROFILE_ID.to_string(),
        display_name: constants::profile::DEFAULT_PROFILE_ID.to_string(),
        provider_kind: config.provider.clone(),
        scope: sync_directories::resolve_with_config(config),
        enabled: true,
    }
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

/// Safe profile disconnect/delete primitive: removes a
/// profile's durable state directory and its namespaced secrets while
/// leaving every other profile untouched. Callers (the profiles CLI /
/// app surface) must stop the daemon first — this function is
/// the storage-side primitive, not the orchestration.
///
/// The implicit `default` profile is refused: its state lives on the
/// legacy shared paths and removing it is a daemon-reset, not a
/// profile delete.
pub fn purge_profile_state(
    profile_id: &str,
    secret_store: &dyn vapor_platform::SecretStore,
) -> std::io::Result<()> {
    if profile_id == constants::profile::DEFAULT_PROFILE_ID || !is_valid_profile_id(profile_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("profile '{profile_id}' cannot be purged"),
        ));
    }

    let state_directory = vapor_shared::runtime_paths::state_directory()
        .join("profiles")
        .join(profile_id);
    match std::fs::remove_dir_all(&state_directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let prefix = format!("auth.{profile_id}.");
    if let Ok(names) = secret_store.list() {
        for name in names {
            if name.starts_with(&prefix)
                && let Err(error) = secret_store.delete(&name)
            {
                logging::warning(
                    "Could not delete profile secret during purge",
                    &[("name", name.clone()), ("error", error.to_string())],
                );
            }
        }
    }
    logging::info(
        "Purged profile state",
        &[("profile_id", profile_id.to_string())],
    );
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
    use vapor_shared::config::ProfileConfig;

    fn base_config() -> VaporConfig {
        VaporConfig {
            local_sync_directory: "/tmp/vapor-top-local".to_string(),
            cloud_sync_directory: "/tmp/vapor-top-cloud".to_string(),
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
                local_sync_directory: Some("/tmp/vapor-mirror".to_string()),
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
        assert_eq!(mirror.display_name, "Cloud Mirror");
        assert_eq!(mirror.scope.sync_mode, SyncMode::PullOnly, "override wins");
        // Windows absolutizes "/tmp/…" against the current drive, so
        // assert resolution shape rather than a Unix-literal path.
        let mirror_local = mirror
            .scope
            .local_sync_directory
            .as_deref()
            .expect("override resolves a local root");
        assert!(mirror_local.is_absolute());
        assert!(
            mirror_local.ends_with("tmp/vapor-mirror"),
            "override wins: {mirror_local:?}"
        );
        assert_eq!(
            mirror.scope.cloud_sync_directory, "/tmp/vapor-top-cloud",
            "unset fields inherit the top level"
        );
        let docs = &profiles[1];
        assert_eq!(docs.scope.sync_mode, SyncMode::TwoWay, "inherits default");
        assert_eq!(docs.display_name, "docs", "name defaults to the id");
    }

    #[test]
    fn invalid_and_duplicate_profile_ids_are_skipped() {
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

        let profiles = resolve_profiles(&config);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "good");
    }

    #[test]
    fn all_invalid_profiles_fall_back_to_the_implicit_default() {
        let mut config = base_config();
        config.profiles = vec![ProfileConfig {
            id: "***".to_string(),
            ..ProfileConfig::default()
        }];
        let profiles = resolve_profiles(&config);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "default");
    }

    #[test]
    fn one_way_mode_on_a_profile_requires_the_exact_spelling() {
        let mut config = base_config();
        config.profiles = vec![ProfileConfig {
            id: "sloppy".to_string(),
            sync_mode: Some("Pull-Only".to_string()),
            ..ProfileConfig::default()
        }];
        let profiles = resolve_profiles(&config);
        assert_eq!(
            profiles[0].scope.sync_mode,
            SyncMode::TwoWay,
            "a typo must never activate a destructive mode (C8-63)"
        );
    }

    #[test]
    fn purge_refuses_the_default_profile_and_clears_namespaced_secrets() {
        use vapor_platform::SecretStore;
        let store = vapor_platform::InMemorySecretStore::new();
        store
            .set(&secret_key("work", "gdrive", "token"), "secret-a")
            .expect("seed");
        store
            .set(&secret_key("home", "gdrive", "token"), "secret-b")
            .expect("seed");

        purge_profile_state("work", &store).expect("purge");
        assert!(
            store.get(&secret_key("work", "gdrive", "token")).is_err(),
            "purged profile secrets must be gone"
        );
        assert!(
            store.get(&secret_key("home", "gdrive", "token")).is_ok(),
            "other profiles' secrets must survive"
        );

        assert!(purge_profile_state("default", &store).is_err());
        assert!(purge_profile_state("Not Valid!", &store).is_err());
    }

    #[test]
    fn secret_keys_are_namespaced_by_profile_and_provider() {
        assert_eq!(
            secret_key("work", "gdrive", "token"),
            "auth.work.gdrive.token"
        );
    }
}
