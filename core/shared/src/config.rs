//! Typed loader for the persisted user configuration at
//! `<vapor_dir>/vapor.json`.
//!
//! Every surface shares this file: the macOS app's
//! `VaporConfigurationStore` and the `vapor config` CLI command write it;
//! the daemon reads it here at startup so persisted settings actually
//! reach the runtime. Environment variables (`VAPOR_*`) remain a
//! per-field override on top of the file — the resolution order is
//! env var → `vapor.json` → compiled default — which keeps the
//! LaunchAgent policy honest: the service definition only needs to carry
//! `VAPOR_DIR` (and optionally `VAPOR_ENV`).
//!
//! Parsing is tolerant the same way the Swift store is: a missing file
//! yields pure defaults, missing fields yield per-field defaults, and an
//! unreadable/corrupt file yields defaults plus a `load_issue` the caller
//! can surface — the file itself is never modified or overwritten by the
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
    pub use_git_ignore: bool,
    pub use_vapor_ignore: bool,
    pub local_sync_directory: String,
    pub cloud_sync_directory: String,
    pub pre_ignore_rules: String,
    pub post_ignore_rules: String,
    pub language_code: String,
    pub timeline_limit: i64,
    /// Provider selection: `filesystem` (default pre-GA) or
    /// `gdrive`. When `filesystem` is selected,
    /// `cloud_sync_directory` is reinterpreted as an absolute local
    /// directory that plays the cloud role.
    pub provider: String,
    /// Sync direction selector: `two-way` (default),
    /// `pull-only`, `push-only`. One-way values are strict mirrors and
    /// destructive to the subordinate side; they only activate through
    /// an explicit, enum-validated set — see
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
    /// config load — so a non-daemon consumer of `load_from` sees the raw
    /// values.
    pub resource_limits: ResourceLimitsConfig,
    /// Idle-boost group. `boost*Percent` values below their matching
    /// `resourceLimits` ceiling are clamped up at daemon budget-resolve
    /// time (same place as `resource_limits`), not at config load.
    pub idle_boost: IdleBoostConfig,
    /// Safeguards group (mass-delete guard tuning). Clamped to floors at
    /// daemon resolve time, not at config load.
    pub safeguards: SafeguardsConfig,
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
    /// downloads (each direction separately). `None` means automatic —
    /// the throttle ladder derives its ceiling from the machine's core
    /// count. Clamped into `1..=16` at daemon budget-resolve time.
    #[serde(default)]
    pub max_concurrent_transfers: Option<u8>,
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
            max_concurrent_transfers: None,
        }
    }
}

/// The `safeguards` config group. Values are clamped to their floors at
/// daemon resolve time (`core/daemon/src/safeguards.rs`), not at load,
/// so non-daemon consumers see the raw values.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SafeguardsConfig {
    #[serde(default = "default_mass_delete_enabled")]
    pub mass_delete_enabled: bool,
    #[serde(default = "default_mass_delete_threshold")]
    pub mass_delete_threshold: u64,
    #[serde(default = "default_mass_delete_window_seconds")]
    pub mass_delete_window_seconds: u64,
    #[serde(default = "default_mass_delete_ratio_percent")]
    pub mass_delete_ratio_percent: u8,
}

fn default_mass_delete_ratio_percent() -> u8 {
    constants::engine::MASS_DELETE_RATIO_PERCENT
}
fn default_mass_delete_enabled() -> bool {
    constants::safeguards::DEFAULT_MASS_DELETE_ENABLED
}
fn default_mass_delete_threshold() -> u64 {
    constants::engine::MASS_DELETE_THRESHOLD as u64
}
fn default_mass_delete_window_seconds() -> u64 {
    constants::engine::MASS_DELETE_WINDOW_SECONDS
}

