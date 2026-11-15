//! `vapor config get|set <key> [value]`.
//!
//! Reads / writes user-facing keys in `<vapor_dir>/vapor.json`. The
//! shape matches the macOS Swift `VaporConfigurationStore` so both
//! surfaces share one file. Unknown keys are preserved on every write:
//! the CLI only touches the key it was asked to
//! touch.

use std::error::Error;
use std::fmt::{self, Display};
use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;
use vapor_shared::constants;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigCommand {
    Get { key: String },
    Set { key: String, value: String },
}

#[derive(Debug)]
pub enum ConfigError {
    Io(io::Error),
    Parse(String),
    UnknownKey(String),
}

impl Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "config I/O error: {error}"),
            Self::Parse(reason) => write!(f, "config parse error: {reason}"),
            Self::UnknownKey(key) => write!(f, "config key '{key}' is not recognized"),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ConfigError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Result of a `get`: the persisted value, or the compiled default
/// when the key has never been persisted. `None` for keys with no
/// default (the two sync roots until the user sets them, `deviceId`
/// before the daemon's first run,
/// `resourceLimits.concurrentTransfers` while automatic). A dotted key
/// (`resourceLimits.cpuPercent`) reads one field of a group.
pub fn get(path: &Path, key: &str) -> Result<Option<String>, ConfigError> {
    let segments = validate_key(key)?;
    let document = read_or_empty_object(path)?;
    if let Some(value) = lookup(&document, &segments).filter(|value| !value.is_null()) {
        return Ok(Some(format_json_scalar(value)));
    }
    let defaults = serde_json::to_value(vapor_shared::config::VaporConfig::default())
        .map_err(|error| ConfigError::Parse(error.to_string()))?;
    Ok(lookup(&defaults, &segments)
        .filter(|value| !value.is_null())
        .map(format_json_scalar))
}

fn lookup<'a>(document: &'a Value, segments: &[&str]) -> Option<&'a Value> {
    segments
        .iter()
        .try_fold(document, |value, segment| value.get(*segment))
}

/// One line telling the user when the value takes effect, from the
/// same key classes the daemon's live reload uses. A one-way sync mode
/// adds what it does to the other side and that the daemon asks before
/// it runs.
pub fn apply_hint(key: &str, value: &str) -> String {
    let top = key.split('.').next().unwrap_or(key);
    let mut hint = if constants::config::LIVE_RELOAD_KEYS.contains(&top) {
        format!("{key} saved; a running daemon applies it within a few seconds")
    } else if constants::config::RESTART_REQUIRED_KEYS.contains(&top) {
        format!("{key} saved; restart the daemon to apply it (vapor service restart)")
    } else {
        format!("{key} saved")
    };
    if let Some(warning) = one_way_warning(key, value) {
        hint.push('\n');
        hint.push_str(&warning);
    }
    hint
}

/// What a one-way `syncMode` does, for the command that sets it. The
/// mode does not run on this alone: the daemon holds the profile behind
/// a decision until the user confirms.
pub fn one_way_warning(key: &str, value: &str) -> Option<String> {
    if key != constants::config::KEY_SYNC_MODE {
        return None;
    }
    let effect = match vapor_shared::SyncMode::from_config_value(value)? {
        vapor_shared::SyncMode::TwoWay => return None,
        vapor_shared::SyncMode::PullOnly => {
            "pull-only makes this device an exact copy of the cloud: it overwrites files that \
             differ here and removes files that are only here"
        }
        vapor_shared::SyncMode::PushOnly => {
            "push-only makes the cloud an exact copy of this device: it overwrites files that \
             differ in the cloud and removes files that are only in the cloud"
        }
    };
    Some(format!(
        "Warning: {effect}. The profile syncs nothing until you confirm: vapor decisions list"
    ))
}

