//! The contract every `FsWatcher` must keep, run as one body against
//! the in-memory fake on every host and the native watcher on each
//! shipping OS, so the two cannot drift on what an event looks like.
//!
//! The body performs real filesystem actions under a throwaway root
//! and reads what the watcher delivers. The fake sees no OS events, so
//! the test drives it through `reflect`, which mirrors each action as
//! the events a native watcher would report; for the native watcher
//! `reflect` does nothing and the OS is the source. Either way the
//! same assertions run.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

use super::{FsWatcher, FsWatcherError, WatchEvent, WatchEventKind};

/// What the contract does to the tree, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Create,
    Modify,
    RenameAway,
    RenameIn,
    Remove,
}

pub struct ContractHarness<'a> {
    pub start:
        &'a dyn Fn(PathBuf, Sender<WatchEvent>) -> Result<Box<dyn FsWatcher>, FsWatcherError>,
    /// Mirrors an action as the events a native watcher would report,
    /// through a clone of the watcher's own channel; a no-op for a
    /// watcher with an OS behind it.
    pub reflect: &'a dyn Fn(&Sender<WatchEvent>, &Path, Action),
    /// How long a native backend may take to deliver an event.
    pub delivery: Duration,
}

/// Drains events until one about `path` with an acceptable kind
/// arrives or the deadline passes. Every event drained on the way is
/// checked against the shape rules.
fn wait_for(
    receiver: &Receiver<WatchEvent>,
    root: &Path,
    path: &Path,
    accept: &[WatchEventKind],
    started: SystemTime,
    deadline: Duration,
) -> WatchEvent {
    let until = Instant::now() + deadline;
    let mut seen = Vec::new();
    while Instant::now() < until {
        let Ok(event) = receiver.recv_timeout(Duration::from_millis(50)) else {
            continue;
        };
        assert!(
            event.path.is_absolute(),
            "event paths are absolute: {:?}",
            event.path
        );
        assert!(
            event.path.starts_with(root),
            "event paths lie under the watch root: {:?} not under {:?}",
            event.path,
            root
        );
        assert!(
            event.observed_at + Duration::from_secs(5) >= started,
            "observed_at is not before the action that caused it"
        );
        assert!(
            event.observed_at <= SystemTime::now() + Duration::from_secs(5),
            "observed_at is not in the future"
        );
        if event.path == path && accept.contains(&event.kind) {
            return event;
        }
        seen.push(event);
    }
    panic!(
        "no {accept:?} event for {} within {deadline:?}; saw {seen:?}",
        path.display()
    );
}

pub fn run(harness: ContractHarness<'_>) {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let root = vapor_shared::paths::canonicalize(temp.path()).expect("canonical root");
    let (sender, receiver) = std::sync::mpsc::channel();
    let mirror = sender.clone();
    let watcher = (harness.start)(root.clone(), sender).expect("watcher starts");
    assert_eq!(
        watcher.watch_root(),
        root.as_path(),
        "the watch root reads back canonical"
    );
    // A backend needs a moment to arm before the first action.
    std::thread::sleep(harness.delivery.min(Duration::from_millis(500)));

    let file = root.join("contract.txt");
    let started = SystemTime::now();
    std::fs::write(&file, b"first").expect("create");
    (harness.reflect)(&mirror, &file, Action::Create);
    wait_for(
        &receiver,
        &root,
        &file,
        &[WatchEventKind::Created, WatchEventKind::Modified],
        started,
        harness.delivery,
    );

    let started = SystemTime::now();
    std::fs::write(&file, b"second, longer").expect("modify");
    (harness.reflect)(&mirror, &file, Action::Modify);
    wait_for(
        &receiver,
        &root,
        &file,
        &[WatchEventKind::Modified, WatchEventKind::Created],
        started,
        harness.delivery,
    );

    let renamed = root.join("renamed.txt");
    let started = SystemTime::now();
    std::fs::rename(&file, &renamed).expect("rename");
    (harness.reflect)(&mirror, &file, Action::RenameAway);
    (harness.reflect)(&mirror, &renamed, Action::RenameIn);
    // A rename reaches the engine as a removal of the old name and a
    // creation of the new one, or as `Renamed` halves it re-stats.
    wait_for(
        &receiver,
        &root,
        &file,
        &[WatchEventKind::Removed, WatchEventKind::Renamed],
        started,
        harness.delivery,
    );
    wait_for(
        &receiver,
        &root,
        &renamed,
        &[WatchEventKind::Created, WatchEventKind::Renamed],
        started,
        harness.delivery,
    );

    let started = SystemTime::now();
    std::fs::remove_file(&renamed).expect("remove");
    (harness.reflect)(&mirror, &renamed, Action::Remove);
    wait_for(
        &receiver,
        &root,
        &renamed,
        &[WatchEventKind::Removed, WatchEventKind::Renamed],
        started,
        harness.delivery,
    );

    // Dropping the watcher closes the channel: a consumer blocked on
    // it wakes with a disconnect instead of hanging.
    drop(mirror);
    drop(watcher);
    let until = Instant::now() + harness.delivery;
    loop {
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if Instant::now() < until => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("the channel stayed open after the watcher was dropped")
            }
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs_watch::InMemoryFsWatcher;

    #[test]
    fn the_fake_watcher_keeps_the_contract() {
        let start = |root: PathBuf, sender: Sender<WatchEvent>| {
            InMemoryFsWatcher::start(root, sender).map(|w| Box::new(w) as Box<dyn FsWatcher>)
        };
        let reflect = |mirror: &Sender<WatchEvent>, path: &Path, action: Action| {
            let kind = match action {
                Action::Create | Action::RenameIn => WatchEventKind::Created,
                Action::Modify => WatchEventKind::Modified,
                Action::RenameAway | Action::Remove => WatchEventKind::Removed,
            };
            let _ = mirror.send(WatchEvent {
                path: path.to_path_buf(),
                kind,
                observed_at: SystemTime::now(),
            });
        };
        run(ContractHarness {
            start: &start,
            reflect: &reflect,
            delivery: Duration::from_millis(500),
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_native_watcher_keeps_the_contract() {
        let start = |root: PathBuf, sender: Sender<WatchEvent>| {
            crate::fs_watch::start_native_watcher(root, sender)
        };
        let reflect = |_: &Sender<WatchEvent>, _: &Path, _: Action| {};
        run(ContractHarness {
            start: &start,
            reflect: &reflect,
            delivery: Duration::from_secs(10),
        });
    }
}
