//! Filesystem-backed remote changes feed.
//!
//! A `core/platform/fs_watch` watcher on the remote root feeds a
//! bounded in-memory ring of [`RemoteChange`]s, each tagged with a
//! monotonic sequence number that doubles as the feed cursor. The
//! watcher callback keeps the fs-watch discipline (push-only, no
//! hashing / tag reads); normalization, internal-file filtering, and
//! op-id lookup run inside [`ChangesFeed::poll`] on the engine's
//! runtime thread.
//!
//! The ring is in-memory by design: a daemon restart (or a ring
//! overflow) invalidates old cursors, and [`ChangesPoll::CursorExpired`]
//! tells the engine to run a whole-scope reconcile and re-baseline —
//! the same convergence path a real cloud provider needs when its
//! server-side cursor expires.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::SystemTime;

use vapor_platform::fs_watch::{FsWatcher, WatchEvent, WatchEventKind, start_native_watcher};
use vapor_shared::constants;

use super::is_internal_file_name;
use crate::tags::OpIdTagStore;
use crate::{
    ChangesPoll, ProviderError, RemoteChange, RemoteChangeKind, RemoteChangesPage, RemotePath,
};

/// Test hook: pushes synthetic watch events into the feed exactly the
/// way the native watcher would, so daemon integration tests drive the
/// remote pipeline deterministically.
#[derive(Clone)]
pub struct ManualFeedHandle {
    sender: Sender<WatchEvent>,
}

impl ManualFeedHandle {
    pub fn emit(
        &self,
        path: impl Into<std::path::PathBuf>,
        kind: WatchEventKind,
        observed_at: SystemTime,
    ) {
        let _ = self.sender.send(WatchEvent {
            path: path.into(),
            kind,
            observed_at,
        });
    }

    pub fn emit_created(&self, path: impl Into<std::path::PathBuf>, observed_at: SystemTime) {
        self.emit(path, WatchEventKind::Created, observed_at);
    }

    pub fn emit_modified(&self, path: impl Into<std::path::PathBuf>, observed_at: SystemTime) {
        self.emit(path, WatchEventKind::Modified, observed_at);
    }

    pub fn emit_removed(&self, path: impl Into<std::path::PathBuf>, observed_at: SystemTime) {
        self.emit(path, WatchEventKind::Removed, observed_at);
    }
}

pub(crate) struct ChangesFeed {
    sender: Sender<WatchEvent>,
    receiver: Mutex<Receiver<WatchEvent>>,
    watcher: Mutex<Option<Box<dyn FsWatcher>>>,
    ring: Mutex<FeedRing>,
}

struct FeedRing {
    entries: VecDeque<(u64, RemoteChange)>,
    /// Sequence the next appended change receives; the head cursor is
    /// `next_sequence - 1`.
    next_sequence: u64,
}

impl FeedRing {
    /// Oldest sequence still replayable. A cursor below
    /// `floor_sequence - 1` has lost events and must be expired.
    fn floor_sequence(&self) -> u64 {
        self.entries
            .front()
            .map(|(sequence, _)| *sequence)
            .unwrap_or(self.next_sequence)
    }

    fn head_cursor(&self) -> u64 {
        self.next_sequence - 1
    }

    fn push(&mut self, change: RemoteChange) {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.entries.push_back((sequence, change));
        while self.entries.len() > constants::provider::CHANGES_FEED_RING_MAX_EVENTS {
            self.entries.pop_front();
        }
    }
}

