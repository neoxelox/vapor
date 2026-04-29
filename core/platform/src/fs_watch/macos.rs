//! macOS `FsWatcher` implementation backed by FSEvents (via `notify`).
//!
//! The runtime-thread half of the discipline (per-component symlink
//! resolution, durable enqueue) lives in `core/daemon::fs_events`. This
//! file is intentionally thin: it owns the `RecommendedWatcher` handle
//! and forwards normalized events onto the supplied `Sender`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::SystemTime;

use notify::event::{CreateKind, ModifyKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use super::{FsWatcher, FsWatcherError, WatchEvent, WatchEventKind};

pub struct NativeFsWatcher {
    _watcher: RecommendedWatcher,
    watch_root: PathBuf,
}

impl std::fmt::Debug for NativeFsWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeFsWatcher")
            .field("watch_root", &self.watch_root)
            .finish()
    }
}

impl NativeFsWatcher {
    pub fn start(watch_root: PathBuf, sender: Sender<WatchEvent>) -> Result<Self, FsWatcherError> {
        let watch_root = canonical_watch_root(watch_root)?;
        let callback_sender = sender;

        let mut watcher = notify::recommended_watcher(move |result| {
            forward_event(&callback_sender, result);
        })
        .map_err(|error| FsWatcherError::Backend(Box::new(error)))?;

        watcher
            .watch(&watch_root, RecursiveMode::Recursive)
            .map_err(|error| FsWatcherError::Backend(Box::new(error)))?;

        Ok(Self {
            _watcher: watcher,
            watch_root,
        })
    }
}

impl FsWatcher for NativeFsWatcher {
    fn watch_root(&self) -> &Path {
        &self.watch_root
    }
}

fn canonical_watch_root(watch_root: PathBuf) -> Result<PathBuf, FsWatcherError> {
    if watch_root.as_os_str().is_empty() {
        return Err(FsWatcherError::InvalidWatchRoot {
            path: watch_root,
            reason: "empty watch root".to_string(),
        });
    }

    if !watch_root.exists() {
        return Err(FsWatcherError::InvalidWatchRoot {
            path: watch_root.clone(),
            reason: "does not exist".to_string(),
        });
    }

    if !watch_root.is_dir() {
        return Err(FsWatcherError::InvalidWatchRoot {
            path: watch_root.clone(),
            reason: "not a directory".to_string(),
        });
    }

    std::fs::canonicalize(&watch_root).map_err(|error| FsWatcherError::InvalidWatchRoot {
        path: watch_root,
        reason: format!("canonicalize failed: {error}"),
    })
}

fn forward_event(sender: &Sender<WatchEvent>, result: Result<Event, notify::Error>) {
    let Ok(event) = result else {
        return;
    };

    let kind = map_event_kind(&event.kind);
    let observed_at = SystemTime::now();
    for path in event.paths {
        let _ = sender.send(WatchEvent {
            path,
            kind,
            observed_at,
        });
    }
}

fn map_event_kind(kind: &EventKind) -> WatchEventKind {
    match kind {
        EventKind::Create(CreateKind::Any)
        | EventKind::Create(CreateKind::File)
        | EventKind::Create(CreateKind::Folder)
        | EventKind::Create(CreateKind::Other) => WatchEventKind::Created,
        EventKind::Modify(ModifyKind::Name(_)) => WatchEventKind::Renamed,
        EventKind::Modify(_) => WatchEventKind::Modified,
        EventKind::Remove(_) => WatchEventKind::Removed,
        _ => WatchEventKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tempfile::TempDir;

    #[test]
    fn native_watcher_canonicalizes_existing_directory_at_start() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let (tx, _rx) = mpsc::channel();
        let watcher = NativeFsWatcher::start(watch_root.clone(), tx).expect("start watcher");
        assert_eq!(
            watcher.watch_root(),
            std::fs::canonicalize(&watch_root)
                .expect("canonical")
                .as_path()
        );
    }

    #[test]
    fn native_watcher_rejects_missing_root() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("missing");
        let (tx, _rx) = mpsc::channel();
        let error = NativeFsWatcher::start(watch_root, tx).expect_err("missing root");
        assert!(matches!(
            error,
            FsWatcherError::InvalidWatchRoot { reason, .. } if reason.contains("does not exist")
        ));
    }

    #[test]
    fn native_watcher_rejects_file_path() {
        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("file.txt");
        std::fs::write(&file_path, b"x").expect("write file");
        let (tx, _rx) = mpsc::channel();
        let error = NativeFsWatcher::start(file_path, tx).expect_err("file path");
        assert!(matches!(
            error,
            FsWatcherError::InvalidWatchRoot { reason, .. } if reason.contains("not a directory")
        ));
    }
}
