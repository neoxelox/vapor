//! Typed loader for the persisted user configuration at
//! `<vapor_dir>/vapor.json`.
//!
//! Every surface shares this file: the macOS app's
//! `VaporConfigurationStore` and the `vapor` CLI write it; the daemon
//! reads it here at startup and again when it changes. A setting comes
//! from the file or from its compiled default. No environment variable
//! overrides a key: the `VAPOR_*` variables choose where the runtime
//! directory is and how a development run behaves, so the service
//! definition only needs to carry `VAPOR_DIR` (and optionally
//! `VAPOR_ENV`).
//!
//! Parsing is tolerant the same way the Swift store is: a missing file
//! yields pure defaults, missing fields yield per-field defaults, and an
//! unreadable/corrupt file yields defaults plus a `load_issue` the caller
//! can surface: the file itself is never modified or overwritten by the
//! loader.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::constants;

/// Fully-resolved configuration with every field defaulted. Field names
/// follow Rust conventions; the on-disk keys are the camelCase names in
/// [`constants::config`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaporConfig {
    pub auto_launch: bool,
    pub use_gitignore: bool,
    pub use_vaporignore: bool,
    /// The local root, or `None` while the user has not set one. There
    /// is no compiled default: Vapor syncs only folders the user chose,
    /// so a configuration that names none composes no profile.
    pub local_root: Option<String>,
    /// The cloud root, or `None` while the user has not set one.
    pub cloud_root: Option<String>,
    /// Ignore rules in `.gitignore` syntax, one rule per entry, applied
    /// before the ignore files found under the root.
    pub pre_ignore_rules: Vec<String>,
    /// Ignore rules applied after the ignore files; a `!rule` here wins
    /// over everything.
    pub post_ignore_rules: Vec<String>,
    pub language: String,
    pub timeline_limit: i64,
    /// Provider selection: `filesystem` (default pre-GA) or
    /// `gdrive`. When `filesystem` is selected,
    /// `cloud_root` is reinterpreted as an absolute local
    /// directory that plays the cloud role.
    pub provider: String,
    /// Sync direction selector: `two-way` (default),
    /// `pull-only`, `push-only`. One-way values are strict mirrors and
    /// destructive to the subordinate side; they only activate through
    /// an explicit, enum-validated set, see
    /// `docs/architecture/sync-modes.md`.
    pub sync_mode: String,
    /// Sync profiles. Empty means one implicit profile
    /// (`default`) assembled from the top-level fields above. Each
    /// entry overrides the profile-capable fields outright; unset
    /// fields inherit the top-level values.
    pub profiles: Vec<ProfileConfig>,
    /// Hard user ceilings on daemon device impact. Out-of-range values
    /// are clamped into `1..=100` when the daemon resolves its effective
    /// budget (`resource_budget.rs`, with a logged warning), not at
    /// config load, so a non-daemon consumer of `load_from` sees the raw
    /// values.
    pub resource_limits: ResourceLimitsConfig,
    /// Idle-boost group. `*Percent` ceilings below their matching
    /// `resourceLimits` ceiling are clamped up at daemon budget-resolve
    /// time (same place as `resource_limits`), not at config load.
    pub idle_boost: IdleBoostConfig,
    /// Trash group: where a file Vapor removes on this device goes.
    pub trash: TrashConfig,
}

/// The `trash` config group.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashConfig {
    #[serde(default = "default_trash_enabled")]
    pub enabled: bool,
    #[serde(default = "default_trash_retention_days")]
    pub retention_days: u32,
    #[serde(default = "default_trash_use_system")]
    pub use_system: bool,
}

fn default_trash_enabled() -> bool {
    constants::trash::DEFAULT_ENABLED
}

fn default_trash_retention_days() -> u32 {
    constants::trash::DEFAULT_RETENTION_DAYS
}

fn default_trash_use_system() -> bool {
    constants::trash::DEFAULT_USE_SYSTEM
}

impl Default for TrashConfig {
    fn default() -> Self {
        Self {
            enabled: default_trash_enabled(),
            retention_days: default_trash_retention_days(),
            use_system: default_trash_use_system(),
        }
    }
}

