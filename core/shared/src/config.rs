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

use serde::Deserialize;

use crate::constants;

/// Fully-resolved configuration with every field defaulted. Field names
/// follow Rust conventions; the on-disk keys are the camelCase names in
/// [`constants::config`].
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Hard user ceilings on daemon device impact. Values are
    /// clamped into `1..=100` at load with a classified warning
    /// surfaced through `load_issue`.
    pub resource_limits: ResourceLimitsConfig,
    /// Idle-boost group. `boost*Percent` values below their
    /// matching `resourceLimits` ceiling are clamped up at load.
    pub idle_boost: IdleBoostConfig,
}

/// The `resourceLimits` config group.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLimitsConfig {
    #[serde(default = "default_cpu_percent")]
    pub cpu_percent: u8,
    #[serde(default = "default_memory_percent")]
    pub memory_percent: u8,
    #[serde(default = "default_bandwidth_percent")]
    pub bandwidth_percent: u8,
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
        }
    }
}

/// The `idleBoost` config group.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
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

/// One entry of the `profiles` array. Every field except `id` is
/// optional on the wire; unset fields inherit the top-level defaults
/// (categorical override semantics — a profile value wins
/// outright, no merging).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
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
    /// Optional per-profile ceilings; resolved by MIN-lowering against
    /// the top-level group — a profile can only tighten.
    #[serde(default)]
    pub resource_limits: Option<ResourceLimitsConfig>,
    /// Optional per-profile idle-boost override. Any enabled profile
    /// with `enabled = false` disables boost daemon-wide.
    #[serde(default)]
    pub idle_boost: Option<IdleBoostConfig>,
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

    match serde_json::from_str::<RawVaporConfig>(&contents) {
        Ok(raw) => VaporConfigLoadResult {
            config: raw.into_config(),
            load_issue: None,
        },
        Err(error) => VaporConfigLoadResult {
            config: VaporConfig::default(),
            load_issue: Some(format!("failed to parse {}: {error}", path.display())),
        },
    }
}

/// Wire shape: every field optional so partial files (written by an older
/// surface, or hand-edited) fill in per-field defaults, mirroring the
/// Swift store's `decodeIfPresent` behavior. Unknown fields are ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawVaporConfig {
    auto_launch: Option<bool>,
    use_git_ignore: Option<bool>,
    use_vapor_ignore: Option<bool>,
    local_sync_directory: Option<String>,
    cloud_sync_directory: Option<String>,
    pre_ignore_rules: Option<String>,
    post_ignore_rules: Option<String>,
    language_code: Option<String>,
    timeline_limit: Option<i64>,
    provider: Option<String>,
    sync_mode: Option<String>,
    profiles: Option<Vec<ProfileConfig>>,
    resource_limits: Option<ResourceLimitsConfig>,
    idle_boost: Option<IdleBoostConfig>,
}

impl RawVaporConfig {
    fn into_config(self) -> VaporConfig {
        let defaults = VaporConfig::default();
        VaporConfig {
            auto_launch: self.auto_launch.unwrap_or(defaults.auto_launch),
            use_git_ignore: self.use_git_ignore.unwrap_or(defaults.use_git_ignore),
            use_vapor_ignore: self.use_vapor_ignore.unwrap_or(defaults.use_vapor_ignore),
            local_sync_directory: self
                .local_sync_directory
                .unwrap_or(defaults.local_sync_directory),
            cloud_sync_directory: self
                .cloud_sync_directory
                .unwrap_or(defaults.cloud_sync_directory),
            pre_ignore_rules: self.pre_ignore_rules.unwrap_or(defaults.pre_ignore_rules),
            post_ignore_rules: self.post_ignore_rules.unwrap_or(defaults.post_ignore_rules),
            language_code: self.language_code.unwrap_or(defaults.language_code),
            timeline_limit: self.timeline_limit.unwrap_or(defaults.timeline_limit),
            provider: self.provider.unwrap_or(defaults.provider),
            sync_mode: self.sync_mode.unwrap_or(defaults.sync_mode),
            profiles: self.profiles.unwrap_or_default(),
            resource_limits: self.resource_limits.unwrap_or_default(),
            idle_boost: self.idle_boost.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
    fn unknown_fields_are_tolerated() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("vapor.json");
        std::fs::write(&path, r#"{ "futureKey": [1, 2], "autoLaunch": false }"#)
            .expect("seed config");

        let result = load_from(&path);
        assert!(result.load_issue.is_none());
        assert!(!result.config.auto_launch);
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
        assert!(rules.contains(".git/"));
        assert_eq!(
            rules.lines().count(),
            constants::filtering::DEFAULT_PRE_IGNORE_RULES.len()
        );
    }
}