/// Updates one key in place and keeps every other key, so the Swift
/// `VaporConfigurationStore` and the daemon never lose state. A dotted
/// key (`resourceLimits.cpuPercent`, `safeguards.massDelete.threshold`)
/// changes one field of a group and keeps its other fields; `null`
/// removes the field so it takes its default. A whole group given as
/// JSON replaces the group.
pub fn set(path: &Path, key: &str, value: &str) -> Result<(), ConfigError> {
    let segments = validate_key(key)?;
    let top = segments[0];
    let field_value = if segments.len() == 1 {
        parse_value_for_key(top, value)?
    } else {
        // One field of a group: a number, a boolean, or `null` reads as
        // JSON; anything else is a string. The whole group is checked
        // below against the type the daemon loads.
        serde_json::from_str::<Value>(value).unwrap_or_else(|_| Value::String(value.to_string()))
    };

    // Serialize the whole read-modify-write against every other vapor.json
    // writer (the daemon, the app, a concurrent CLI): otherwise two writers
    // each read the same document and the last rename silently drops the
    // other's key.
    vapor_shared::runtime_paths::with_config_lock(path, || {
        let mut document = read_or_empty_object(path)?;
        let object = document
            .as_object_mut()
            .expect("read_or_empty_object returns an object");
        if segments.len() == 1 {
            object.insert(top.to_string(), field_value);
        } else {
            let mut group = object
                .get(top)
                .filter(|group| group.is_object())
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default()));
            set_path(&mut group, &segments[1..], field_value);
            let checked = parse_value_for_key(top, &group.to_string())?;
            object.insert(top.to_string(), checked);
        }
        vapor_shared::runtime_paths::write_config_document(path, &document)?;
        Ok(())
    })
}

fn set_path(target: &mut Value, segments: &[&str], value: Value) {
    let Some((last, parents)) = segments.split_last() else {
        return;
    };
    let mut node = target;
    for segment in parents {
        let object = node.as_object_mut().expect("groups are objects");
        let child = object
            .entry(segment.to_string())
            .or_insert_with(|| Value::Object(Default::default()));
        if !child.is_object() {
            *child = Value::Object(Default::default());
        }
        node = child;
    }
    let object = node.as_object_mut().expect("groups are objects");
    if value.is_null() {
        object.remove(*last);
    } else {
        object.insert(last.to_string(), value);
    }
}

/// The key's path: one segment for a top-level key, more for a field of
/// a group (`resourceLimits.cpuPercent`, `safeguards.massDelete.enabled`).
fn validate_key(key: &str) -> Result<Vec<&str>, ConfigError> {
    use constants::config as keys;
    let segments: Vec<&str> = key.split('.').collect();
    let known = match segments.as_slice() {
        [top] => keys::ALL_KEYS.contains(top),
        [keys::KEY_RESOURCE_LIMITS, field] => constants::resource_limits::ALL_KEYS.contains(field),
        [keys::KEY_IDLE_BOOST, field] => constants::idle_boost::ALL_KEYS.contains(field),
        [keys::KEY_TRASH, field] => constants::trash::ALL_KEYS.contains(field),
        [keys::KEY_SAFEGUARDS, constants::safeguards::KEY_MASS_DELETE] => true,
        [
            keys::KEY_SAFEGUARDS,
            constants::safeguards::KEY_MASS_DELETE,
            field,
        ] => constants::safeguards::MASS_DELETE_KEYS.contains(field),
        _ => false,
    };
    if known {
        Ok(segments)
    } else {
        Err(ConfigError::UnknownKey(key.to_string()))
    }
}

