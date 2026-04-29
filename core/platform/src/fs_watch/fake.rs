//! In-memory `FsWatcher` for tests + headless contexts.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use super::{FsWatcher, FsWatcherError, WatchEvent, WatchEventKind};

/// `FsWatcher` that never sees real OS events. Tests push synthetic
/// events through [`InMemoryFsWatcher::push_event`] to drive the runtime
/// deterministically.
#[derive(Debug)]
pub struct InMemoryFsWatcher {
    watch_root: PathBuf,
    inner: Arc<Mutex<InMemoryFsWatcherInner>>,
}

#[derive(Debug)]
struct InMemoryFsWatcherInner {
    sender: Sender<WatchEvent>,
}

impl InMemoryFsWatcher {
    pub fn start(watch_root: PathBuf, sender: Sender<WatchEvent>) -> Result<Self, FsWatcherError> {
        if watch_root.as_os_str().is_empty() {
            return Err(FsWatcherError::InvalidWatchRoot {
                path: watch_root,
                reason: "empty watch root".to_string(),
            });
        }
        Ok(Self {
            watch_root,
            inner: Arc::new(Mutex::new(InMemoryFsWatcherInner { sender })),
        })
    }

    pub fn push_event(&self, path: PathBuf, kind: WatchEventKind, observed_at: SystemTime) {
        let inner = self.inner.lock().expect("InMemoryFsWatcher mutex poisoned");
        let _ = inner.sender.send(WatchEvent {
            path,
            kind,
            observed_at,
        });
    }
}

impl FsWatcher for InMemoryFsWatcher {
    fn watch_root(&self) -> &Path {
        &self.watch_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn fake_watcher_emits_pushed_events_on_the_provided_channel() {
        let (tx, rx) = mpsc::channel();
        let watcher =
            InMemoryFsWatcher::start(PathBuf::from("/tmp/vapor-test"), tx).expect("fake watcher");
        let observed = SystemTime::now();
        watcher.push_event(
            PathBuf::from("/tmp/vapor-test/file.txt"),
            WatchEventKind::Created,
            observed,
        );

        let event = rx.try_recv().expect("event delivered");
        assert_eq!(event.path, PathBuf::from("/tmp/vapor-test/file.txt"));
        assert_eq!(event.kind, WatchEventKind::Created);
        assert_eq!(event.observed_at, observed);
    }

    #[test]
    fn fake_watcher_rejects_empty_watch_root() {
        let (tx, _rx) = mpsc::channel();
        let error = InMemoryFsWatcher::start(PathBuf::from(""), tx).expect_err("empty root");
        assert!(matches!(
            error,
            FsWatcherError::InvalidWatchRoot { reason, .. } if reason.contains("empty")
        ));
    }

    #[test]
    fn fake_watcher_exposes_watch_root() {
        let (tx, _rx) = mpsc::channel();
        let watcher =
            InMemoryFsWatcher::start(PathBuf::from("/tmp/vapor-test"), tx).expect("fake watcher");
        assert_eq!(watcher.watch_root(), Path::new("/tmp/vapor-test"));
    }
}
