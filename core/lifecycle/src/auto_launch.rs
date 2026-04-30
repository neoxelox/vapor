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
/// The minimal JSON parser embedded here intentionally only handles the
/// `"autoLaunch"` key — everything else is preserved when other keys
/// already exist in the file. The Swift `VaporConfigurationStore` is
/// responsible for the full schema; this writer only updates one key
/// without touching the rest, mirroring the Swift `set(_:forKey:)`
/// semantics.
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
        match fs::read_to_string(&self.path) {
            Ok(contents) => parse_auto_launch(&contents).map_err(JsonFileError::Parse),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(JsonFileError::Io(error)),
        }
    }

    fn write(&self, value: bool) -> Result<(), JsonFileError> {
        let existing = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(JsonFileError::Io(error)),
        };

        let updated = upsert_auto_launch(&existing, value);

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp_path = self.path.with_extension("vapor-tmp");
        fs::write(&tmp_path, updated.as_bytes())?;
        fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }
}

/// Tiny, focused parser. Returns `None` when `autoLaunch` is absent or
/// when the file is empty / whitespace-only.
fn parse_auto_launch(contents: &str) -> Result<Option<bool>, String> {
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let Some(rest) = find_after(trimmed, "\"autoLaunch\"") else {
        return Ok(None);
    };
    let after_colon = skip_whitespace_and_colon(rest)
        .ok_or_else(|| "expected ':' after \"autoLaunch\"".to_string())?;
    if after_colon.starts_with("true") {
        Ok(Some(true))
    } else if after_colon.starts_with("false") {
        Ok(Some(false))
    } else {
        Err("expected boolean value for \"autoLaunch\"".to_string())
    }
}

/// Returns updated JSON with `"autoLaunch": <value>` set. Preserves all
/// other top-level keys when the file already exists; otherwise emits a
/// minimal one-key object so Swift's `VaporConfigurationStore` will
/// reconcile the remaining fields on next load.
fn upsert_auto_launch(contents: &str, value: bool) -> String {
    let trimmed = contents.trim();
    let needle = "\"autoLaunch\"";
    if trimmed.is_empty() {
        return format!(
            "{{\n  \"autoLaunch\": {}\n}}\n",
            if value { "true" } else { "false" }
        );
    }
    if let Some(start) = trimmed.find(needle) {
        // Replace the boolean literal that follows.
        let after_key = &trimmed[start + needle.len()..];
        let Some(rel_value_index) = skip_whitespace_and_colon_index(after_key) else {
            return contents.to_string();
        };
        let value_start = start + needle.len() + rel_value_index;
        let after_value = &trimmed[value_start..];
        let len = if after_value.starts_with("true") {
            4
        } else if after_value.starts_with("false") {
            5
        } else {
            return contents.to_string();
        };
        let mut updated = String::with_capacity(trimmed.len());
        updated.push_str(&trimmed[..value_start]);
        updated.push_str(if value { "true" } else { "false" });
        updated.push_str(&trimmed[value_start + len..]);
        return ensure_trailing_newline(updated);
    }
    // Key missing: insert before the closing `}`. Preserves whatever
    // content the Swift store already wrote.
    let last_brace = trimmed.rfind('}').unwrap_or(trimmed.len());
    let prefix = &trimmed[..last_brace];
    let suffix = &trimmed[last_brace..];
    let needs_comma = !prefix.trim_end().ends_with('{')
        && prefix
            .trim_end()
            .chars()
            .last()
            .map(|c| c != ',')
            .unwrap_or(true);
    let separator = if needs_comma { "," } else { "" };
    let inserted = format!(
        "{}{}\n  \"autoLaunch\": {}\n{}",
        prefix.trim_end(),
        separator,
        if value { "true" } else { "false" },
        suffix
    );
    ensure_trailing_newline(inserted)
}

fn ensure_trailing_newline(mut s: String) -> String {
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn find_after<'a>(haystack: &'a str, needle: &str) -> Option<&'a str> {
    let index = haystack.find(needle)?;
    Some(&haystack[index + needle.len()..])
}

fn skip_whitespace_and_colon(s: &str) -> Option<&str> {
    let index = skip_whitespace_and_colon_index(s)?;
    Some(&s[index..])
}

fn skip_whitespace_and_colon_index(s: &str) -> Option<usize> {
    let mut index = 0;
    let mut saw_colon = false;
    for (offset, c) in s.char_indices() {
        if c.is_whitespace() {
            index = offset + c.len_utf8();
            continue;
        }
        if c == ':' {
            if saw_colon {
                return None;
            }
            saw_colon = true;
            index = offset + 1;
            continue;
        }
        if saw_colon {
            return Some(index);
        }
        return None;
    }
    saw_colon.then_some(index)
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
    fn parse_auto_launch_handles_whitespace_variants() {
        assert_eq!(
            parse_auto_launch("{\"autoLaunch\":true}").expect("ok"),
            Some(true)
        );
        assert_eq!(
            parse_auto_launch("{ \"autoLaunch\" :  false }").expect("ok"),
            Some(false)
        );
        assert_eq!(parse_auto_launch("{}").expect("ok"), None);
    }
}