impl Default for SafeguardsConfig {
    fn default() -> Self {
        Self {
            mass_delete_enabled: default_mass_delete_enabled(),
            mass_delete_threshold: default_mass_delete_threshold(),
            mass_delete_window_seconds: default_mass_delete_window_seconds(),
            mass_delete_ratio_percent: default_mass_delete_ratio_percent(),
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
    pub boost_cpu_percent: u8,
    #[serde(default = "default_boost_memory")]
    pub boost_memory_percent: u8,
    #[serde(default = "default_boost_bandwidth")]
    pub boost_bandwidth_percent: u8,
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
    constants::idle_boost::DEFAULT_BOOST_CPU_PERCENT
}
fn default_boost_memory() -> u8 {
    constants::idle_boost::DEFAULT_BOOST_MEMORY_PERCENT
}
fn default_boost_bandwidth() -> u8 {
    constants::idle_boost::DEFAULT_BOOST_BANDWIDTH_PERCENT
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
            boost_cpu_percent: default_boost_cpu(),
            boost_memory_percent: default_boost_memory(),
            boost_bandwidth_percent: default_boost_bandwidth(),
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
    pub max_concurrent_transfers: Option<u8>,
}

/// Per-profile `idleBoost`. Every field is optional; the daemon merges
/// each named value in the direction that makes boost more cautious:
/// `enabled: false` wins daemon-wide, `boost*Percent`, `headroomCpuPercent`
/// and `rampDownSeconds` are MIN-lowered, `minIdleSeconds` and
/// `rampUpSeconds` are MAX-raised.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IdleBoostOverride {
    pub enabled: Option<bool>,
    pub min_idle_seconds: Option<u64>,
    pub boost_cpu_percent: Option<u8>,
    pub boost_memory_percent: Option<u8>,
    pub boost_bandwidth_percent: Option<u8>,
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
    pub local_sync_directory: Option<String>,
    #[serde(default)]
    pub cloud_sync_directory: Option<String>,
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

impl Default for VaporConfig {
    fn default() -> Self {
        Self {
            auto_launch: constants::config::DEFAULT_AUTO_LAUNCH,
            use_git_ignore: constants::config::DEFAULT_USE_GIT_IGNORE,
            use_vapor_ignore: constants::config::DEFAULT_USE_VAPOR_IGNORE,
            local_sync_directory: constants::filtering::DEFAULT_LOCAL_SYNC_DIRECTORY.to_string(),
            cloud_sync_directory: constants::filtering::DEFAULT_CLOUD_SYNC_DIRECTORY.to_string(),
            pre_ignore_rules: default_pre_ignore_rules(),
            post_ignore_rules: String::new(),
            language_code: constants::config::DEFAULT_LANGUAGE_CODE.to_string(),
            timeline_limit: constants::config::DEFAULT_TIMELINE_LIMIT,
            provider: constants::provider::DEFAULT.to_string(),
            sync_mode: constants::sync_mode::DEFAULT.to_string(),
            profiles: Vec::new(),
            resource_limits: ResourceLimitsConfig::default(),
            idle_boost: IdleBoostConfig::default(),
            safeguards: SafeguardsConfig::default(),
        }
    }
}

/// The default pre-ignore rules as a newline-joined string, matching the
/// shape the config file and the Swift store use.
pub fn default_pre_ignore_rules() -> String {
    constants::filtering::DEFAULT_PRE_IGNORE_RULES.join("\n")
}

/// Result of a configuration load. `load_issue` is `Some` when the file
/// existed but could not be read or parsed; the returned configuration is
/// then the compiled defaults and the caller should surface the issue
/// (the daemon logs it; the app shows a banner) without touching the
/// on-disk file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaporConfigLoadResult {
    pub config: VaporConfig,
    pub load_issue: Option<String>,
}

/// Loads `vapor.json` from the current `vapor_dir`.
pub fn load_default() -> VaporConfigLoadResult {
    let path =
        crate::runtime_paths::vapor_directory().join(constants::runtime::CONFIGURATION_FILE_NAME);
    load_from(&path)
}

/// Loads a configuration file from an explicit path. Missing file →
/// defaults with no issue. Unreadable or unparsable file → defaults with
/// a `load_issue` describing why.
pub fn load_from(path: &Path) -> VaporConfigLoadResult {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return VaporConfigLoadResult {
                config: VaporConfig::default(),
                load_issue: None,
            };
        }
        Err(error) => {
            return VaporConfigLoadResult {
                config: VaporConfig::default(),
                load_issue: Some(format!("failed to read {}: {error}", path.display())),
            };
        }
    };

    if contents.trim().is_empty() {
        return VaporConfigLoadResult {
            config: VaporConfig::default(),
            load_issue: None,
        };
    }

    match serde_json::from_str::<serde_json::Value>(&contents) {
        Ok(serde_json::Value::Object(map)) => config_from_object(&map),
        // Valid JSON but not an object: the file shape is wrong.
        Ok(_) => VaporConfigLoadResult {
            config: VaporConfig::default(),
            load_issue: Some(format!("{} is not a JSON object", path.display())),
        },
        Err(error) => VaporConfigLoadResult {
            config: VaporConfig::default(),
            load_issue: Some(format!("failed to parse {}: {error}", path.display())),
        },
    }
}

