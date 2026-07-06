//! `vapor config get|set <key> [value]`.
//!
//! Reads / writes user-facing keys in `<vapor_dir>/vapor.json`. The
//! shape matches the macOS Swift `VaporConfigurationStore` so both
//! surfaces share one file. Unknown keys are preserved on every write
//! per `cli.md` L1-2 — the CLI only touches the key it was asked to
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

/// Result of a `get`. `None` when the key has never been persisted —
/// the binary renders that as the documented default value.
pub fn get(path: &Path, key: &str) -> Result<Option<String>, ConfigError> {
    validate_key(key)?;
    let document = read_or_empty_object(path)?;
    let value = document.get(key);
    Ok(value.map(format_json_scalar))
}

/// Updates one key in-place. Preserves every other key so the macOS
/// Swift `VaporConfigurationStore` doesn't lose state on next read.
pub fn set(path: &Path, key: &str, value: &str) -> Result<(), ConfigError> {
    validate_key(key)?;
    let mut document = read_or_empty_object(path)?;
    let new_value = parse_value_for_key(key, value)?;

    let object = document
        .as_object_mut()
        .expect("read_or_empty_object returns an object");
    object.insert(key.to_string(), new_value);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let serialized = serde_json::to_string_pretty(&document)
        .map_err(|error| ConfigError::Parse(error.to_string()))?;
    let mut serialized = serialized;
    serialized.push('\n');

    let tmp_path = path.with_extension("vapor-tmp");
    fs::write(&tmp_path, serialized.as_bytes())?;
    fs::rename(&tmp_path, path)?;
    Ok(())
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
        KEY_AUTO_LAUNCH, KEY_PROVIDER, KEY_SYNC_MODE, KEY_TIMELINE_EVENT_LIMIT, KEY_USE_GIT_IGNORE,
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
    if key == KEY_TIMELINE_EVENT_LIMIT {
        return raw
            .parse::<i64>()
            .map(|value| Value::Number(value.into()))
            .map_err(|error| ConfigError::Parse(format!("expected integer for '{key}': {error}")));
    }
    if key == KEY_PROVIDER {
        return parse_enum_value(key, raw, constants::provider::ALL);
    }
    if key == KEY_SYNC_MODE {
        return parse_enum_value(key, raw, constants::sync_mode::ALL);
    }
    Ok(Value::String(raw.to_string()))
}

/// Enum-typed keys reject unknown values with the accepted list —
/// especially load-bearing for `syncMode`, whose one-way values are
/// destructive and must never be a typo away (C8-59).
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
    fn get_returns_none_for_missing_file() {
        let temp = TempDir::new().expect("temp");
        assert_eq!(get(&config_path(&temp), "autoLaunch").expect("get"), None);
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
        set(&path, "timelineEventLimit", "2500").expect("set");
        assert_eq!(
            get(&path, "timelineEventLimit").expect("get"),
            Some("2500".to_string())
        );
    }

    #[test]
    fn provider_key_is_enum_validated() {
        let temp = TempDir::new().expect("temp");
        let path = config_path(&temp);
        set(&path, "provider", "filesystem").expect("filesystem accepted");
        set(&path, "provider", "google_drive").expect("google_drive accepted");
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
}