impl ChangesFeed {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = channel();
        Self {
            sender,
            receiver: Mutex::new(receiver),
            watcher: Mutex::new(None),
            ring: Mutex::new(FeedRing {
                entries: VecDeque::new(),
                next_sequence: 1,
            }),
        }
    }

    pub(crate) fn manual_handle(&self) -> ManualFeedHandle {
        ManualFeedHandle {
            sender: self.sender.clone(),
        }
    }

    pub(crate) fn ensure_native_watcher(&self, root: &Path) -> Result<(), ProviderError> {
        let mut watcher = self
            .watcher
            .lock()
            .expect("changes feed watcher mutex poisoned");
        if watcher.is_some() {
            return Ok(());
        }
        let started =
            start_native_watcher(root.to_path_buf(), self.sender.clone()).map_err(|error| {
                ProviderError::transient(format!(
                    "cannot start remote changes watcher on {}: {error}",
                    root.display()
                ))
            })?;
        *watcher = Some(started);
        Ok(())
    }

    pub(crate) fn poll(
        &self,
        root: &Path,
        tags: &OpIdTagStore,
        cursor: Option<&str>,
        max_changes: usize,
    ) -> Result<ChangesPoll, ProviderError> {
        self.drain_watch_events(root, tags);

        let ring = self.ring.lock().expect("changes feed ring mutex poisoned");
        let Some(cursor) = cursor else {
            // Baseline: changes before this poll are the reconcile's
            // job; the feed starts reporting from "now".
            return Ok(ChangesPoll::Page(RemoteChangesPage {
                changes: Vec::new(),
                next_cursor: ring.head_cursor().to_string(),
            }));
        };

        let Ok(cursor) = cursor.parse::<u64>() else {
            return Ok(ChangesPoll::CursorExpired);
        };
        if cursor > ring.head_cursor() || cursor + 1 < ring.floor_sequence() {
            return Ok(ChangesPoll::CursorExpired);
        }

        let mut changes = Vec::new();
        let mut next_cursor = cursor;
        for (sequence, change) in ring.entries.iter() {
            if *sequence <= cursor {
                continue;
            }
            if changes.len() >= max_changes {
                break;
            }
            changes.push(change.clone());
            next_cursor = *sequence;
        }
        Ok(ChangesPoll::Page(RemoteChangesPage {
            changes,
            next_cursor: next_cursor.to_string(),
        }))
    }

    fn drain_watch_events(&self, root: &Path, tags: &OpIdTagStore) {
        let receiver = self
            .receiver
            .lock()
            .expect("changes feed receiver mutex poisoned");
        let mut ring = self.ring.lock().expect("changes feed ring mutex poisoned");
        for event in receiver.try_iter() {
            if let Some(change) = normalize_watch_event(root, tags, event) {
                ring.push(change);
            }
        }
    }
}

