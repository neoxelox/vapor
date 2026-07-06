//! Incremental reconcile comparison walk.
//!
//! Reconcile compares the local tree with the provider's remote tree
//! and enqueues the durable intents that make the two converge under
//! the scope's [`SyncMode`]. The walk is deliberately incremental — a
//! bounded number of directories per checkpoint — so the reconcile
//! controller's slice/throttle discipline (`AGENTS.md §3`) applies to
//! real comparison work: the runtime holds the walker across slice
//! pauses and resumes where it left off.
//!
//! Divergence handling per mode (`docs/architecture/sync-modes.md`):
//! - `two-way`: local-only → upload; remote-only → download; content
//!   divergence routes through the conflict machinery (C8-14) via an
//!   upload whose planner consults the conflict policy.
//! - `pull-only` (strict mirror, cloud authoritative): remote-only and
//!   divergent entries → download; local-only entries → local removal.
//! - `push-only` (strict mirror, local authoritative): local-only and
//!   divergent entries → upload; remote-only entries → remote delete.
//!
//! Fast-path equality is size-based; equal-size same-name files are
//! treated as converged. Content-hash comparison for equal-size pairs
//! is deliberately out of the walk's budget (a whole-tree hash pass
//! would violate the low-impact posture); event-driven intents cover
//! same-size edits because the editing side observes the change.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use vapor_providers::{Provider, ProviderError, RemoteEntry, RemoteEntryKind, RemotePath};
use vapor_shared::{ProviderErrorKind, SyncMode};

use crate::event_intents::PendingIntentKind;
use crate::state_db::{DurableStateDb, StateDbError};

#[derive(Debug)]
pub enum WalkError {
    Provider(ProviderError),
    StateDb(StateDbError),
}

