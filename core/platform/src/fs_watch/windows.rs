//! Windows `FsWatcher` stub.
//!
//! The real implementation will use `ReadDirectoryChangesW` + IOCP.
//! Until that ships,
//! the engine compiles on Windows with this stub so the workspace stays
//! cross-OS green; instantiating it returns an error so any accidental
//! call surface fails loudly instead of silently no-op-ing.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use super::{FsWatcher, FsWatcherError, WatchErrorHandler, WatchEvent};

#[derive(Debug)]
pub struct NativeFsWatcher {
    watch_root: PathBuf,
}

impl NativeFsWatcher {
    pub fn start(watch_root: PathBuf, sender: Sender<WatchEvent>) -> Result<Self, FsWatcherError> {
        Self::start_with_error_handler(watch_root, sender, None)
    }

    /// Same constructor surface as the macOS implementation so
    /// `start_native_watcher_with_error_handler` compiles on every OS.
    pub fn start_with_error_handler(
        watch_root: PathBuf,
        _sender: Sender<WatchEvent>,
        _on_error: Option<WatchErrorHandler>,
    ) -> Result<Self, FsWatcherError> {
        Err(FsWatcherError::InvalidWatchRoot {
            path: watch_root,
            reason: "Windows NativeFsWatcher is not implemented yet".to_string(),
        })
    }
}

impl FsWatcher for NativeFsWatcher {
    fn watch_root(&self) -> &Path {
        &self.watch_root
    }
}