fn read_or_empty_object(path: &Path) -> Result<Value, ConfigError> {
    match fs::read_to_string(path) {
        Ok(contents) if contents.trim().is_empty() => Ok(Value::Object(Default::default())),
        Ok(contents) => serde_json::from_str(&contents)
            .map_err(|error| ConfigError::Parse(format!("{} (at {})", error, path.display()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(Value::Object(Default::default()))
        }
        Err(error) => Err(ConfigError::Io(error)),
    }
}

fn parse_value_for_key(key: &str, raw: &str) -> Result<Value, ConfigError> {
    use constants::config::{
        KEY_AUTO_LAUNCH, KEY_IDLE_BOOST, KEY_POST_IGNORE_RULES, KEY_PRE_IGNORE_RULES, KEY_PROFILES,
        KEY_PROVIDER, KEY_RESOURCE_LIMITS, KEY_SAFEGUARDS, KEY_SYNC_MODE, KEY_TIMELINE_LIMIT,
        KEY_TRASH, KEY_USE_GITIGNORE, KEY_USE_VAPORIGNORE,
    };
    if matches!(
        key,
        KEY_AUTO_LAUNCH | KEY_USE_GITIGNORE | KEY_USE_VAPORIGNORE
    ) {
        return match raw {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            other => Err(ConfigError::Parse(format!(
                "expected 'true' or 'false' for '{key}', got '{other}'"
            ))),
        };
    }
    if key == KEY_TIMELINE_LIMIT {
        let parsed = raw.parse::<i64>().map_err(|error| {
            ConfigError::Parse(format!("expected integer for '{key}': {error}"))
        })?;
        // The daemon silently ignores non-positive limits (keeps the
        // default), so storing one would leave config and behavior in
        // permanent disagreement: reject it here.
        if parsed < 1 {
            return Err(ConfigError::Parse(format!(
                "'{key}' must be a positive integer, got {parsed}"
            )));
        }
        // The daemon keeps at most this many: the timeline is read back
        // in one IPC frame.
        if parsed > constants::config::MAX_TIMELINE_LIMIT {
            return Err(ConfigError::Parse(format!(
                "'{key}' is at most {}, got {parsed}",
                constants::config::MAX_TIMELINE_LIMIT
            )));
        }
        return Ok(Value::Number(parsed.into()));
    }
    if key == KEY_PROVIDER {
        return parse_enum_value(key, raw, constants::provider::ALL);
    }
    if key == KEY_SYNC_MODE {
        return parse_enum_value(key, raw, constants::sync_mode::ALL);
    }
    if matches!(key, KEY_PRE_IGNORE_RULES | KEY_POST_IGNORE_RULES) {
        return parse_json_value::<Vec<String>>(
            key,
            raw,
            "a JSON array of rules such as [\"build/\", \"*.log\"]",
        );
    }
    if key == KEY_PROFILES {
        return parse_json_value::<Vec<vapor_shared::config::ProfileConfig>>(
            key,
            raw,
            "a JSON array of profile objects",
        );
    }
    if key == KEY_RESOURCE_LIMITS {
        return parse_json_value::<vapor_shared::config::ResourceLimitsConfig>(
            key,
            raw,
            "a JSON object such as {\"cpuPercent\": 10}",
        );
    }
    if key == KEY_IDLE_BOOST {
        return parse_json_value::<vapor_shared::config::IdleBoostConfig>(
            key,
            raw,
            "a JSON object such as {\"enabled\": false}",
        );
    }
    if key == KEY_SAFEGUARDS {
        return parse_json_value::<vapor_shared::config::SafeguardsConfig>(
            key,
            raw,
            "a JSON object such as {\"massDelete\": {\"threshold\": 500}}",
        );
    }
    if key == KEY_TRASH {
        return parse_json_value::<vapor_shared::config::TrashConfig>(
            key,
            raw,
            "a JSON object such as {\"retentionDays\": 14}",
        );
    }
    Ok(Value::String(raw.to_string()))
}

/// Structured keys take JSON and must deserialize into the type the
/// daemon loads, with no field it does not know; otherwise the daemon
/// would fall back to defaults for what the user meant to set.
fn parse_json_value<T: serde::de::DeserializeOwned>(
    key: &str,
    raw: &str,
    shape: &str,
) -> Result<Value, ConfigError> {
    let value: Value = serde_json::from_str(raw).map_err(|error| {
        ConfigError::Parse(format!(
            "expected {shape} for '{key}', got '{raw}' ({error})"
        ))
    })?;
    serde_json::from_value::<T>(value.clone())
        .map_err(|error| ConfigError::Parse(format!("'{key}' is not {shape}: {error}")))?;
    let unknown = vapor_shared::config::unknown_nested_keys(key, &value);
    if !unknown.is_empty() {
        return Err(ConfigError::Parse(format!(
            "unknown field(s) {} in '{key}'; docs/product/configuration.md lists its fields",
            unknown.join(", ")
        )));
    }
    Ok(value)
}

/// Enum-typed keys reject unknown values with the accepted list,
/// especially load-bearing for `syncMode`, whose one-way values are
/// destructive and must never be a typo away.
fn parse_enum_value(key: &str, raw: &str, accepted: &[&str]) -> Result<Value, ConfigError> {
    if accepted.contains(&raw) {
        Ok(Value::String(raw.to_string()))
    } else {
        Err(ConfigError::Parse(format!(
            "expected one of [{}] for '{key}', got '{raw}'",
            accepted.join(", ")
        )))
    }
}

fn format_json_scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn config_path(dir: &TempDir) -> std::path::PathBuf {
        dir.path().join("vapor.json")
    }

    #[test]
    fn get_renders_the_compiled_default_for_an_unset_key() {
        let temp = TempDir::new().expect("temp");
        assert_eq!(
            get(&config_path(&temp), "syncMode").expect("get"),
            Some("two-way".to_string())
        );
        assert_eq!(
            get(&config_path(&temp), "autoLaunch").expect("get"),
            Some(constants::config::DEFAULT_AUTO_LAUNCH.to_string())
        );
        let limits = get(&config_path(&temp), "resourceLimits")
            .expect("get")
            .expect("has a default");
        assert!(limits.contains("\"cpuPercent\":15"), "{limits}");
        assert_eq!(get(&config_path(&temp), "deviceId").expect("get"), None);
    }

    #[test]
    fn setting_a_one_way_mode_says_what_it_does_and_that_the_daemon_asks_first() {
        let pull = apply_hint("syncMode", "pull-only");
        assert!(pull.contains("restart the daemon"), "{pull}");
        assert!(pull.contains("removes files that are only here"), "{pull}");
        assert!(pull.contains("vapor decisions list"), "{pull}");
        let push = apply_hint("syncMode", "push-only");
        assert!(
            push.contains("removes files that are only in the cloud"),
            "{push}"
        );
        assert!(!apply_hint("syncMode", "two-way").contains("Warning"));
        assert!(!apply_hint("provider", "pull-only").contains("Warning"));
    }

    #[test]
    fn the_sync_roots_have_no_default_until_they_are_set() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        assert_eq!(get(&path, "localRoot").expect("get"), None);
        assert_eq!(get(&path, "cloudRoot").expect("get"), None);
        set(&path, "provider", "gdrive").expect("set provider");
        assert_eq!(
            get(&path, "cloudRoot").expect("get"),
            None,
            "a provider does not choose a folder for the user"
        );
        set(&path, "cloudRoot", "/Work").expect("set root");
        assert_eq!(
            get(&path, "cloudRoot").expect("get"),
            Some("/Work".to_string())
        );
    }

    #[test]
    fn structured_keys_take_json_and_round_trip() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        set(&path, "resourceLimits", r#"{"cpuPercent": 5}"#).expect("set");
        assert_eq!(
            get(&path, "resourceLimits").expect("get"),
            Some(r#"{"cpuPercent":5}"#.to_string())
        );
        set(
            &path,
            "profiles",
            r#"[{"id": "work", "idleBoost": {"enabled": false}}]"#,
        )
        .expect("set profiles");
        let contents = fs::read_to_string(&path).expect("file");
        // Stored as JSON, not as a string containing JSON.
        assert!(contents.contains("\"profiles\": ["), "{contents}");
        let error = set(&path, "safeguards", "massDelete.threshold=500").expect_err("not JSON");
        assert!(matches!(error, ConfigError::Parse(_)));
        let error = set(&path, "profiles", r#"{"id": "x"}"#).expect_err("wrong shape");
        assert!(error.to_string().contains("profiles"), "{error}");
        let error = set(&path, "idleBoost", r#"{"enabled": "yes"}"#).expect_err("wrong type");
        assert!(matches!(error, ConfigError::Parse(_)));
    }

    #[test]
    fn get_rejects_unknown_keys() {
        let temp = TempDir::new().expect("temp");
        let error = get(&config_path(&temp), "nonsense").expect_err("unknown key");
        assert!(matches!(error, ConfigError::UnknownKey(_)));
    }

    #[test]
    fn set_then_get_round_trips_a_boolean_key() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        set(&path, "autoLaunch", "false").expect("set");
        assert_eq!(
            get(&path, "autoLaunch").expect("get"),
            Some("false".to_string())
        );
    }

    #[test]
    fn set_preserves_other_keys() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        fs::write(
            &path,
            r#"{
  "autoLaunch": true,
  "language": "en"
}
"#,
        )
        .expect("seed");

        set(&path, "useGitignore", "false").expect("set");
        let contents = fs::read_to_string(&path).expect("file");
        assert!(contents.contains("\"autoLaunch\": true"));
        assert!(contents.contains("\"language\": \"en\""));
        assert!(contents.contains("\"useGitignore\": false"));
    }

    #[test]
    fn set_rejects_non_boolean_for_boolean_key() {
        let temp = TempDir::new().expect("temp");
        let error = set(&config_path(&temp), "autoLaunch", "maybe").expect_err("bad value");
        assert!(matches!(error, ConfigError::Parse(_)));
    }

    #[test]
    fn a_dotted_key_sets_one_field_and_keeps_the_rest_of_its_group() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        set(
            &path,
            "resourceLimits",
            r#"{"bandwidthPercent": 5, "concurrentTransfers": 2}"#,
        )
        .expect("set group");
        set(&path, "resourceLimits.cpuPercent", "10").expect("set field");
        assert_eq!(
            get(&path, "resourceLimits.cpuPercent").expect("get"),
            Some("10".into())
        );
        assert_eq!(
            get(&path, "resourceLimits.bandwidthPercent").expect("get"),
            Some("5".into())
        );

        // `null` removes the field, so it takes its default again.
        set(&path, "resourceLimits.concurrentTransfers", "null").expect("unset");
        assert_eq!(
            get(&path, "resourceLimits.concurrentTransfers").expect("get"),
            None
        );
        assert_eq!(
            get(&path, "safeguards.massDelete.threshold").expect("default"),
            Some("1000".into())
        );
        set(&path, "safeguards.massDelete.enabled", "false").expect("nested field");
        assert_eq!(
            get(&path, "safeguards.massDelete.enabled").expect("get"),
            Some("false".into())
        );

        // The field's type is checked like the whole group's.
        assert!(set(&path, "resourceLimits.cpuPercent", "lots").is_err());
        assert!(matches!(
            set(&path, "resourceLimits.boostCpuPercent", "10"),
            Err(ConfigError::UnknownKey(_))
        ));
        assert!(apply_hint("resourceLimits.cpuPercent", "5").contains("within a few seconds"));
    }

    #[test]
    fn a_group_with_a_field_it_does_not_have_is_refused() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        let error = set(&path, "idleBoost", r#"{"boostCpuPercent": 10}"#).expect_err("old name");
        assert!(
            error.to_string().contains("idleBoost.boostCpuPercent"),
            "{error}"
        );
        let error = set(
            &path,
            "profiles",
            r#"[{"id": "work", "localSyncDirectory": "~/W"}]"#,
        )
        .expect_err("old name in a profile");
        assert!(
            error.to_string().contains("profiles[0].localSyncDirectory"),
            "{error}"
        );
    }

    #[test]
    fn ignore_rules_are_set_as_a_json_list() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        set(&path, "postIgnoreRules", r#"["build/", "*.log"]"#).expect("set");
        assert_eq!(
            get(&path, "postIgnoreRules").expect("get"),
            Some(r#"["build/","*.log"]"#.to_string())
        );
        let error = set(&path, "postIgnoreRules", "build/").expect_err("not a list");
        assert!(error.to_string().contains("JSON array"), "{error}");
    }

    #[test]
    fn set_persists_string_keys_verbatim() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        set(&path, "language", "es").expect("set");
        assert_eq!(get(&path, "language").expect("get"), Some("es".to_string()));
    }

    #[test]
    fn set_parses_integer_keys() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        set(&path, "timelineLimit", "2500").expect("set");
        assert_eq!(
            get(&path, "timelineLimit").expect("get"),
            Some("2500".to_string())
        );
    }

    #[test]
    fn provider_key_is_enum_validated() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        set(&path, "provider", "filesystem").expect("filesystem accepted");
        set(&path, "provider", "gdrive").expect("gdrive accepted");
        let error = set(&path, "provider", "dropbox").expect_err("unknown provider");
        assert!(matches!(error, ConfigError::Parse(_)));
        assert!(error.to_string().contains("filesystem"));
    }

    #[test]
    fn sync_mode_key_is_enum_validated() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        set(&path, "syncMode", "two-way").expect("two-way accepted");
        set(&path, "syncMode", "pull-only").expect("pull-only accepted");
        set(&path, "syncMode", "push-only").expect("push-only accepted");
        let error = set(&path, "syncMode", "mirror").expect_err("unknown mode");
        assert!(matches!(error, ConfigError::Parse(_)));
        assert!(error.to_string().contains("two-way"));
    }

    #[test]
    fn timeline_limit_rejects_non_positive_values() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        set(&path, "timelineLimit", "500").expect("positive accepted");
        let too_large = set(&path, "timelineLimit", "10001").expect_err("over the maximum");
        assert!(
            too_large.to_string().contains("at most 10000"),
            "{too_large}"
        );
        for bad in ["0", "-100"] {
            let error = set(&path, "timelineLimit", bad).expect_err("non-positive rejected");
            assert!(matches!(error, ConfigError::Parse(_)), "got {error:?}");
        }
        // The valid value from the first set is intact.
        assert_eq!(
            get(&path, "timelineLimit").expect("get").as_deref(),
            Some("500")
        );
    }
}