impl From<StateDbError> for WalkError {
    fn from(error: StateDbError) -> Self {
        Self::StateDb(error)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WalkStats {
    pub directories_compared: usize,
    pub intents_enqueued: usize,
    /// Strict-mirror removals scheduled by this walk (C8-65).
    pub mirror_deletes: usize,
    /// Strict-mirror overwrites scheduled by this walk (C8-65).
    pub mirror_reverts: usize,
}

/// One local/remote entry pair keyed by name inside a directory.
#[derive(Debug, Default)]
struct EntryPair {
    local: Option<LocalEntry>,
    remote: Option<RemoteEntry>,
}

#[derive(Debug)]
struct LocalEntry {
    path: PathBuf,
    is_dir: bool,
    size_bytes: u64,
    modified_at: Option<SystemTime>,
}

pub struct ReconcileWalker {
    /// The scope's local root (remote paths derive relative to it).
    scope_root: PathBuf,
    /// The subtree being reconciled (== scope_root for whole-scope).
    subtree_root: PathBuf,
    pending_dirs: VecDeque<PathBuf>,
    stats: WalkStats,
}

impl ReconcileWalker {
    pub fn new(scope_root: &Path, subtree_root: &Path) -> Self {
        Self {
            scope_root: scope_root.to_path_buf(),
            subtree_root: subtree_root.to_path_buf(),
            pending_dirs: VecDeque::from([subtree_root.to_path_buf()]),
            stats: WalkStats::default(),
        }
    }

    pub fn subtree_root(&self) -> &Path {
        &self.subtree_root
    }

    pub fn stats(&self) -> WalkStats {
        self.stats
    }

    /// Drains the per-walk stat deltas accumulated since the last call
    /// (the runtime folds them into its cumulative mirror counters).
    pub fn take_mirror_deltas(&mut self) -> (usize, usize) {
        let deltas = (self.stats.mirror_reverts, self.stats.mirror_deletes);
        self.stats.mirror_reverts = 0;
        self.stats.mirror_deletes = 0;
        deltas
    }

    /// Compares up to `max_directories` directories and durably
    /// enqueues the resulting convergence intents. Returns `Ok(true)`
    /// when the walk has no work left.
    pub fn process(
        &mut self,
        provider: &dyn Provider,
        sync_mode: SyncMode,
        state_db: &mut DurableStateDb,
        max_directories: usize,
        now: SystemTime,
    ) -> Result<bool, WalkError> {
        for _ in 0..max_directories {
            let Some(directory) = self.pending_dirs.pop_front() else {
                return Ok(true);
            };
            self.compare_directory(provider, sync_mode, state_db, &directory, now)?;
            self.stats.directories_compared += 1;
        }
        Ok(self.pending_dirs.is_empty())
    }

    fn compare_directory(
        &mut self,
        provider: &dyn Provider,
        sync_mode: SyncMode,
        state_db: &mut DurableStateDb,
        directory: &Path,
        now: SystemTime,
    ) -> Result<(), WalkError> {
        let remote_dir = if directory == self.scope_root {
            RemotePath::root()
        } else {
            match RemotePath::from_local(&self.scope_root, directory) {
                Some(path) => path,
                None => {
                    crate::logging::warning(
                        "Reconcile walk skipped a directory outside the scope root",
                        &[("directory", directory.display().to_string())],
                    );
                    return Ok(());
                }
            }
        };

        let mut pairs: BTreeMap<String, EntryPair> = BTreeMap::new();

        match fs::read_dir(directory) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                        continue;
                    };
                    if vapor_providers::filesystem::is_internal_file_name(&name) {
                        continue;
                    }
                    let Ok(metadata) = entry.path().symlink_metadata() else {
                        continue;
                    };
                    if metadata.file_type().is_symlink() {
                        continue;
                    }
                    pairs.entry(name).or_default().local = Some(LocalEntry {
                        path: entry.path(),
                        is_dir: metadata.is_dir(),
                        size_bytes: metadata.len(),
                        modified_at: metadata.modified().ok(),
                    });
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                crate::logging::warning(
                    "Reconcile walk cannot read a local directory; skipping it",
                    &[
                        ("directory", directory.display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
            }
        }

        match provider.enumerate(&remote_dir) {
            Ok(entries) => {
                for entry in entries {
                    let Some(name) = entry.path.file_name().map(ToOwned::to_owned) else {
                        continue;
                    };
                    pairs.entry(name).or_default().remote = Some(entry);
                }
            }
            Err(error) if error.kind == ProviderErrorKind::NotFound => {}
            Err(error) => return Err(WalkError::Provider(error)),
        }

        let mut batch: Vec<(PathBuf, PendingIntentKind, SystemTime)> = Vec::new();
        for (name, pair) in pairs {
            let local_path = directory.join(&name);
            match (pair.local, pair.remote) {
                (Some(local), Some(remote)) => match (local.is_dir, remote.kind) {
                    (true, RemoteEntryKind::Directory) => {
                        self.pending_dirs.push_back(local.path);
                    }
                    (false, RemoteEntryKind::File) => {
                        if local.size_bytes != remote.size_bytes {
                            match sync_mode {
                                SyncMode::TwoWay => {
                                    // Divergence in two-way routes through
                                    // the upload planner, where the
                                    // conflict policy decides (C8-14).
                                    batch.push((local_path, PendingIntentKind::Upload, now));
                                }
                                SyncMode::PullOnly => {
                                    batch.push((local_path, PendingIntentKind::Download, now));
                                    self.stats.mirror_reverts += 1;
                                }
                                SyncMode::PushOnly => {
                                    batch.push((local_path, PendingIntentKind::Upload, now));
                                    self.stats.mirror_reverts += 1;
                                }
                            }
                        }
                    }
                    // Type mismatch (file vs directory): resolve in favor
                    // of the mode's source of truth. The clearing intent
                    // and the re-materializing intents share a path, so
                    // the executor's per-path serialization (and retry
                    // backoff for parents that are still blocked) orders
                    // them safely.
                    (local_is_dir, remote_kind) => match sync_mode {
                        SyncMode::PullOnly => {
                            batch.push((
                                local_path.clone(),
                                PendingIntentKind::ApplyRemoteDelete,
                                now,
                            ));
                            self.stats.mirror_deletes += 1;
                            match remote_kind {
                                RemoteEntryKind::Directory => {
                                    self.pending_dirs.push_back(local_path);
                                }
                                RemoteEntryKind::File => {
                                    batch.push((
                                        directory.join(&name),
                                        PendingIntentKind::Download,
                                        now,
                                    ));
                                }
                            }
                        }
                        SyncMode::PushOnly => {
                            batch.push((local_path.clone(), PendingIntentKind::Delete, now));
                            self.stats.mirror_deletes += 1;
                            if local_is_dir {
                                self.pending_dirs.push_back(local_path);
                            } else {
                                batch.push((directory.join(&name), PendingIntentKind::Upload, now));
                            }
                        }
                        SyncMode::TwoWay => {
                            crate::logging::warning(
                                "Reconcile found a file/directory type mismatch in two-way mode; leaving both sides untouched",
                                &[("path", local_path.display().to_string())],
                            );
                        }
                    },
                },
                (Some(local), None) => match sync_mode {
                    SyncMode::TwoWay | SyncMode::PushOnly => {
                        if local.is_dir {
                            self.pending_dirs.push_back(local.path);
                        } else if sync_mode == SyncMode::TwoWay
                            && remote_deletion_wins(state_db, &local_path, local.modified_at)
                        {
                            // Restart-safe deletion replay (C8-16): the
                            // remote deleted this path and the local copy
                            // has not been modified since — finish the
                            // apply instead of resurrecting the file.
                            batch.push((local_path, PendingIntentKind::ApplyRemoteDelete, now));
                        } else {
                            batch.push((local_path, PendingIntentKind::Upload, now));
                        }
                    }
                    SyncMode::PullOnly => {
                        // Strict mirror: local-only content does not
                        // exist at the source of truth — remove it,
                        // recursively for directories.
                        batch.push((local_path, PendingIntentKind::ApplyRemoteDelete, now));
                        self.stats.mirror_deletes += 1;
                    }
                },
                (None, Some(remote)) => match sync_mode {
                    SyncMode::TwoWay | SyncMode::PullOnly => match remote.kind {
                        RemoteEntryKind::Directory => {
                            // Children materialize local parents on
                            // apply; walk deeper to find them.
                            self.pending_dirs.push_back(local_path);
                        }
                        RemoteEntryKind::File => {
                            if sync_mode == SyncMode::TwoWay
                                && local_deletion_wins(state_db, &local_path, remote.modified_at)
                            {
                                // Restart-safe deletion replay (C8-16):
                                // we deleted this path locally and the
                                // remote copy has not changed since —
                                // finish propagating the delete instead
                                // of re-downloading.
                                batch.push((local_path, PendingIntentKind::Delete, now));
                            } else {
                                batch.push((local_path, PendingIntentKind::Download, now));
                            }
                        }
                    },
                    SyncMode::PushOnly => {
                        // Strict mirror: cloud-only content does not
                        // exist at the local source of truth — remove it
                        // (provider delete handles directories).
                        batch.push((local_path, PendingIntentKind::Delete, now));
                        self.stats.mirror_deletes += 1;
                    }
                },
                (None, None) => unreachable!("pair map only holds observed entries"),
            }
        }

        if !batch.is_empty() {
            self.stats.intents_enqueued += state_db.enqueue_intents_coalesced(&batch)?;
        }
        Ok(())
    }
}

/// Whether a remote-origin tombstone should win over a surviving local
/// file: the deletion wins only when the local copy was not modified
/// after the deletion ("data preservation wins over deletion" — a newer
/// local edit uploads instead; C8-17).
fn remote_deletion_wins(
    state_db: &DurableStateDb,
    local_path: &Path,
    local_modified_at: Option<SystemTime>,
) -> bool {
    match state_db.tombstone(local_path) {
        Ok(Some(tombstone)) if tombstone.origin == crate::state_db::TombstoneOrigin::Remote => {
            match local_modified_at {
                Some(modified_at) => modified_at <= tombstone.deleted_at,
                None => true,
            }
        }
        _ => false,
    }
}

/// Whether a local-origin tombstone should win over a surviving remote
/// file: symmetric to [`remote_deletion_wins`] — a remote copy modified
/// after our deletion re-downloads instead.
fn local_deletion_wins(
    state_db: &DurableStateDb,
    local_path: &Path,
    remote_modified_at: SystemTime,
) -> bool {
    match state_db.tombstone(local_path) {
        Ok(Some(tombstone)) if tombstone.origin == crate::state_db::TombstoneOrigin::Local => {
            remote_modified_at <= tombstone.deleted_at
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vapor_platform::fs_caps::{CaseSensitivity, InMemoryFilesystemCapabilities};
    use vapor_providers::FilesystemProvider;

    struct Fixture {
        _temp: tempfile::TempDir,
        local_root: PathBuf,
        cloud_root: PathBuf,
        provider: FilesystemProvider,
        state_db: DurableStateDb,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::TempDir::new().expect("temp dir");
            let local_root = temp.path().join("local");
            let cloud_root = temp.path().join("cloud");
            std::fs::create_dir_all(&local_root).expect("local root");
            std::fs::create_dir_all(&cloud_root).expect("cloud root");
            let local_root = local_root.canonicalize().expect("canonical local");
            let caps = Arc::new(InMemoryFilesystemCapabilities::new(
                true,
                CaseSensitivity::Sensitive,
            ));
            let (provider, _feed) =
                FilesystemProvider::with_manual_feed(&cloud_root, caps).expect("manual provider");
            let state_db = DurableStateDb::open(temp.path().join("state/vapor.sqlite"))
                .expect("open state db");
            Self {
                local_root,
                cloud_root: temp.path().join("cloud"),
                _temp: temp,
                provider,
                state_db,
            }
        }

        fn run_walk(&mut self, mode: SyncMode) -> WalkStats {
            let mut walker = ReconcileWalker::new(&self.local_root, &self.local_root);
            for _ in 0..64 {
                let done = walker
                    .process(&self.provider, mode, &mut self.state_db, 8, ts(0))
                    .expect("walk step");
                if done {
                    break;
                }
            }
            walker.stats()
        }

        fn queued_kinds(&mut self) -> Vec<(PathBuf, PendingIntentKind)> {
            let mut drained = Vec::new();
            while let Some(intent) = self.state_db.lease_next_ready(ts(10)).expect("lease") {
                drained.push((intent.path.clone(), intent.kind));
            }
            drained.sort_by(|a, b| a.0.cmp(&b.0));
            drained
        }
    }

    fn ts(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_750_000_000_000 + ms)
    }

    #[test]
    fn two_way_walk_uploads_local_only_and_downloads_remote_only() {
        let mut fixture = Fixture::new();
        std::fs::create_dir_all(fixture.local_root.join("sub")).expect("dirs");
        std::fs::write(fixture.local_root.join("sub/local-only.txt"), b"L").expect("seed");
        std::fs::create_dir_all(fixture.cloud_root.join("remote-dir")).expect("dirs");
        std::fs::write(fixture.cloud_root.join("remote-dir/remote-only.txt"), b"R").expect("seed");

        let stats = fixture.run_walk(SyncMode::TwoWay);
        assert!(stats.directories_compared >= 3);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![
                (
                    fixture.local_root.join("remote-dir/remote-only.txt"),
                    PendingIntentKind::Download
                ),
                (
                    fixture.local_root.join("sub/local-only.txt"),
                    PendingIntentKind::Upload
                ),
            ]
        );
    }

    #[test]
    fn pull_only_walk_mirrors_cloud_removing_local_only_content() {
        let mut fixture = Fixture::new();
        std::fs::write(fixture.local_root.join("local-only.txt"), b"L").expect("seed");
        std::fs::create_dir_all(fixture.local_root.join("local-only-dir")).expect("dirs");
        std::fs::write(fixture.local_root.join("local-only-dir/nested.txt"), b"N").expect("seed");
        std::fs::write(fixture.cloud_root.join("shared.txt"), b"remote-version").expect("seed");
        std::fs::write(fixture.local_root.join("shared.txt"), b"different-local!").expect("seed");

        let stats = fixture.run_walk(SyncMode::PullOnly);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![
                (
                    fixture.local_root.join("local-only-dir"),
                    PendingIntentKind::ApplyRemoteDelete
                ),
                (
                    fixture.local_root.join("local-only.txt"),
                    PendingIntentKind::ApplyRemoteDelete
                ),
                (
                    fixture.local_root.join("shared.txt"),
                    PendingIntentKind::Download
                ),
            ]
        );
        assert_eq!(stats.mirror_deletes, 2);
        assert_eq!(stats.mirror_reverts, 1);
    }

