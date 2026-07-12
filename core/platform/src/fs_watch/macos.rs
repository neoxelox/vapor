//! macOS `FsWatcher` implementation backed by FSEvents (via `notify`).
//!
//! The runtime-thread half of the discipline (per-component symlink
//! resolution, durable enqueue) lives in `core/daemon::fs_events`. This
//! file is intentionally thin: it owns the `RecommendedWatcher` handle
//! and forwards normalized events onto the supplied `Sender`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::SystemTime;

use notify::event::{CreateKind, ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use super::{FsWatcher, FsWatcherError, WatchErrorHandler, WatchEvent, WatchEventKind};

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
        Self::start_with_error_handler(watch_root, sender, None)
    }

    pub fn start_with_error_handler(
        watch_root: PathBuf,
        sender: Sender<WatchEvent>,
        on_error: Option<WatchErrorHandler>,
    ) -> Result<Self, FsWatcherError> {
        let watch_root = canonical_watch_root(watch_root)?;
        let callback_sender = sender;

        let mut watcher = notify::recommended_watcher(move |result| {
            forward_event(&callback_sender, on_error.as_ref(), result);
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

fn forward_event(
    sender: &Sender<WatchEvent>,
    on_error: Option<&WatchErrorHandler>,
    result: Result<Event, notify::Error>,
) {
    let event = match result {
        Ok(event) => event,
        Err(error) => {
            if let Some(handler) = on_error {
                handler(&error.to_string());
            }
            return;
        }
    };

    // A paired rename carries `[from, to]` paths; splitting it into a
    // Removed(from) + Created(to) pair keeps both sides independently
    // actionable — a bare "Renamed" for a path that no longer exists
    // cannot be acted on.
    let is_paired_rename = matches!(
        event.kind,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both))
    ) && event.paths.len() == 2;
    let uniform_kind = map_event_kind(&event.kind);
    let observed_at = SystemTime::now();
    for (index, path) in event.paths.into_iter().enumerate() {
        let kind = if is_paired_rename {
            if index == 0 {
                WatchEventKind::Removed
            } else {
                WatchEventKind::Created
            }
        } else {
            uniform_kind
        };
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
        // Unpaired rename halves are directional and map to the
        // actionable kind directly. The paired `Both` case is split in
        // `forward_event`; `Any`/`Other` keep the ambiguous `Renamed`
        // kind (the source vs destination side is unknown).
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => WatchEventKind::Removed,
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => WatchEventKind::Created,
        EventKind::Modify(ModifyKind::Name(_)) => WatchEventKind::Renamed,
        EventKind::Modify(_) => WatchEventKind::Modified,
        EventKind::Remove(_) => WatchEventKind::Removed,
        _ => WatchEventKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
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
    fn event_mapping_splits_paired_renames_and_maps_directional_halves() {
        let (tx, rx) = mpsc::channel();
        forward_event(
            &tx,
            None,
            Ok(Event {
                kind: EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                paths: vec![PathBuf::from("/w/old.txt"), PathBuf::from("/w/new.txt")],
                attrs: Default::default(),
            }),
        );
        let first = rx.try_recv().expect("from half");
        let second = rx.try_recv().expect("to half");
        assert_eq!(
            (first.path.as_path(), first.kind),
            (Path::new("/w/old.txt"), WatchEventKind::Removed)
        );
        assert_eq!(
            (second.path.as_path(), second.kind),
            (Path::new("/w/new.txt"), WatchEventKind::Created)
        );

        assert_eq!(
            map_event_kind(&EventKind::Modify(ModifyKind::Name(RenameMode::From))),
            WatchEventKind::Removed
        );
        assert_eq!(
            map_event_kind(&EventKind::Modify(ModifyKind::Name(RenameMode::To))),
            WatchEventKind::Created
        );
        assert_eq!(
            map_event_kind(&EventKind::Modify(ModifyKind::Name(RenameMode::Any))),
            WatchEventKind::Renamed
        );
    }

    #[test]
    fn backend_errors_reach_the_error_handler() {
        use std::sync::Mutex;
        let (tx, _rx) = mpsc::channel();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let handler: WatchErrorHandler = Arc::new(move |description: &str| {
            sink.lock().expect("sink").push(description.to_string());
        });
        forward_event(
            &tx,
            Some(&handler),
            Err(notify::Error::generic("backend exploded")),
        );
        assert_eq!(seen.lock().expect("seen").len(), 1);
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