/// The `resourceLimits` config group.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLimitsConfig {
    #[serde(default = "default_cpu_percent")]
    pub cpu_percent: u8,
    #[serde(default = "default_memory_percent")]
    pub memory_percent: u8,
    #[serde(default = "default_bandwidth_percent")]
    pub bandwidth_percent: u8,
    /// Optional hard ceiling on concurrent uploads and on concurrent
    /// downloads (each direction separately). `None` means automatic:
    /// the throttle ladder derives its ceiling from the machine's core
    /// count. Clamped into `1..=16` at daemon budget-resolve time.
    #[serde(default)]
    pub concurrent_transfers: Option<u8>,
}

fn default_cpu_percent() -> u8 {
    constants::resource_limits::DEFAULT_CPU_PERCENT
}
fn default_memory_percent() -> u8 {
    constants::resource_limits::DEFAULT_MEMORY_PERCENT
}
fn default_bandwidth_percent() -> u8 {
    constants::resource_limits::DEFAULT_BANDWIDTH_PERCENT
}

impl Default for ResourceLimitsConfig {
    fn default() -> Self {
        Self {
            cpu_percent: default_cpu_percent(),
            memory_percent: default_memory_percent(),
            bandwidth_percent: default_bandwidth_percent(),
            concurrent_transfers: None,
        }
    }
}

/// The `idleBoost` config group.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdleBoostConfig {
    #[serde(default = "default_boost_enabled")]
    pub enabled: bool,
    #[serde(default = "default_min_idle_seconds")]
    pub min_idle_seconds: u64,
    #[serde(default = "default_boost_cpu")]
    pub cpu_percent: u8,
    #[serde(default = "default_boost_memory")]
    pub memory_percent: u8,
    #[serde(default = "default_boost_bandwidth")]
    pub bandwidth_percent: u8,
    #[serde(default = "default_headroom_cpu")]
    pub headroom_cpu_percent: u8,
    #[serde(default = "default_ramp_up")]
    pub ramp_up_seconds: u64,
    #[serde(default = "default_ramp_down")]
    pub ramp_down_seconds: u64,
}

fn default_boost_enabled() -> bool {
    constants::idle_boost::DEFAULT_ENABLED
}
fn default_min_idle_seconds() -> u64 {
    constants::idle_boost::DEFAULT_MIN_IDLE_SECONDS
}
fn default_boost_cpu() -> u8 {
    constants::idle_boost::DEFAULT_CPU_PERCENT
}
fn default_boost_memory() -> u8 {
    constants::idle_boost::DEFAULT_MEMORY_PERCENT
}
fn default_boost_bandwidth() -> u8 {
    constants::idle_boost::DEFAULT_BANDWIDTH_PERCENT
}
fn default_headroom_cpu() -> u8 {
    constants::idle_boost::DEFAULT_HEADROOM_CPU_PERCENT
}
fn default_ramp_up() -> u64 {
    constants::idle_boost::DEFAULT_RAMP_UP_SECONDS
}
fn default_ramp_down() -> u64 {
    constants::idle_boost::DEFAULT_RAMP_DOWN_SECONDS
}

impl Default for IdleBoostConfig {
    fn default() -> Self {
        Self {
            enabled: default_boost_enabled(),
            min_idle_seconds: default_min_idle_seconds(),
            cpu_percent: default_boost_cpu(),
            memory_percent: default_boost_memory(),
            bandwidth_percent: default_boost_bandwidth(),
            headroom_cpu_percent: default_headroom_cpu(),
            ramp_up_seconds: default_ramp_up(),
            ramp_down_seconds: default_ramp_down(),
        }
    }
}

/// Per-profile `resourceLimits`. Every field is optional so a profile
/// tightens only what it names; the daemon MIN-lowers each named value
/// against the top-level group.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResourceLimitsOverride {
    pub cpu_percent: Option<u8>,
    pub memory_percent: Option<u8>,
    pub bandwidth_percent: Option<u8>,
    pub concurrent_transfers: Option<u8>,
}

