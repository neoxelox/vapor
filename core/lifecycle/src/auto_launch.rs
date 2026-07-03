//! `AutoLaunchSettingStore` — persist the user's autolaunch preference.
//!
//! Mirrors the Swift `AutoLaunchSettingStore` /
//! `VaporConfigurationAutoLaunchSettingStore` types in
//! `apps/macos/Sources/VaporCore/DaemonLifecycle.swift`. The on-disk
//! format is the same `vapor.json` the Swift app reads / writes; both
//! surfaces share one source of truth.
//!
//! Closes `core.md` C4-4.

use std::error::Error;
use std::fmt::{self, Display};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use vapor_shared::constants;

#[derive(Debug)]
pub enum JsonFileError {
    Io(io::Error),
    Parse(String),
}

impl Display for JsonFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "auto-launch store I/O error: {error}"),
            Self::Parse(reason) => write!(f, "auto-launch store parse error: {reason}"),
        }
    }
}

impl Error for JsonFileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Parse(_) => None,
        }
    }
}

impl From<io::Error> for JsonFileError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Persistent storage for the autolaunch preference.
///
/// `read` returns `Ok(None)` when the value has never been persisted,
/// matching Swift's `bool(forKey:) -> Bool?`. `write` is responsible
/// for atomic-on-disk semantics so a crash mid-write cannot leave the
/// file half-flushed.
pub trait AutoLaunchSettingStore: Send + Sync {
    fn read(&self) -> Result<Option<bool>, JsonFileError>;
    fn write(&self, value: bool) -> Result<(), JsonFileError>;
}

/// Process-local in-memory store. Used by tests and by the
/// pre-Wave-6 scaffolding while the CLI consumer is being written.
#[derive(Debug, Default)]
pub struct InMemoryAutoLaunchSettingStore {
    inner: Mutex<Option<bool>>,
}

impl InMemoryAutoLaunchSettingStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn seeded(value: Option<bool>) -> Self {
        Self {
            inner: Mutex::new(value),
        }
    }
}

impl AutoLaunchSettingStore for InMemoryAutoLaunchSettingStore {
    fn read(&self) -> Result<Option<bool>, JsonFileError> {
        Ok(*self
            .inner
            .lock()
            .expect("InMemoryAutoLaunchSettingStore mutex poisoned"))
    }

    fn write(&self, value: bool) -> Result<(), JsonFileError> {
        *self
            .inner
            .lock()
            .expect("InMemoryAutoLaunchSettingStore mutex poisoned") = Some(value);
        Ok(())
    }
}

/// JSON-file-backed store. Reads `autoLaunch` from the configured path
/// (typically `<vapor_dir>/vapor.json`) and writes back atomically via
/// the temp-file-then-rename pattern.
///
/// The file is parsed and re-emitted with `serde_json` (the same
/// serializer the `vapor config` CLI command uses), so every other
/// top-level key in the file is preserved verbatim on write. A file that
/// exists but does not parse as a JSON object is a hard error on write —
/// silently rewriting a corrupt config would destroy whatever the user
/// (or another surface) had there.
#[derive(Debug, Clone)]
pub struct JsonFileAutoLaunchSettingStore {
    path: PathBuf,
}

impl JsonFileAutoLaunchSettingStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AutoLaunchSettingStore for JsonFileAutoLaunchSettingStore {
    fn read(&self) -> Result<Option<bool>, JsonFileError> {
        let document = match read_document(&self.path)? {
            Some(document) => document,
            None => return Ok(None),
        };
        match document.get(constants::config::KEY_AUTO_LAUNCH) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
            Some(other) => Err(JsonFileError::Parse(format!(
                "expected boolean for \"{}\", found {other}",
                constants::config::KEY_AUTO_LAUNCH
            ))),
        }
    }

    fn write(&self, value: bool) -> Result<(), JsonFileError> {
        let mut document = read_document(&self.path)?
            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));

        let object = document.as_object_mut().ok_or_else(|| {
            JsonFileError::Parse("top-level JSON value is not an object".to_string())
        })?;
        object.insert(
            constants::config::KEY_AUTO_LAUNCH.to_string(),
            serde_json::Value::Bool(value),
        );

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut serialized = serde_json::to_string_pretty(&document)
            .map_err(|error| JsonFileError::Parse(error.to_string()))?;
        serialized.push('\n');

        let tmp_path = self.path.with_extension("vapor-tmp");
        fs::write(&tmp_path, serialized.as_bytes())?;
        fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }
}

