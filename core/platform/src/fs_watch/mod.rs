//! Recursive filesystem watcher trait.
//!
//! See `docs/architecture/platform-abstractions.md` §`FsWatcher`.

use std::error::Error;
use std::fmt::{self, Display};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::SystemTime;

mod fake;
pub use fake::InMemoryFsWatcher;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::NativeFsWatcher;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::NativeFsWatcher;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::NativeFsWatcher;

/// Normalized fs-watch event kind. Maps the per-OS event vocabulary onto
/// a stable cross-platform set.
///
/// Rename semantics: directional rename halves are mapped to the
/// actionable kind directly — a rename-away is `Removed` at the old
/// path, a rename-in is `Created` at the new path, and a paired rename
/// event is split into that Removed/Created pair. `Renamed` survives
/// only for ambiguous OS reports where the source/destination side is
/// unknown; consumers must re-stat the path to disambiguate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchEventKind {
    Created,
    Modified,
    Removed,
    Renamed,
    Other,
}

/// Callback for watcher backend errors. Runs on the OS watcher's
/// callback thread, so it must stay cheap (log/record only). Without
/// one, backend errors are silently dropped — acceptable only for
/// consumers that re-derive state by stat (the filesystem provider's
/// changes feed); the daemon's local watch records every error.
pub type WatchErrorHandler = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchEvent {
    pub path: PathBuf,
    pub kind: WatchEventKind,
    pub observed_at: SystemTime,
}

#[derive(Debug)]
pub enum FsWatcherError {
    /// The watch root was empty / non-existent / not a directory.
    InvalidWatchRoot { path: PathBuf, reason: String },
    /// The OS notifier failed to install or returned an error.
    Backend(Box<dyn Error + Send + Sync>),
}

impl Display for FsWatcherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidWatchRoot { path, reason } => {
                write!(f, "invalid watch root {}: {reason}", path.display())
            }
            Self::Backend(error) => write!(f, "fs-watch backend failed: {error}"),
        }
    }
}

impl Error for FsWatcherError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Backend(error) => Some(&**error),
            _ => None,
        }
    }
}

/// Cross-OS recursive filesystem watcher.
///
/// Contract:
/// - `start` registers the OS notifier on `watch_root` and returns a
///   handle that keeps the notifier alive until dropped.
/// - The notifier callback is intentionally minimal: it normalizes the
///   path, applies a watch-root prefix check, and pushes a [`WatchEvent`]
///   onto the supplied [`Sender`]. No DB / hash / network work runs in
///   the callback. Per-component symlink resolution runs on the runtime
///   thread, not in the callback (see `core/daemon::fs_events`).
pub trait FsWatcher: Send + 'static {
    fn watch_root(&self) -> &Path;
}

/// Hook that engine code uses to construct the right watcher for the
/// current host. Tests substitute [`InMemoryFsWatcher::start`] directly
/// instead of going through this constructor.
/// Whether this host has a real native fs-watch implementation.
/// Consumers that would otherwise advertise watch-backed capabilities
/// (the filesystem provider's changes feed, the daemon runtime's local
/// watch) must check this and degrade honestly on hosts whose native
/// watcher is still a stub (currently Linux and Windows); the stub
/// constructor itself fails with an `Unsupported` backend error.
pub fn native_watcher_available() -> bool {
    cfg!(target_os = "macos")
}

pub fn start_native_watcher(
    watch_root: PathBuf,
    sender: Sender<WatchEvent>,
) -> Result<Box<dyn FsWatcher>, FsWatcherError> {
    start_native_watcher_with_error_handler(watch_root, sender, None)
}

pub fn start_native_watcher_with_error_handler(
    watch_root: PathBuf,
    sender: Sender<WatchEvent>,
    on_error: Option<WatchErrorHandler>,
) -> Result<Box<dyn FsWatcher>, FsWatcherError> {
    NativeFsWatcher::start_with_error_handler(watch_root, sender, on_error)
        .map(|watcher| Box::new(watcher) as Box<dyn FsWatcher>)
}