/// Per-profile `idleBoost`. Every field is optional; the daemon merges
/// each named value in the direction that makes boost more cautious:
/// `enabled: false` wins daemon-wide, the three `*Percent` ceilings,
/// `headroomCpuPercent` and `rampDownSeconds` are MIN-lowered,
/// `minIdleSeconds` and `rampUpSeconds` are MAX-raised.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IdleBoostOverride {
    pub enabled: Option<bool>,
    pub min_idle_seconds: Option<u64>,
    pub cpu_percent: Option<u8>,
    pub memory_percent: Option<u8>,
    pub bandwidth_percent: Option<u8>,
    pub headroom_cpu_percent: Option<u8>,
    pub ramp_up_seconds: Option<u64>,
    pub ramp_down_seconds: Option<u64>,
}

/// One entry of the `profiles` array. Every field except `id` is
/// optional on the wire; unset fields inherit the top-level values. The
/// scalar fields override outright; the two resource groups merge per
/// field as their types describe.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileConfig {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub local_root: Option<String>,
    #[serde(default)]
    pub cloud_root: Option<String>,
    #[serde(default)]
    pub sync_mode: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Optional per-profile ceilings; each named value is MIN-lowered
    /// against the top-level group, so a profile can only tighten.
    #[serde(default)]
    pub resource_limits: Option<ResourceLimitsOverride>,
    /// Optional per-profile idle-boost override; see
    /// [`IdleBoostOverride`] for the merge direction of each field.
    #[serde(default)]
    pub idle_boost: Option<IdleBoostOverride>,
}

impl VaporConfig {
    /// The local root the user set, trimmed; `None` when unset or blank.
    pub fn configured_local_root(&self) -> Option<&str> {
        non_blank(self.local_root.as_deref())
    }

    /// The cloud root the user set, trimmed; `None` when unset or blank.
    pub fn configured_cloud_root(&self) -> Option<&str> {
        non_blank(self.cloud_root.as_deref())
    }
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

impl Default for VaporConfig {
    fn default() -> Self {
        Self {
            auto_launch: constants::config::DEFAULT_AUTO_LAUNCH,
            use_gitignore: constants::config::DEFAULT_USE_GITIGNORE,
            use_vaporignore: constants::config::DEFAULT_USE_VAPORIGNORE,
            local_root: None,
            cloud_root: None,
            pre_ignore_rules: default_pre_ignore_rules(),
            post_ignore_rules: Vec::new(),
            language: constants::config::DEFAULT_LANGUAGE.to_string(),
            timeline_limit: constants::config::DEFAULT_TIMELINE_LIMIT,
            provider: constants::provider::DEFAULT.to_string(),
            sync_mode: constants::sync_mode::DEFAULT.to_string(),
            profiles: Vec::new(),
            resource_limits: ResourceLimitsConfig::default(),
            idle_boost: IdleBoostConfig::default(),
            trash: TrashConfig::default(),
        }
    }
}

pub fn default_pre_ignore_rules() -> Vec<String> {
    constants::filtering::DEFAULT_PRE_IGNORE_RULES
        .iter()
        .map(|rule| (*rule).to_string())
        .collect()
}

/// Result of a configuration load. `load_issue` is `Some` when the file
/// could not be read or parsed, or when some of its keys are invalid or
/// unknown; the caller surfaces it (the daemon logs it; the app shows a
/// banner) without touching the on-disk file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VaporConfigLoadResult {
    pub config: VaporConfig,
    pub load_issue: Option<String>,
    /// Top-level keys whose value had the wrong shape; `config` holds
    /// their defaults.
    pub invalid_keys: Vec<&'static str>,
    /// The file exists but is not a readable JSON object, so `config` is
    /// the compiled defaults through and through.
    pub unreadable: bool,
}

impl VaporConfigLoadResult {
    fn unreadable(issue: String) -> Self {
        Self {
            load_issue: Some(issue),
            unreadable: true,
            ..Self::default()
        }
    }
}