    #[test]
    fn push_only_walk_mirrors_local_removing_cloud_only_content() {
        let mut fixture = Fixture::new();
        std::fs::write(fixture.cloud_root.join("cloud-only.txt"), b"C").expect("seed");
        std::fs::write(fixture.cloud_root.join("shared.txt"), b"remote-version").expect("seed");
        std::fs::write(fixture.local_root.join("shared.txt"), b"different-local!").expect("seed");
        std::fs::write(fixture.local_root.join("local-only.txt"), b"L").expect("seed");

        let stats = fixture.run_walk(SyncMode::PushOnly);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![
                (
                    fixture.local_root.join("cloud-only.txt"),
                    PendingIntentKind::Delete
                ),
                (
                    fixture.local_root.join("local-only.txt"),
                    PendingIntentKind::Upload
                ),
                (
                    fixture.local_root.join("shared.txt"),
                    PendingIntentKind::Upload
                ),
            ]
        );
        assert_eq!(stats.mirror_deletes, 1);
        assert_eq!(stats.mirror_reverts, 1);
    }

    #[test]
    fn local_tombstone_replays_the_deletion_when_remote_is_unchanged() {
        // C8-16 restart-safe replay: we deleted locally, the propagation
        // was lost, and the remote copy has not changed since — the
        // reconcile finishes the deletion instead of resurrecting it.
        let mut fixture = Fixture::new();
        let remote_file = fixture.cloud_root.join("deleted-here.txt");
        std::fs::write(&remote_file, b"old remote copy").expect("seed remote");

        // Tombstone is NEWER than the remote copy's mtime.
        fixture
            .state_db
            .record_tombstone(
                &fixture.local_root.join("deleted-here.txt"),
                crate::state_db::TombstoneOrigin::Local,
                SystemTime::now() + std::time::Duration::from_secs(60),
            )
            .expect("tombstone");

        fixture.run_walk(SyncMode::TwoWay);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![(
                fixture.local_root.join("deleted-here.txt"),
                PendingIntentKind::Delete
            )],
            "the walk must finish propagating the deletion"
        );
    }

    #[test]
    fn remote_recreation_after_local_tombstone_downloads_again() {
        // The remote copy is newer than our deletion: it was recreated
        // or edited after we deleted — data preservation wins.
        let mut fixture = Fixture::new();
        std::fs::write(fixture.cloud_root.join("recreated.txt"), b"newer").expect("seed remote");

        fixture
            .state_db
            .record_tombstone(
                &fixture.local_root.join("recreated.txt"),
                crate::state_db::TombstoneOrigin::Local,
                SystemTime::now() - std::time::Duration::from_secs(3_600),
            )
            .expect("tombstone");

        fixture.run_walk(SyncMode::TwoWay);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![(
                fixture.local_root.join("recreated.txt"),
                PendingIntentKind::Download
            )]
        );
    }

    #[test]
    fn remote_tombstone_replays_the_local_removal_when_local_is_unchanged() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("deleted-there.txt");
        std::fs::write(&local_file, b"stale local copy").expect("seed local");

        fixture
            .state_db
            .record_tombstone(
                &local_file,
                crate::state_db::TombstoneOrigin::Remote,
                SystemTime::now() + std::time::Duration::from_secs(60),
            )
            .expect("tombstone");

        fixture.run_walk(SyncMode::TwoWay);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![(local_file, PendingIntentKind::ApplyRemoteDelete)],
            "the walk must finish applying the remote deletion"
        );
    }

    #[test]
    fn local_edit_after_remote_tombstone_uploads_instead_of_deleting() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("revived.txt");
        std::fs::write(&local_file, b"edited after the deletion").expect("seed local");

        // Tombstone predates the local file's mtime.
        fixture
            .state_db
            .record_tombstone(
                &local_file,
                crate::state_db::TombstoneOrigin::Remote,
                SystemTime::now() - std::time::Duration::from_secs(3_600),
            )
            .expect("tombstone");

        fixture.run_walk(SyncMode::TwoWay);
        let kinds = fixture.queued_kinds();
        assert_eq!(
            kinds,
            vec![(local_file, PendingIntentKind::Upload)],
            "data preservation must win over the stale deletion"
        );
    }

    #[test]
    fn equal_size_same_name_files_are_treated_as_converged() {
        let mut fixture = Fixture::new();
        std::fs::write(fixture.local_root.join("same.txt"), b"12345").expect("seed");
        std::fs::write(fixture.cloud_root.join("same.txt"), b"12345").expect("seed");

        fixture.run_walk(SyncMode::TwoWay);
        assert!(fixture.queued_kinds().is_empty());
    }

    #[test]
    fn walk_is_incremental_across_process_calls() {
        let mut fixture = Fixture::new();
        for index in 0..5 {
            let dir = fixture.local_root.join(format!("dir-{index}"));
            std::fs::create_dir_all(&dir).expect("dirs");
            std::fs::write(dir.join("f.txt"), b"x").expect("seed");
        }

        let mut walker = ReconcileWalker::new(&fixture.local_root, &fixture.local_root);
        // Budget of 2 directories per call: the root plus one child.
        let first_done = walker
            .process(
                &fixture.provider,
                SyncMode::TwoWay,
                &mut fixture.state_db,
                2,
                ts(0),
            )
            .expect("walk step");
        assert!(!first_done, "five child dirs cannot finish in one call");

        let mut done = false;
        for _ in 0..8 {
            done = walker
                .process(
                    &fixture.provider,
                    SyncMode::TwoWay,
                    &mut fixture.state_db,
                    2,
                    ts(0),
                )
                .expect("walk step");
            if done {
                break;
            }
        }
        assert!(done, "walk must finish within the budget");
        assert_eq!(fixture.queued_kinds().len(), 5, "one upload per file");
    }
}
