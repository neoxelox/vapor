//! Durable lifecycle state — crash-loop bookkeeping and supervision
//! expectations that must survive process restarts.
//!
//! Every `vapor service …` invocation (and, through it, the macOS app
//! shim) is a fresh process, so the crash-loop guard's working memory
//! only matters within one command. This side-file is the state that
//! carries across invocations and surfaces: how many crashes are inside
//! the failure window, when the last one happened, whether the guard is
//! paused awaiting acknowledgement, and whether the daemon is expected
//! to be running at all.
//!
//! Lives at `<vapor_dir>/state/lifecycle.json`
//! (`runtime_paths::lifecycle_state_path`). Writes are atomic via the
//! temp-file-then-rename pattern shared with the autolaunch store.
//! Concurrent writers (two CLI invocations racing) resolve last-writer-
//! wins on the whole document; registrations are rare, user-driven or
//! tick-driven events, so the window is negligible and never corrupts
//! the file.
//!
//!(tracked there because the gap was observed
//! on the macOS surface; the implementation is portable).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::auto_launch::JsonFileError;

/// Bump on any incompatible shape change. Pre-GA, a file with a newer
/// version than this build understands is a hard error rather than a
/// silent reset — resetting could un-pause a crash-looping daemon.
pub const LIFECYCLE_STATE_SCHEMA_VERSION: u32 = 1;

/// The durable lifecycle document. Field names are the wire format.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedLifecycleState {
    pub schema_version: u32,
    /// True after any successful daemon start; false after an expected
    /// stop / uninstall. `check` only treats an absent daemon as a crash
    /// while this is set.
    pub daemon_should_be_running: bool,
    /// Crashes currently inside the failure window.
    pub consecutive_crashes: u32,
    /// Wall-clock moment of the most recent crash, in milliseconds since
    /// the Unix epoch. `None` when no crash is tracked.
    pub last_crash_at_ms: Option<u64>,
    /// Crash-loop pause engaged; only user acknowledgement clears it.
    pub paused_indefinitely: bool,
    /// An unexpected exit has been registered and its restart has not
    /// happened yet (backoff pending or pause engaged). Prevents the
    /// same exit from being counted as a fresh crash on every check.
    pub awaiting_restart: bool,
}

impl Default for PersistedLifecycleState {
    fn default() -> Self {
        Self {
            schema_version: LIFECYCLE_STATE_SCHEMA_VERSION,
            daemon_should_be_running: false,
            consecutive_crashes: 0,
            last_crash_at_ms: None,
            paused_indefinitely: false,
            awaiting_restart: false,
        }
    }
}

/// Persistence seam for [`PersistedLifecycleState`].
///
/// `load` returns `Ok(None)` when nothing has ever been persisted.
/// A file that exists but cannot be parsed is a hard error, never a
/// silent reset: resetting would clear a crash-loop pause without the
/// user's acknowledgement.
pub trait LifecycleStateStore: Send + Sync {
    fn load(&self) -> Result<Option<PersistedLifecycleState>, JsonFileError>;
    fn save(&self, state: &PersistedLifecycleState) -> Result<(), JsonFileError>;
}

/// Process-local store for tests and non-persistent scaffolding.
#[derive(Debug, Default)]
pub struct InMemoryLifecycleStateStore {
    inner: Mutex<Option<PersistedLifecycleState>>,
}

impl InMemoryLifecycleStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn seeded(state: PersistedLifecycleState) -> Self {
        Self {
            inner: Mutex::new(Some(state)),
        }
    }

    /// Test observer: the most recently saved state, if any.
    pub fn current(&self) -> Option<PersistedLifecycleState> {
        self.inner
            .lock()
            .expect("InMemoryLifecycleStateStore mutex poisoned")
            .clone()
    }
}

impl LifecycleStateStore for InMemoryLifecycleStateStore {
    fn load(&self) -> Result<Option<PersistedLifecycleState>, JsonFileError> {
        Ok(self.current())
    }

    fn save(&self, state: &PersistedLifecycleState) -> Result<(), JsonFileError> {
        *self
            .inner
            .lock()
            .expect("InMemoryLifecycleStateStore mutex poisoned") = Some(state.clone());
        Ok(())
    }
}

/// JSON-file-backed store at `<vapor_dir>/state/lifecycle.json`.
#[derive(Debug, Clone)]
pub struct JsonFileLifecycleStateStore {
    path: PathBuf,
}

impl JsonFileLifecycleStateStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl LifecycleStateStore for JsonFileLifecycleStateStore {
    fn load(&self) -> Result<Option<PersistedLifecycleState>, JsonFileError> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(JsonFileError::Io(error)),
        };
        if contents.trim().is_empty() {
            return Ok(None);
        }
        let state: PersistedLifecycleState = serde_json::from_str(&contents).map_err(|error| {
            JsonFileError::Parse(format!(
                "{error} (at {}; delete the file to reset lifecycle state)",
                self.path.display()
            ))
        })?;
        if state.schema_version > LIFECYCLE_STATE_SCHEMA_VERSION {
            return Err(JsonFileError::Parse(format!(
                "lifecycle state schema v{} is newer than this build supports (v{}) \
                 (at {})",
                state.schema_version,
                LIFECYCLE_STATE_SCHEMA_VERSION,
                self.path.display()
            )));
        }
        Ok(Some(state))
    }

    fn save(&self, state: &PersistedLifecycleState) -> Result<(), JsonFileError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut serialized = serde_json::to_string_pretty(state)
            .map_err(|error| JsonFileError::Parse(error.to_string()))?;
        serialized.push('\n');
        let tmp_path = self.path.with_extension("vapor-tmp");
        fs::write(&tmp_path, serialized.as_bytes())?;
        fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }
}

/// Wall-clock seam. The crash-loop guard runs on `Instant` (monotonic),
/// but durable timestamps need a clock that survives process
/// restarts.
pub trait WallClock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

#[derive(Debug, Default)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

/// Settable clock for deterministic tests.
#[derive(Debug, Default)]
pub struct FixedWallClock {
    ms: Mutex<u64>,
}

impl FixedWallClock {
    pub fn at(ms: u64) -> Self {
        Self { ms: Mutex::new(ms) }
    }

    pub fn set(&self, ms: u64) {
        *self.ms.lock().expect("FixedWallClock mutex poisoned") = ms;
    }
}

impl WallClock for FixedWallClock {
    fn now_ms(&self) -> u64 {
        *self.ms.lock().expect("FixedWallClock mutex poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_state() -> PersistedLifecycleState {
        PersistedLifecycleState {
            schema_version: LIFECYCLE_STATE_SCHEMA_VERSION,
            daemon_should_be_running: true,
            consecutive_crashes: 3,
            last_crash_at_ms: Some(1_750_000_000_000),
            paused_indefinitely: false,
            awaiting_restart: true,
        }
    }

    #[test]
    fn json_file_store_returns_none_for_missing_file() {
        let temp = TempDir::new().expect("temp");
        let store = JsonFileLifecycleStateStore::new(temp.path().join("lifecycle.json"));
        assert_eq!(store.load().expect("load"), None);
    }

    #[test]
    fn json_file_store_round_trips_state() {
        let temp = TempDir::new().expect("temp");
        let store = JsonFileLifecycleStateStore::new(temp.path().join("lifecycle.json"));
        store.save(&sample_state()).expect("save");
        assert_eq!(store.load().expect("load"), Some(sample_state()));
    }

    #[test]
    fn json_file_store_creates_missing_parent_directories() {
        let temp = TempDir::new().expect("temp");
        let store =
            JsonFileLifecycleStateStore::new(temp.path().join("state/nested/lifecycle.json"));
        store.save(&sample_state()).expect("save");
        assert_eq!(store.load().expect("load"), Some(sample_state()));
    }

    #[test]
    fn corrupt_file_is_a_parse_error_not_a_silent_reset() {
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("lifecycle.json");
        fs::write(&path, "{ not json").expect("seed corrupt file");
        let store = JsonFileLifecycleStateStore::new(path.clone());
        let error = store.load().expect_err("corrupt file must error");
        assert!(matches!(error, JsonFileError::Parse(_)));
        // The corrupt file is preserved for diagnosis.
        assert_eq!(fs::read_to_string(&path).expect("file"), "{ not json");
    }

    #[test]
    fn newer_schema_version_is_rejected() {
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("lifecycle.json");
        fs::write(
            &path,
            format!(
                "{{ \"schema_version\": {} }}",
                LIFECYCLE_STATE_SCHEMA_VERSION + 1
            ),
        )
        .expect("seed file");
        let store = JsonFileLifecycleStateStore::new(path);
        assert!(matches!(
            store.load().expect_err("newer schema must error"),
            JsonFileError::Parse(_)
        ));
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // Forward-compatible reads: a minimal document hydrates with
        // default values for absent fields.
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("lifecycle.json");
        fs::write(&path, "{ \"consecutive_crashes\": 2 }").expect("seed file");
        let store = JsonFileLifecycleStateStore::new(path);
        let state = store.load().expect("load").expect("state");
        assert_eq!(state.consecutive_crashes, 2);
        assert!(!state.daemon_should_be_running);
        assert_eq!(state.last_crash_at_ms, None);
    }

    #[test]
    fn empty_file_reads_as_none() {
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("lifecycle.json");
        fs::write(&path, "\n").expect("seed file");
        let store = JsonFileLifecycleStateStore::new(path);
        assert_eq!(store.load().expect("load"), None);
    }
}