/// Loads `vapor.json` from the current `vapor_dir`.
pub fn load_default() -> VaporConfigLoadResult {
    let path =
        crate::runtime_paths::vapor_directory().join(constants::runtime::CONFIGURATION_FILE_NAME);
    load_from(&path)
}

/// Loads a configuration file from an explicit path. A missing file is
/// the defaults with no issue. A file that is there and empty is the
/// defaults with a `load_issue`: nothing writes an empty `vapor.json`,
/// so one that exists lost its contents (an interrupted write, a full
/// disk). An unreadable or unparsable file is the defaults with a
/// `load_issue` saying why, and `unreadable` set.
pub fn load_from(path: &Path) -> VaporConfigLoadResult {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return VaporConfigLoadResult::default();
        }
        Err(error) => {
            return VaporConfigLoadResult::unreadable(format!(
                "failed to read {}: {error}",
                path.display()
            ));
        }
    };

    if contents.trim().is_empty() {
        return VaporConfigLoadResult {
            load_issue: Some(format!(
                "{} is empty: every setting and every profile it held is gone",
                path.display()
            )),
            ..VaporConfigLoadResult::default()
        };
    }

    match serde_json::from_str::<serde_json::Value>(&contents) {
        Ok(serde_json::Value::Object(map)) => config_from_object(&map),
        // Valid JSON but not an object: the file shape is wrong.
        Ok(_) => {
            VaporConfigLoadResult::unreadable(format!("{} is not a JSON object", path.display()))
        }
        Err(error) => VaporConfigLoadResult::unreadable(format!(
            "failed to parse {}: {error}",
            path.display()
        )),
    }
}

/// The fields of a group value that `vapor.json` does not define, as
/// dotted paths (`idleBoost.boostCpuPercent`,
/// `profiles[0].localSyncDirectory`). Serde ignores unknown fields, so
/// without this a renamed or misspelled field would silently fall back
/// to its default.
pub fn unknown_nested_keys(key: &str, value: &serde_json::Value) -> Vec<String> {
    use constants::config as keys;
    fn unknown_in(prefix: &str, value: &serde_json::Value, known: &[&str], out: &mut Vec<String>) {
        if let Some(object) = value.as_object() {
            for field in object.keys() {
                if !known.contains(&field.as_str()) {
                    out.push(format!("{prefix}.{field}"));
                }
            }
        }
    }
    let mut unknown = Vec::new();
    match key {
        keys::KEY_RESOURCE_LIMITS => unknown_in(
            key,
            value,
            constants::resource_limits::ALL_KEYS,
            &mut unknown,
        ),
        keys::KEY_IDLE_BOOST => {
            unknown_in(key, value, constants::idle_boost::ALL_KEYS, &mut unknown)
        }
        keys::KEY_TRASH => unknown_in(key, value, constants::trash::ALL_KEYS, &mut unknown),
        keys::KEY_PROFILES => {
            for (index, profile) in value.as_array().into_iter().flatten().enumerate() {
                let prefix = format!("{key}[{index}]");
                unknown_in(&prefix, profile, constants::profile::ALL_KEYS, &mut unknown);
                for (group, known) in [
                    (
                        constants::profile::KEY_RESOURCE_LIMITS,
                        constants::resource_limits::ALL_KEYS,
                    ),
                    (
                        constants::profile::KEY_IDLE_BOOST,
                        constants::idle_boost::ALL_KEYS,
                    ),
                ] {
                    if let Some(nested) = profile.get(group) {
                        unknown_in(&format!("{prefix}.{group}"), nested, known, &mut unknown);
                    }
                }
            }
        }
        _ => {}
    }
    unknown
}