/// Per-field-tolerant decode. Each known field is decoded independently:
/// one malformed value (a type mismatch, an out-of-range number) reverts
/// only that field to its default and is noted in `load_issue`, instead
/// of discarding the user's entire configuration. Unknown top-level keys
/// (typos like `profles`) are surfaced the same way rather than silently
/// dropped. Absent fields fill in defaults (partial files stay valid).
fn config_from_object(map: &serde_json::Map<String, serde_json::Value>) -> VaporConfigLoadResult {
    use constants::config as keys;

    fn field<T: serde::de::DeserializeOwned>(
        map: &serde_json::Map<String, serde_json::Value>,
        key: &str,
        default: T,
        issues: &mut Vec<String>,
    ) -> T {
        match map.get(key) {
            None => default,
            Some(value) => match serde_json::from_value::<T>(value.clone()) {
                Ok(parsed) => parsed,
                Err(error) => {
                    issues.push(format!("ignoring invalid `{key}`: {error}"));
                    default
                }
            },
        }
    }

    let defaults = VaporConfig::default();
    let mut issues: Vec<String> = Vec::new();
    let config = VaporConfig {
        auto_launch: field(
            map,
            keys::KEY_AUTO_LAUNCH,
            defaults.auto_launch,
            &mut issues,
        ),
        use_git_ignore: field(
            map,
            keys::KEY_USE_GIT_IGNORE,
            defaults.use_git_ignore,
            &mut issues,
        ),
        use_vapor_ignore: field(
            map,
            keys::KEY_USE_VAPOR_IGNORE,
            defaults.use_vapor_ignore,
            &mut issues,
        ),
        local_sync_directory: field(
            map,
            keys::KEY_LOCAL_SYNC_DIRECTORY,
            defaults.local_sync_directory,
            &mut issues,
        ),
        cloud_sync_directory: field(
            map,
            keys::KEY_CLOUD_SYNC_DIRECTORY,
            defaults.cloud_sync_directory,
            &mut issues,
        ),
        pre_ignore_rules: field(
            map,
            keys::KEY_PRE_IGNORE_RULES,
            defaults.pre_ignore_rules,
            &mut issues,
        ),
        post_ignore_rules: field(
            map,
            keys::KEY_POST_IGNORE_RULES,
            defaults.post_ignore_rules,
            &mut issues,
        ),
        language_code: field(
            map,
            keys::KEY_LANGUAGE_CODE,
            defaults.language_code,
            &mut issues,
        ),
        timeline_limit: field(
            map,
            keys::KEY_TIMELINE_LIMIT,
            defaults.timeline_limit,
            &mut issues,
        ),
        provider: field(map, keys::KEY_PROVIDER, defaults.provider, &mut issues),
        sync_mode: field(map, keys::KEY_SYNC_MODE, defaults.sync_mode, &mut issues),
        profiles: field(map, keys::KEY_PROFILES, defaults.profiles, &mut issues),
        resource_limits: field(
            map,
            keys::KEY_RESOURCE_LIMITS,
            defaults.resource_limits,
            &mut issues,
        ),
        idle_boost: field(map, keys::KEY_IDLE_BOOST, defaults.idle_boost, &mut issues),
        safeguards: field(map, keys::KEY_SAFEGUARDS, defaults.safeguards, &mut issues),
    };

    for key in map.keys() {
        if !keys::ALL_KEYS.contains(&key.as_str()) {
            issues.push(format!("unrecognized config key `{key}`"));
        }
    }

    VaporConfigLoadResult {
        config,
        load_issue: (!issues.is_empty()).then(|| issues.join("; ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn safeguards_group_and_transfer_ceiling_parse_with_partial_values() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("vapor.json");
        std::fs::write(
            &path,
            r#"{
                "safeguards": { "massDeleteThreshold": 1000 },
                "resourceLimits": { "maxConcurrentTransfers": 2 }
            }"#,
        )
        .expect("write config");

        let result = load_from(&path);
        assert!(result.load_issue.is_none(), "{:?}", result.load_issue);
        assert_eq!(result.config.safeguards.mass_delete_threshold, 1_000);
        // Unset group members keep their defaults.
        assert!(result.config.safeguards.mass_delete_enabled);
        assert_eq!(
            result.config.resource_limits.max_concurrent_transfers,
            Some(2)
        );
        // Absent means automatic.
        assert_eq!(
            VaporConfig::default()
                .resource_limits
                .max_concurrent_transfers,
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
            r#"{ "localSyncDirectory": "~/Projects", "useGitIgnore": false }"#,
        )
        .expect("seed config");

        let result = load_from(&path);
        assert!(result.load_issue.is_none());
        assert_eq!(result.config.local_sync_directory, "~/Projects");
        assert!(!result.config.use_git_ignore);
        assert!(result.config.use_vapor_ignore);
        assert_eq!(
            result.config.cloud_sync_directory,
            constants::filtering::DEFAULT_CLOUD_SYNC_DIRECTORY
        );
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
        // `timelineLimit` has the wrong type; `localSyncDirectory` is valid.
        std::fs::write(
            &path,
            r#"{ "timelineLimit": "oops", "localSyncDirectory": "~/Keep" }"#,
        )
        .expect("seed config");

        let result = load_from(&path);
        // The valid field is preserved (not discarded to defaults)...
        assert_eq!(result.config.local_sync_directory, "~/Keep");
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
    fn empty_file_loads_defaults_without_issue() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("vapor.json");
        std::fs::write(&path, "  \n").expect("seed empty config");

        let result = load_from(&path);
        assert_eq!(result.config, VaporConfig::default());
        assert!(result.load_issue.is_none());
    }

    #[test]
    fn default_pre_ignore_rules_join_the_constants_list() {
        let rules = default_pre_ignore_rules();
        assert!(rules.contains("node_modules/"));
        assert!(rules.contains("target/"));
        assert!(rules.contains("__pycache__/"));
        assert!(
            !rules.contains(".git/"),
            "repositories sync whole by default (owner decision)"
        );
        assert!(
            !rules.contains(".env"),
            "dotenv files sync by default (owner decision)"
        );
        assert_eq!(
            rules.lines().count(),
            constants::filtering::DEFAULT_PRE_IGNORE_RULES.len()
        );
    }
}