/// Reads and parses the JSON document at `path`. `Ok(None)` when the
/// file is missing or effectively empty; `Err(Parse)` when it exists but
/// is not valid JSON.
fn read_document(path: &Path) -> Result<Option<serde_json::Value>, JsonFileError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(JsonFileError::Io(error)),
    };

    if contents.trim().is_empty() {
        return Ok(None);
    }

    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|error| JsonFileError::Parse(format!("{error} (at {})", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn in_memory_store_starts_empty() {
        let store = InMemoryAutoLaunchSettingStore::new();
        assert_eq!(store.read().expect("read"), None);
    }

    #[test]
    fn in_memory_store_round_trips_bool_value() {
        let store = InMemoryAutoLaunchSettingStore::new();
        store.write(true).expect("write");
        assert_eq!(store.read().expect("read"), Some(true));
        store.write(false).expect("write");
        assert_eq!(store.read().expect("read"), Some(false));
    }

    #[test]
    fn json_file_store_returns_none_for_missing_file() {
        let temp = TempDir::new().expect("temp");
        let store = JsonFileAutoLaunchSettingStore::new(temp.path().join("vapor.json"));
        assert_eq!(store.read().expect("read"), None);
    }

    #[test]
    fn json_file_store_writes_minimal_object_on_first_set() {
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        let store = JsonFileAutoLaunchSettingStore::new(path.clone());
        store.write(true).expect("write");
        assert_eq!(store.read().expect("read"), Some(true));
        let contents = fs::read_to_string(&path).expect("file");
        assert!(contents.contains("\"autoLaunch\": true"));
    }

    #[test]
    fn json_file_store_preserves_unrelated_keys_when_updating_auto_launch() {
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        fs::write(
            &path,
            r#"{
  "autoLaunch": false,
  "useGitIgnore": true,
  "languageCode": "en"
}
"#,
        )
        .expect("seed file");

        let store = JsonFileAutoLaunchSettingStore::new(path.clone());
        store.write(true).expect("write");

        let contents = fs::read_to_string(&path).expect("file");
        assert!(contents.contains("\"autoLaunch\": true"));
        assert!(contents.contains("\"useGitIgnore\": true"));
        assert!(contents.contains("\"languageCode\": \"en\""));
    }

    #[test]
    fn json_file_store_inserts_auto_launch_when_only_other_keys_exist() {
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        fs::write(&path, "{ \"useGitIgnore\": true }\n").expect("seed file");
        let store = JsonFileAutoLaunchSettingStore::new(path.clone());
        store.write(false).expect("write");
        let contents = fs::read_to_string(&path).expect("file");
        assert!(contents.contains("\"autoLaunch\": false"));
        assert!(contents.contains("\"useGitIgnore\": true"));
    }

    #[test]
    fn json_file_store_round_trips_through_filesystem() {
        let temp = TempDir::new().expect("temp");
        let store = JsonFileAutoLaunchSettingStore::new(temp.path().join("vapor.json"));
        store.write(true).expect("write");
        store.write(false).expect("write");
        store.write(true).expect("write");
        assert_eq!(store.read().expect("read"), Some(true));
    }

    #[test]
    fn corrupt_json_is_a_parse_error_on_write_and_the_file_is_preserved() {
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        fs::write(&path, "{ definitely not json").expect("seed corrupt file");

        let store = JsonFileAutoLaunchSettingStore::new(path.clone());
        let error = store
            .write(true)
            .expect_err("corrupt file must not be clobbered");
        assert!(matches!(error, JsonFileError::Parse(_)));
        assert_eq!(
            fs::read_to_string(&path).expect("file preserved"),
            "{ definitely not json"
        );
    }

    #[test]
    fn auto_launch_key_inside_string_values_is_not_misread() {
        // Regression for the pre-serde parser, which substring-matched
        // `"autoLaunch"` anywhere in the file — including inside string
        // values of unrelated keys.
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        fs::write(&path, r#"{ "note": "set \"autoLaunch\": true someday" }"#).expect("seed file");

        let store = JsonFileAutoLaunchSettingStore::new(path);
        assert_eq!(store.read().expect("read"), None);
    }

    #[test]
    fn non_boolean_auto_launch_value_is_a_parse_error() {
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        fs::write(&path, r#"{ "autoLaunch": "yes" }"#).expect("seed file");

        let store = JsonFileAutoLaunchSettingStore::new(path);
        assert!(matches!(
            store.read().expect_err("non-boolean must error"),
            JsonFileError::Parse(_)
        ));
    }
}