impl VaporConfig {
    /// Takes the value of one top-level `key` from `other`. The live
    /// reload uses it to keep the applied value of a key whose new value
    /// is invalid, rather than dropping that key to its default.
    pub fn take_key_from(&mut self, other: &VaporConfig, key: &str) {
        use constants::config as keys;
        match key {
            keys::KEY_AUTO_LAUNCH => self.auto_launch = other.auto_launch,
            keys::KEY_USE_GITIGNORE => self.use_gitignore = other.use_gitignore,
            keys::KEY_USE_VAPORIGNORE => self.use_vaporignore = other.use_vaporignore,
            keys::KEY_LOCAL_ROOT => self.local_root = other.local_root.clone(),
            keys::KEY_CLOUD_ROOT => self.cloud_root = other.cloud_root.clone(),
            keys::KEY_PRE_IGNORE_RULES => self.pre_ignore_rules = other.pre_ignore_rules.clone(),
            keys::KEY_POST_IGNORE_RULES => self.post_ignore_rules = other.post_ignore_rules.clone(),
            keys::KEY_LANGUAGE => self.language = other.language.clone(),
            keys::KEY_TIMELINE_LIMIT => self.timeline_limit = other.timeline_limit,
            keys::KEY_PROVIDER => self.provider = other.provider.clone(),
            keys::KEY_SYNC_MODE => self.sync_mode = other.sync_mode.clone(),
            keys::KEY_PROFILES => self.profiles = other.profiles.clone(),
            keys::KEY_RESOURCE_LIMITS => self.resource_limits = other.resource_limits.clone(),
            keys::KEY_IDLE_BOOST => self.idle_boost = other.idle_boost.clone(),
            keys::KEY_TRASH => self.trash = other.trash.clone(),
            _ => {}
        }
    }
}

/// The configuration a parsed `vapor.json` document gives, with the
/// same checks as a load from disk. For a caller that edits the
/// document and has to know what the edit gives before it writes it.
pub fn load_from_document(
    document: &serde_json::Map<String, serde_json::Value>,
) -> VaporConfigLoadResult {
    config_from_object(document)
}

