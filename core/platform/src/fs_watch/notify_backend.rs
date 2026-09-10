//! The `notify`-backed `FsWatcher` shared by macOS (FSEvents) and Linux
//! (inotify): it owns the `RecommendedWatcher` handle and forwards
//! normalized events onto the supplied `Sender`. The runtime-thread
//! half of the discipline (per-component symlink resolution, durable
//! enqueue) lives in `core/daemon::fs_events`.
//!
//! Two backend conditions surface as events rather than silence. A
//! kernel queue overflow (inotify's `Q_OVERFLOW`, reported by `notify`
//! as a path-less `Rescan`) becomes an `Other` event on the watch root,
//! which the engine answers with a whole-scope reconcile. A watch that
//! cannot be added because the host's inotify watch limit is reached
//! fails the start with a message naming the sysctl to raise.

use std::path::{Path, PathBuf};
use std::sync::Arc;
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
        let callback_root: Arc<Path> = Arc::from(watch_root.as_path());

        let mut watcher = notify::recommended_watcher(move |result| {
            forward_event(&callback_sender, &callback_root, on_error.as_ref(), result);
        })
        .map_err(|error| FsWatcherError::Backend(Box::new(error)))?;

        watcher
            .watch(&watch_root, RecursiveMode::Recursive)
            .map_err(|error| watch_failure(error, &watch_root))?;

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

    vapor_shared::paths::canonicalize(&watch_root).map_err(|error| {
        FsWatcherError::InvalidWatchRoot {
            path: watch_root,
            reason: format!("canonicalize failed: {error}"),
        }
    })
}

/// A failed `watch` call, with the one Linux-specific cause spelled
/// out: inotify refuses new watches with `ENOSPC` once the user's
/// `fs.inotify.max_user_watches` budget is spent, and nothing about
/// disk space is wrong.
fn watch_failure(error: notify::Error, watch_root: &Path) -> FsWatcherError {
    let out_of_watches = matches!(
        &error.kind,
        notify::ErrorKind::Io(io) if io.raw_os_error() == Some(libc::ENOSPC)
    ) || matches!(&error.kind, notify::ErrorKind::MaxFilesWatch);
    if out_of_watches {
        return FsWatcherError::InvalidWatchRoot {
            path: watch_root.to_path_buf(),
            reason: "the host is out of inotify watches for this user; raise \
                     fs.inotify.max_user_watches (for example \
                     `sudo sysctl fs.inotify.max_user_watches=524288`) and restart"
                .to_string(),
        };
    }
    FsWatcherError::Backend(Box::new(error))
}

fn forward_event(
    sender: &Sender<WatchEvent>,
    watch_root: &Path,
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
    if event.need_rescan() || (event.paths.is_empty() && matches!(event.kind, EventKind::Other)) {
        // The backend dropped events (a full kernel queue): the only
        // honest report is "something under the root changed".
        let _ = sender.send(WatchEvent {
            path: watch_root.to_path_buf(),
            kind: WatchEventKind::Other,
            observed_at: SystemTime::now(),
        });
        return;
    }

    // Opens and closes change nothing. inotify reports them for every
    // directory the reconcile walk reads, root included; forwarding
    // them as "something happened on the root" would make each walk
    // schedule the next one.
    if matches!(event.kind, EventKind::Access(_)) {
        return;
    }

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
        let mut kind = if is_paired_rename {
            if index == 0 {
                WatchEventKind::Removed
            } else {
                WatchEventKind::Created
            }
        } else {
            uniform_kind
        };
        // The root itself being removed, renamed, or (re)mounted is
        // never a file operation to mirror (FSEvents reports a root
        // change as a rename-away, a mount as a create); it is a
        // reason to look at the whole scope again, and the root
        // identity check decides what became of the root.
        if path == watch_root
            && matches!(
                kind,
                WatchEventKind::Removed | WatchEventKind::Renamed | WatchEventKind::Created
            )
        {
            kind = WatchEventKind::Other;
        }
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
            vapor_shared::paths::canonicalize(&watch_root)
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
            Path::new("/w"),
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
    fn a_queue_overflow_becomes_an_other_event_on_the_watch_root() {
        let (tx, rx) = mpsc::channel();
        forward_event(
            &tx,
            Path::new("/w"),
            None,
            Ok(Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan)),
        );
        let event = rx.try_recv().expect("an event for the root");
        assert_eq!(event.path, Path::new("/w"));
        assert_eq!(event.kind, WatchEventKind::Other);
    }

    #[test]
    fn a_removal_or_rename_of_the_root_itself_is_a_rescan_not_a_delete() {
        let (tx, rx) = mpsc::channel();
        for kind in [
            EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            EventKind::Remove(notify::event::RemoveKind::Other),
            EventKind::Create(CreateKind::Other),
        ] {
            forward_event(
                &tx,
                Path::new("/w"),
                None,
                Ok(Event {
                    kind,
                    paths: vec![PathBuf::from("/w")],
                    attrs: Default::default(),
                }),
            );
            let event = rx.try_recv().expect("an event for the root");
            assert_eq!(event.path, Path::new("/w"));
            assert_eq!(event.kind, WatchEventKind::Other);
        }
    }

    #[test]
    fn opens_and_closes_are_not_forwarded() {
        use notify::event::AccessKind;
        let (tx, rx) = mpsc::channel();
        for kind in [
            EventKind::Access(AccessKind::Open(notify::event::AccessMode::Any)),
            EventKind::Access(AccessKind::Close(notify::event::AccessMode::Write)),
            EventKind::Access(AccessKind::Any),
        ] {
            forward_event(
                &tx,
                Path::new("/w"),
                None,
                Ok(Event {
                    kind,
                    paths: vec![PathBuf::from("/w")],
                    attrs: Default::default(),
                }),
            );
        }
        assert!(
            rx.try_recv().is_err(),
            "an open or close on the root is not a dropped-events signal"
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
            Path::new("/w"),
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
