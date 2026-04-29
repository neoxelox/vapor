//! Linux `FsWatcher` stub.
//!
//! The real implementation lives behind Wave 13 (`docs/tasks/core.md`
//! C7-1) and will use `inotify` (user) / `fanotify` (system). Until that
//! ships, the engine compiles on Linux with this stub so the workspace
//! stays cross-OS green; instantiating it returns an error so any
//! accidental call surface fails loudly instead of silently no-op-ing.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use super::{FsWatcher, FsWatcherError, WatchEvent};

#[derive(Debug)]
pub struct NativeFsWatcher {
    watch_root: PathBuf,
}

impl NativeFsWatcher {
    pub fn start(watch_root: PathBuf, _sender: Sender<WatchEvent>) -> Result<Self, FsWatcherError> {
        Err(FsWatcherError::InvalidWatchRoot {
            path: watch_root,
            reason: "Linux NativeFsWatcher is not implemented yet (Wave 13 / C7-1)".to_string(),
        })
    }
}

impl FsWatcher for NativeFsWatcher {
    fn watch_root(&self) -> &Path {
        &self.watch_root
    }
}