/// Per-field-tolerant decode. Each known field is decoded independently:
/// one malformed value (a type mismatch, an out-of-range number) reverts
/// only that field to its default and is noted in `load_issue`, instead
/// of discarding the user's entire configuration. Unknown keys, top-level
/// or inside a group, are surfaced the same way rather than silently
/// dropped. Absent or `null` fields fill in defaults (partial files stay
/// valid).
fn config_from_object(map: &serde_json::Map<String, serde_json::Value>) -> VaporConfigLoadResult {
    use constants::config as keys;

    fn field<T: serde::de::DeserializeOwned>(
        map: &serde_json::Map<String, serde_json::Value>,
        key: &'static str,
        default: T,
        issues: &mut Vec<String>,
        invalid: &mut Vec<&'static str>,
    ) -> T {
        match map.get(key) {
            None | Some(serde_json::Value::Null) => default,
            Some(value) => match serde_json::from_value::<T>(value.clone()) {
                Ok(parsed) => parsed,
                Err(error) => {
                    issues.push(format!("ignoring invalid `{key}`: {error}"));
                    invalid.push(key);
                    default
                }
            },
        }
    }

    let defaults = VaporConfig::default();
    let mut issues: Vec<String> = Vec::new();
    let mut invalid: Vec<&'static str> = Vec::new();
    let (i, v) = (&mut issues, &mut invalid);
    let config = VaporConfig {
        auto_launch: field(map, keys::KEY_AUTO_LAUNCH, defaults.auto_launch, i, v),
        use_gitignore: field(map, keys::KEY_USE_GITIGNORE, defaults.use_gitignore, i, v),
        use_vaporignore: field(
            map,
            keys::KEY_USE_VAPORIGNORE,
            defaults.use_vaporignore,
            i,
            v,
        ),
        local_root: field(map, keys::KEY_LOCAL_ROOT, defaults.local_root, i, v),
        cloud_root: field(map, keys::KEY_CLOUD_ROOT, defaults.cloud_root, i, v),
        pre_ignore_rules: field(
            map,
            keys::KEY_PRE_IGNORE_RULES,
            defaults.pre_ignore_rules,
            i,
            v,
        ),
        post_ignore_rules: field(
            map,
            keys::KEY_POST_IGNORE_RULES,
            defaults.post_ignore_rules,
            i,
            v,
        ),
        language: field(map, keys::KEY_LANGUAGE, defaults.language, i, v),
        timeline_limit: field(map, keys::KEY_TIMELINE_LIMIT, defaults.timeline_limit, i, v),
        provider: field(map, keys::KEY_PROVIDER, defaults.provider, i, v),
        sync_mode: field(map, keys::KEY_SYNC_MODE, defaults.sync_mode, i, v),
        profiles: field(map, keys::KEY_PROFILES, defaults.profiles, i, v),
        resource_limits: field(
            map,
            keys::KEY_RESOURCE_LIMITS,
            defaults.resource_limits,
            i,
            v,
        ),
        idle_boost: field(map, keys::KEY_IDLE_BOOST, defaults.idle_boost, i, v),
        trash: field(map, keys::KEY_TRASH, defaults.trash, i, v),
    };

    for (key, value) in map {
        if !keys::ALL_KEYS.contains(&key.as_str()) {
            issues.push(format!("unrecognized config key `{key}`"));
            continue;
        }
        for nested in unknown_nested_keys(key, value) {
            issues.push(format!("unrecognized config key `{nested}`"));
        }
    }

    VaporConfigLoadResult {
        config,
        load_issue: (!issues.is_empty()).then(|| issues.join("; ")),
        invalid_keys: invalid,
        unreadable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn trash_group_and_transfer_ceiling_parse_with_partial_values() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("vapor.json");
        std::fs::write(
            &path,
            r#"{
                "resourceLimits": { "concurrentTransfers": 2 },
                "trash": { "retentionDays": 7 }
            }"#,
        )
        .expect("write config");

        let result = load_from(&path);
        assert!(result.load_issue.is_none(), "{:?}", result.load_issue);
        assert_eq!(result.config.trash.retention_days, 7);
        // Unset group members keep their defaults.
        assert!(result.config.trash.enabled);
        assert!(!result.config.trash.use_system);
        assert_eq!(result.config.resource_limits.concurrent_transfers, Some(2));
        // Absent means automatic.
        assert_eq!(
            VaporConfig::default().resource_limits.concurrent_transfers,
            None
        );
    }

    #[test]
    fn missing_file_loads_pure_defaults_without_issue() {
        let temp = TempDir::new().expect("temp dir");
        let result = load_from(&temp.path().join("vapor.json"));
        assert_eq!(result.config, VaporConfig::default());
        assert!(result.load_issue.is_none());
    }

    #[test]
    fn partial_file_fills_missing_fields_with_defaults() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("vapor.json");
        std::fs::write(
            &path,
            r#"{ "localRoot": "~/Projects", "useGitignore": false }"#,
        )
        .expect("seed config");

        let result = load_from(&path);
        assert!(result.load_issue.is_none());
        assert_eq!(result.config.configured_local_root(), Some("~/Projects"));
        assert!(!result.config.use_gitignore);
        assert!(result.config.use_vaporignore);
        // A root the user did not set has no default: nothing syncs
        // into a folder nobody chose.
        assert_eq!(result.config.cloud_root, None);
        assert_eq!(result.config.configured_cloud_root(), None);
        let mut blank = result.config.clone();
        blank.cloud_root = Some("  ".to_string());
        assert_eq!(blank.configured_cloud_root(), None);
        blank.cloud_root = Some(" /Work ".to_string());
        assert_eq!(blank.configured_cloud_root(), Some("/Work"));
        assert_eq!(VaporConfig::default().configured_local_root(), None);
    }

    #[test]
    fn unknown_fields_are_tolerated_but_surfaced() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("vapor.json");
        std::fs::write(&path, r#"{ "futureKey": [1, 2], "autoLaunch": false }"#)
            .expect("seed config");

        let result = load_from(&path);
        // Forward-compat: the known field still applies...
        assert!(!result.config.auto_launch);
        // ...but the unrecognized key is reported (a typo like `profles`
        // must not be dropped silently).
        let issue = result.load_issue.expect("unknown key surfaced");
        assert!(issue.contains("futureKey"), "issue: {issue}");
    }

    #[test]
    fn one_malformed_field_reverts_only_that_field_not_the_whole_config() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("vapor.json");
        // `timelineLimit` has the wrong type; `localRoot` is valid.
        std::fs::write(
            &path,
            r#"{ "timelineLimit": "oops", "localRoot": "~/Keep" }"#,
        )
        .expect("seed config");

        let result = load_from(&path);
        // The valid field is preserved (not discarded to defaults)...
        assert_eq!(result.config.configured_local_root(), Some("~/Keep"));
        // ...the bad field falls back to its default...
        assert_eq!(
            result.config.timeline_limit,
            VaporConfig::default().timeline_limit
        );
        // ...and the problem is surfaced.
        let issue = result.load_issue.expect("bad field surfaced");
        assert!(issue.contains("timelineLimit"), "issue: {issue}");
    }

    #[test]
    fn corrupt_file_yields_defaults_with_issue_and_is_preserved() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("vapor.json");
        std::fs::write(&path, "{ not json").expect("seed corrupt config");

        let result = load_from(&path);
        assert_eq!(result.config, VaporConfig::default());
        assert!(result.load_issue.is_some());
        assert_eq!(
            std::fs::read_to_string(&path).expect("file preserved"),
            "{ not json"
        );
    }

    #[test]
    fn a_file_that_is_there_and_empty_loads_the_defaults_and_says_so() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("vapor.json");
        std::fs::write(&path, "  \n").expect("seed empty config");

        let result = load_from(&path);
        assert_eq!(result.config, VaporConfig::default());
        assert!(
            result
                .load_issue
                .as_deref()
                .is_some_and(|issue| issue.contains("is empty")),
            "an interrupted write must not pass as a fresh install: {:?}",
            result.load_issue
        );
        // The next write repairs it, so it does not count as unreadable.
        assert!(!result.unreadable);
    }

    #[test]
    fn default_pre_ignore_rules_leave_repositories_and_dotenv_files_in_sync() {
        let rules = default_pre_ignore_rules();
        let has = |rule: &str| rules.iter().any(|candidate| candidate == rule);
        assert!(has("node_modules/"));
        assert!(has("target/"));
        assert!(has("__pycache__/"));
        assert!(
            !rules.iter().any(|rule| rule.contains(".git/")),
            "repositories sync whole by default (owner decision)"
        );
        assert!(
            !rules.iter().any(|rule| rule.contains(".env")),
            "dotenv files sync by default (owner decision)"
        );
    }

    #[test]
    fn null_is_absent_and_unknown_group_fields_are_reported() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("vapor.json");
        std::fs::write(
            &path,
            r#"{
                "preIgnoreRules": null,
                "idleBoost": { "boostCpuPercent": 15, "cpuPercent": 40 },
                "profiles": [{ "id": "work", "localSyncDirectory": "~/Work" }]
            }"#,
        )
        .expect("write config");

        let result = load_from(&path);
        assert_eq!(result.config.pre_ignore_rules, default_pre_ignore_rules());
        assert_eq!(result.config.idle_boost.cpu_percent, 40);
        assert!(result.invalid_keys.is_empty());
        assert!(!result.unreadable);
        let issue = result.load_issue.expect("the unknown fields are reported");
        assert!(issue.contains("`idleBoost.boostCpuPercent`"), "{issue}");
        assert!(
            issue.contains("`profiles[0].localSyncDirectory`"),
            "{issue}"
        );
    }

    #[test]
    fn ignore_rules_are_a_list_and_a_string_is_refused() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("vapor.json");
        std::fs::write(
            &path,
            r#"{ "preIgnoreRules": ["tmp/", "*.log"], "postIgnoreRules": "build/" }"#,
        )
        .expect("write config");

        let result = load_from(&path);
        assert_eq!(result.config.pre_ignore_rules, vec!["tmp/", "*.log"]);
        assert!(result.config.post_ignore_rules.is_empty());
        assert_eq!(
            result.invalid_keys,
            vec![constants::config::KEY_POST_IGNORE_RULES]
        );
        assert!(
            result
                .load_issue
                .as_deref()
                .is_some_and(|issue| issue.contains("postIgnoreRules")),
            "{:?}",
            result.load_issue
        );
    }
}
