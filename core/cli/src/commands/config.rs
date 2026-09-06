//! `vapor config get|set <key> [value]`.
//!
//! Reads / writes user-facing keys in `<vapor_dir>/vapor.json`. The
//! shape matches the macOS Swift `VaporConfigurationStore` so both
//! surfaces share one file. Unknown keys are preserved on every write
//! — the CLI only touches the key it was asked to
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
/// when the key has never been persisted. `None` only for keys with no
/// default (`deviceId`, which the daemon derives at first run).
pub fn get(path: &Path, key: &str) -> Result<Option<String>, ConfigError> {
    validate_key(key)?;
    let document = read_or_empty_object(path)?;
    Ok(document
        .get(key)
        .map(format_json_scalar)
        .or_else(|| default_for_key(key).as_ref().map(format_json_scalar)))
}

/// One line telling the user when the value takes effect, from the
/// same key classes the daemon's live reload uses.
pub fn apply_hint(key: &str) -> String {
    if constants::config::LIVE_RELOAD_KEYS.contains(&key) {
        format!("{key} saved; a running daemon applies it within a few seconds")
    } else if constants::config::RESTART_REQUIRED_KEYS.contains(&key) {
        format!("{key} saved; restart the daemon to apply it (vapor service restart)")
    } else {
        format!("{key} saved")
    }
}

/// The compiled default for a key, rendered from the same struct the
/// daemon starts from so the two can never disagree.
fn default_for_key(key: &str) -> Option<Value> {
    let defaults = serde_json::to_value(vapor_shared::config::VaporConfig::default()).ok()?;
    defaults.get(key).cloned()
}

/// Updates one key in-place. Preserves every other key so the macOS
/// Swift `VaporConfigurationStore` doesn't lose state on next read.
pub fn set(path: &Path, key: &str, value: &str) -> Result<(), ConfigError> {
    validate_key(key)?;
    let new_value = parse_value_for_key(key, value)?;

    // Serialize the whole read-modify-write against every other vapor.json
    // writer (the daemon, the app, a concurrent CLI): otherwise two writers
    // each read the same document and the last rename silently drops the
    // other's key.
    vapor_shared::runtime_paths::with_config_lock(path, || {
        let mut document = read_or_empty_object(path)?;
        let object = document
            .as_object_mut()
            .expect("read_or_empty_object returns an object");
        object.insert(key.to_string(), new_value);
        vapor_shared::runtime_paths::write_config_document(path, &document)?;
        Ok(())
    })
}

fn validate_key(key: &str) -> Result<(), ConfigError> {
    if constants::config::ALL_KEYS.contains(&key) {
        Ok(())
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
        KEY_AUTO_LAUNCH, KEY_IDLE_BOOST, KEY_PROFILES, KEY_PROVIDER, KEY_RESOURCE_LIMITS,
        KEY_SAFEGUARDS, KEY_SYNC_MODE, KEY_TIMELINE_LIMIT, KEY_USE_GIT_IGNORE,
        KEY_USE_VAPOR_IGNORE,
    };
    if matches!(
        key,
        KEY_AUTO_LAUNCH | KEY_USE_GIT_IGNORE | KEY_USE_VAPOR_IGNORE
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
        // permanent disagreement — reject it here.
        if parsed < 1 {
            return Err(ConfigError::Parse(format!(
                "'{key}' must be a positive integer, got {parsed}"
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
            "a JSON object such as {\"massDeleteThreshold\": 500}",
        );
    }
    Ok(Value::String(raw.to_string()))
}

/// Structured keys take JSON and must deserialize into the type the
/// daemon loads, otherwise the daemon would log the value as invalid at
/// startup and silently run on defaults. The parsed document is stored
/// as given (unknown fields survive, as the daemon ignores them).
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
    Ok(value)
}

/// Enum-typed keys reject unknown values with the accepted list —
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
        let error = set(&path, "safeguards", "massDeleteThreshold=500").expect_err("not JSON");
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
  "languageCode": "en"
}
"#,
        )
        .expect("seed");

        set(&path, "useGitIgnore", "false").expect("set");
        let contents = fs::read_to_string(&path).expect("file");
        assert!(contents.contains("\"autoLaunch\": true"));
        assert!(contents.contains("\"languageCode\": \"en\""));
        assert!(contents.contains("\"useGitIgnore\": false"));
    }

    #[test]
    fn set_rejects_non_boolean_for_boolean_key() {
        let temp = TempDir::new().expect("temp");
        let error = set(&config_path(&temp), "autoLaunch", "maybe").expect_err("bad value");
        assert!(matches!(error, ConfigError::Parse(_)));
    }

    #[test]
    fn set_persists_string_keys_verbatim() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        set(&path, "languageCode", "es").expect("set");
        assert_eq!(
            get(&path, "languageCode").expect("get"),
            Some("es".to_string())
        );
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