/// Maps a raw watch event onto a feed change. `None` drops the event:
/// paths outside the root, the root itself, internal temp/side-files,
/// symlinks, and directory events (directories materialize through
/// their children or reconcile).
fn normalize_watch_event(
    root: &Path,
    tags: &OpIdTagStore,
    event: WatchEvent,
) -> Option<RemoteChange> {
    let name = event.path.file_name()?.to_str()?;
    if is_internal_file_name(name) {
        return None;
    }
    let remote_path = RemotePath::from_local(root, &event.path)?;

    match fs::symlink_metadata(&event.path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || metadata.is_dir() {
                return None;
            }
            let op_id = tags.read_op_id(&event.path);
            Some(RemoteChange {
                path: remote_path,
                kind: RemoteChangeKind::CreatedOrModified,
                observed_at: event.observed_at,
                op_id,
                content_hash: None,
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(RemoteChange {
            path: remote_path,
            kind: RemoteChangeKind::Removed,
            observed_at: event.observed_at,
            op_id: None,
            content_hash: None,
        }),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use vapor_platform::fs_caps::InMemoryFilesystemCapabilities;

    fn feed_fixture() -> (
        tempfile::TempDir,
        ChangesFeed,
        ManualFeedHandle,
        OpIdTagStore,
    ) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let feed = ChangesFeed::new();
        let handle = feed.manual_handle();
        let tags = OpIdTagStore::new(Arc::new(InMemoryFilesystemCapabilities::default()));
        (dir, feed, handle, tags)
    }

    fn ts(offset_ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(1_750_000_000_000 + offset_ms)
    }

    fn expect_page(poll: ChangesPoll) -> RemoteChangesPage {
        match poll {
            ChangesPoll::Page(page) => page,
            ChangesPoll::CursorExpired => panic!("expected a page, got CursorExpired"),
        }
    }

    #[test]
    fn baseline_then_incremental_polls_deliver_new_changes() {
        let (dir, feed, handle, tags) = feed_fixture();
        let root = dir.path();

        let baseline = expect_page(feed.poll(root, &tags, None, 100).expect("baseline"));
        assert!(baseline.changes.is_empty());

        std::fs::write(root.join("a.txt"), b"a").expect("seed a");
        handle.emit_created(root.join("a.txt"), ts(0));

        let page = expect_page(
            feed.poll(root, &tags, Some(&baseline.next_cursor), 100)
                .expect("first increment"),
        );
        assert_eq!(page.changes.len(), 1);
        assert_eq!(page.changes[0].path.as_str(), "a.txt");
        assert_eq!(page.changes[0].kind, RemoteChangeKind::CreatedOrModified);

        // Nothing new: empty page, cursor stable.
        let idle = expect_page(
            feed.poll(root, &tags, Some(&page.next_cursor), 100)
                .expect("idle poll"),
        );
        assert!(idle.changes.is_empty());
        assert_eq!(idle.next_cursor, page.next_cursor);
    }

    #[test]
    fn removed_files_surface_as_removed_changes() {
        let (dir, feed, handle, tags) = feed_fixture();
        let root = dir.path();
        let baseline = expect_page(feed.poll(root, &tags, None, 100).expect("baseline"));

        handle.emit_removed(root.join("gone.txt"), ts(10));
        let page = expect_page(
            feed.poll(root, &tags, Some(&baseline.next_cursor), 100)
                .expect("poll"),
        );
        assert_eq!(page.changes.len(), 1);
        assert_eq!(page.changes[0].kind, RemoteChangeKind::Removed);
    }

    #[test]
    fn internal_files_and_directories_are_invisible() {
        let (dir, feed, handle, tags) = feed_fixture();
        let root = dir.path();
        let baseline = expect_page(feed.poll(root, &tags, None, 100).expect("baseline"));

        std::fs::create_dir_all(root.join("subdir")).expect("dir");
        std::fs::write(root.join(".vapor-tmp-x"), b"t").expect("tmp");
        std::fs::write(root.join("a.txt.vapor-meta.json"), b"{}").expect("side");
        handle.emit_created(root.join("subdir"), ts(0));
        handle.emit_created(root.join(".vapor-tmp-x"), ts(1));
        handle.emit_created(root.join("a.txt.vapor-meta.json"), ts(2));

        let page = expect_page(
            feed.poll(root, &tags, Some(&baseline.next_cursor), 100)
                .expect("poll"),
        );
        assert!(page.changes.is_empty(), "only real payload files surface");
    }

    #[test]
    fn op_id_tags_ride_along_with_changes() {
        let (dir, feed, handle, tags) = feed_fixture();
        let root = dir.path();
        let baseline = expect_page(feed.poll(root, &tags, None, 100).expect("baseline"));

        std::fs::write(root.join("tagged.txt"), b"x").expect("seed");
        tags.write_op_id(&root.join("tagged.txt"), "op-99")
            .expect("tag");
        handle.emit_modified(root.join("tagged.txt"), ts(0));

        let page = expect_page(
            feed.poll(root, &tags, Some(&baseline.next_cursor), 100)
                .expect("poll"),
        );
        assert_eq!(page.changes[0].op_id, Some("op-99".to_string()));
    }

    #[test]
    fn unparseable_and_stale_cursors_expire() {
        let (dir, feed, handle, tags) = feed_fixture();
        let root = dir.path();
        let _ = feed.poll(root, &tags, None, 100).expect("baseline");

        assert_eq!(
            feed.poll(root, &tags, Some("not-a-cursor"), 100)
                .expect("poll"),
            ChangesPoll::CursorExpired
        );
        // A future cursor is equally invalid.
        assert_eq!(
            feed.poll(root, &tags, Some("999999"), 100).expect("poll"),
            ChangesPoll::CursorExpired
        );

        // Overflow the ring: the oldest events fall off, so cursor 0
        // can no longer be replayed.
        for index in 0..(constants::provider::CHANGES_FEED_RING_MAX_EVENTS + 10) {
            let path = root.join(format!("f-{index}.txt"));
            std::fs::write(&path, b"x").expect("seed");
            handle.emit_created(path, ts(index as u64));
        }
        assert_eq!(
            feed.poll(root, &tags, Some("0"), 100).expect("poll"),
            ChangesPoll::CursorExpired
        );
    }

    #[test]
    fn pagination_respects_max_changes() {
        let (dir, feed, handle, tags) = feed_fixture();
        let root = dir.path();
        let baseline = expect_page(feed.poll(root, &tags, None, 100).expect("baseline"));

        for index in 0..5 {
            let path = root.join(format!("f-{index}.txt"));
            std::fs::write(&path, b"x").expect("seed");
            handle.emit_created(path, ts(index));
        }

        let first = expect_page(
            feed.poll(root, &tags, Some(&baseline.next_cursor), 2)
                .expect("page 1"),
        );
        assert_eq!(first.changes.len(), 2);
        let second = expect_page(
            feed.poll(root, &tags, Some(&first.next_cursor), 2)
                .expect("page 2"),
        );
        assert_eq!(second.changes.len(), 2);
        let third = expect_page(
            feed.poll(root, &tags, Some(&second.next_cursor), 2)
                .expect("page 3"),
        );
        assert_eq!(third.changes.len(), 1);
    }
}
