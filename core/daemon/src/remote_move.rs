//! A file or a folder that got another name or another place in the
//! cloud, applied on this device.
//!
//! The provider says that one object went from one path to another (a
//! feed change that names both, or a listing entry whose id the sync
//! index holds under another path). This module makes the same rename
//! on the device in one step: a folder keeps everything in it, ignored
//! files included, and no file is transferred. It also decides what
//! happens when both sides gave the entry another name, by the sync
//! mode and never by a time.
//!
//! Everything here is a metadata operation: a stat, a rename, and
//! writes to the state database. No file content is read and no
//! provider is called, so it runs on the tick thread, where the
//! reconcile walk renames an entry whose spelling changed.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use vapor_providers::RemotePath;
use vapor_providers::tags::OpIdTagStore;
use vapor_shared::SyncMode;

use crate::event_intents::PendingIntentKind;
use crate::state_db::{DurableStateDb, NameCarry, StateDbError};

/// How many index rows of a folder say whether a local folder is the
/// one that the index holds under another path.
const TWIN_EVIDENCE_ROWS: usize = 16;

/// The most entries of one folder that the look for a renamed entry
/// reads. A folder with more gets no pairing: the look runs on the
/// tick thread.
const TWIN_SEARCH_ENTRIES: usize = 4_096;

/// What became of a move that the cloud made.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CloudMove {
    /// This device has the entry under the cloud's path now.
    RenamedOnDevice,
    /// This device had the entry under the cloud's path already, with
    /// its index rows: the feed said what the device knows (a move
    /// that it made itself, most of the time).
    AlreadyThere,
    /// The entry keeps the path it has on this device, and the cloud
    /// object is owed one move to that path.
    OwedToCloud(PathBuf),
    /// Nothing was paired. The caller works file by file; the text
    /// says why.
    FileByFile(String),
}

/// What a rename on this device needs beyond the two paths.
pub(crate) struct DeviceRenames<'a> {
    pub state_db: &'a mut DurableStateDb,
    /// Gets the old and the new path of each rename that is made. The
    /// caller records them as the daemon's own, so the report of the
    /// watcher is not taken for a change of the user.
    pub renamed: &'a mut Vec<(PathBuf, PathBuf)>,
    /// The store of the op-id tags on this device. A tag that is a
    /// file next to its file has to follow a renamed file.
    pub tags: Option<&'a OpIdTagStore>,
    pub local_root: &'a Path,
    pub sync_mode: SyncMode,
    pub now: SystemTime,
}

impl DeviceRenames<'_> {
    /// Settles a move of one cloud object from the mirror of `from` to
    /// the mirror of `to`, where the sync index holds the entry under
    /// `from`.
    ///
    /// - The entry is still at `from` on this device: only the cloud
    ///   changed. `two-way` and `pull-only` rename it here. `push-only`
    ///   keeps the device's path and owes the cloud a move back.
    /// - The entry is under another name in the folder of `from`: both
    ///   sides changed. `two-way` and `push-only` keep the device's
    ///   name and owe the cloud a move to it. `pull-only` renames the
    ///   device's entry to the cloud's name.
    /// - Neither: no pairing.
    pub(crate) fn settle(
        &mut self,
        from: &Path,
        to: &Path,
        is_folder: bool,
    ) -> Result<CloudMove, StateDbError> {
        let sync_mode = self.sync_mode;
        if from == to || self.is_synced_at(to, from, is_folder)? {
            return Ok(CloudMove::AlreadyThere);
        }
        let here = if is_at(from, is_folder) {
            Some(from.to_path_buf())
        } else if self.holds_what_the_index_describes(to, from, is_folder)? {
            // Both sides gave the entry the same name, or a rename of
            // an earlier run was made and its index write was lost.
            Some(to.to_path_buf())
        } else {
            self.renamed_on_this_device(from, is_folder)?
        };
        let Some(here) = here else {
            return Ok(CloudMove::FileByFile(
                "the entry is not on this device under the path that the index holds, \
                 and no entry next to it is the same one under another name"
                    .to_string(),
            ));
        };
        if sync_mode == SyncMode::PushOnly || (sync_mode == SyncMode::TwoWay && here != from) {
            if here == to {
                // The device's name is the cloud's name already.
                self.move_rows(from, to, is_folder)?;
                return Ok(CloudMove::RenamedOnDevice);
            }
            self.owe_cloud_move(&here, from, to)?;
            return Ok(CloudMove::OwedToCloud(here));
        }
        Ok(match self.rename(&here, to, from, is_folder)? {
            Ok(()) => CloudMove::RenamedOnDevice,
            Err(reason) => CloudMove::FileByFile(reason),
        })
    }

    /// Whether the entry is on this device at `to` with its index rows
    /// there, and nothing of it is left at `from`, on the device or in
    /// the index.
    fn is_synced_at(&self, to: &Path, from: &Path, is_folder: bool) -> Result<bool, StateDbError> {
        if !is_at(to, is_folder) || fs::symlink_metadata(from).is_ok() {
            return Ok(false);
        }
        Ok(if is_folder {
            (self.state_db.sync_index_has_rows_under(to)?
                || self.state_db.synced_folder_id(to)?.is_some())
                && !self.state_db.sync_index_has_rows_under(from)?
        } else {
            self.state_db.sync_index(to)?.is_some() && self.state_db.sync_index(from)?.is_none()
        })
    }

    /// Renames the file or the folder at `here` to `to`, and moves the
    /// index rows of `rows_at` to `to`. `Ok(Err(reason))` when the
    /// rename is not made: the entry and the index are as they were
    /// then. The folders above the new path can have been made; the
    /// cloud has them.
    ///
    /// The new path must be free at the moment of the rename. Nothing
    /// is ever replaced.
    pub(crate) fn rename(
        &mut self,
        here: &Path,
        to: &Path,
        rows_at: &Path,
        is_folder: bool,
    ) -> Result<Result<(), String>, StateDbError> {
        if here == to {
            self.move_rows(rows_at, to, is_folder)?;
            return Ok(Ok(()));
        }
        let inside = |path: &Path| path != self.local_root && path.starts_with(self.local_root);
        if !inside(here) || !inside(to) || to.starts_with(here) {
            return Ok(Err(
                "a path of the move is not a path of the local folder".to_string()
            ));
        }
        if !is_at(here, is_folder) {
            return Ok(Err("the old path is not on this device".to_string()));
        }
        // A change of capitals only, on a volume that folds them: the
        // new path resolves to the entry itself.
        let same_entry = crate::name_collision::colliding_local_path(to).as_deref() == Some(here);
        if !same_entry
            && (fs::symlink_metadata(to).is_ok()
                || crate::name_collision::colliding_local_path(to).is_some())
        {
            return Ok(Err("the new path is taken on this device".to_string()));
        }
        if let Some(folder) = to.parent()
            && matches!(
                crate::name_collision::local_spelling(folder),
                crate::name_collision::LocalSpelling::Other(_)
            )
        {
            return Ok(Err(
                "this device stores the folder above the new path under another spelling"
                    .to_string(),
            ));
        }
        // A removal that a scan queued under the old path and that
        // still waits is the same move, read file by file: it is
        // withdrawn below. Any other intent is work on a path that is
        // about to change, and so is a removal that is in work: its
        // file can be in the trash already while its index row is
        // still here, and that row must not move to the new path.
        for path in [here, rows_at] {
            if self
                .state_db
                .intents_in_the_way_at_or_under(path, PendingIntentKind::ApplyRemoteDelete)?
                > 0
            {
                return Ok(Err("work is queued under the old path".to_string()));
            }
        }
        if let Some(folder) = to.parent() {
            if let Err(reason) = crate::executor::verify_within_local_root(self.local_root, to) {
                return Ok(Err(reason));
            }
            if let Err(error) = fs::create_dir_all(folder) {
                return Ok(Err(format!(
                    "cannot make the folder above the new path: {error}"
                )));
            }
        }
        let renamed = if same_entry {
            fs::rename(here, to)
        } else {
            vapor_platform::fs_ops::atomic_noclobber_rename(here, to)
        };
        if let Err(error) = renamed {
            return Ok(Err(format!("the rename failed: {error}")));
        }
        if !is_folder && let Some(tags) = self.tags {
            let _ = tags.relocate_side_file(here, to);
        }
        for path in [here, rows_at] {
            self.state_db
                .discard_queued_of_kind_under(path, PendingIntentKind::ApplyRemoteDelete)?;
        }
        self.move_rows(rows_at, to, is_folder)?;
        self.renamed.push((here.to_path_buf(), to.to_path_buf()));
        // The entry left its folder. A folder that holds nothing of
        // the user now may be gone from the cloud: it is asked about,
        // as after every change from the cloud that empties a folder.
        if here.parent() != to.parent() {
            crate::executor::ask_about_folder_that_lost(
                self.state_db,
                Some(self.local_root),
                self.sync_mode,
                here,
                self.now,
            );
        }
        crate::logging::info(
            "Renamed on this device what got another name or place in the cloud",
            &[
                ("from", here.display().to_string()),
                ("to", to.display().to_string()),
                ("folder", is_folder.to_string()),
            ],
        );
        Ok(Ok(()))
    }

    /// Moves the index rows, the folder rows, and the name aliases of
    /// the entry at `from` to `to`.
    fn move_rows(&mut self, from: &Path, to: &Path, is_folder: bool) -> Result<(), StateDbError> {
        if from == to {
            return Ok(());
        }
        if !is_folder {
            return self.state_db.rekey_sync_index(from, to);
        }
        self.state_db.rekey_sync_index_under(from, to)?;
        if let (Some(remote_from), Some(remote_to)) = (
            RemotePath::from_local(self.local_root, from),
            RemotePath::from_local(self.local_root, to),
        ) {
            self.state_db.rekey_name_aliases_under(
                from,
                to,
                remote_from.as_str(),
                remote_to.as_str(),
            )?;
        }
        Ok(())
    }

    /// Records that the cloud object at the mirror of `cloud_at` goes
    /// to the mirror of `new`, the path of the entry on this device,
    /// and queues the intent whose planning makes the move. The index
    /// holds the rows of the entry under `rows_at`.
    pub(crate) fn owe_cloud_move(
        &mut self,
        new: &Path,
        rows_at: &Path,
        cloud_at: &Path,
    ) -> Result<(), StateDbError> {
        self.state_db.record_name_carry(
            &NameCarry {
                new: new.to_path_buf(),
                rows_at: rows_at.to_path_buf(),
                cloud_at: cloud_at.to_path_buf(),
            },
            self.now,
        )?;
        self.state_db.enqueue_intents_coalesced(
            &[(new.to_path_buf(), PendingIntentKind::Rename, self.now)],
            crate::safeguards::IntentSource::Fresh,
        )?;
        crate::logging::info(
            "The name on this device stays; the cloud object is moved to it",
            &[
                ("device", new.display().to_string()),
                ("cloud", cloud_at.display().to_string()),
            ],
        );
        Ok(())
    }

    /// The entry in the folder of `gone` that is the one the index
    /// holds under `gone`, renamed on this device. `None` when `gone`
    /// is on this device, when the index holds nothing for it, and
    /// when no entry or more than one entry could be it.
    fn renamed_on_this_device(
        &self,
        gone: &Path,
        is_folder: bool,
    ) -> Result<Option<PathBuf>, StateDbError> {
        if crate::name_collision::exactly_present(gone) {
            return Ok(None);
        }
        let Some(folder) = gone.parent() else {
            return Ok(None);
        };
        let Ok(entries) = fs::read_dir(folder) else {
            return Ok(None);
        };
        let mut twin = None;
        for (read, entry) in entries.flatten().enumerate() {
            if read >= TWIN_SEARCH_ENTRIES {
                return Ok(None);
            }
            let candidate = entry.path();
            if candidate == gone
                || entry
                    .file_name()
                    .to_str()
                    .is_none_or(vapor_providers::filesystem::is_internal_file_name)
            {
                continue;
            }
            if self.holds_what_the_index_describes(&candidate, gone, is_folder)? {
                if twin.is_some() {
                    // Two entries could be it: nothing says which.
                    return Ok(None);
                }
                twin = Some(candidate);
            }
        }
        Ok(twin)
    }

    /// Whether the entry at `candidate` is new to the index and holds
    /// what the index rows of `rows_at` describe. For a file that is
    /// its size and its modification time. For a folder it is the size
    /// and the time of the first files of those rows, each at the same
    /// place under the folder.
    fn holds_what_the_index_describes(
        &self,
        candidate: &Path,
        rows_at: &Path,
        is_folder: bool,
    ) -> Result<bool, StateDbError> {
        if candidate == rows_at || !is_at(candidate, is_folder) {
            return Ok(false);
        }
        let matches_row = |path: &Path, row: &crate::state_db::SyncIndexEntry| {
            fs::symlink_metadata(path).is_ok_and(|metadata| {
                metadata.is_file() && row.matches_local(metadata.len(), metadata.modified().ok())
            })
        };
        if !is_folder {
            if self.state_db.sync_index(candidate)?.is_some() {
                return Ok(false);
            }
            return Ok(self
                .state_db
                .sync_index(rows_at)?
                .is_some_and(|row| matches_row(candidate, &row)));
        }
        if self.state_db.sync_index_has_rows_under(candidate)?
            || self.state_db.synced_folder_id(candidate)?.is_some()
        {
            return Ok(false);
        }
        let rows = self
            .state_db
            .sync_index_first_rows_under(rows_at, TWIN_EVIDENCE_ROWS)?;
        Ok(!rows.is_empty()
            && rows.iter().all(|row| {
                row.path
                    .strip_prefix(rows_at)
                    .is_ok_and(|below| matches_row(&candidate.join(below), row))
            }))
    }
}

/// Whether a file (or a folder, for `is_folder`) is on this device at
/// exactly `path`. A link is neither.
fn is_at(path: &Path, is_folder: bool) -> bool {
    crate::name_collision::exactly_present(path)
        && fs::symlink_metadata(path).is_ok_and(|metadata| {
            if is_folder {
                metadata.is_dir()
            } else {
                metadata.is_file()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        state_db: DurableStateDb,
        renamed: Vec<(PathBuf, PathBuf)>,
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_750_000_000)
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::TempDir::new().expect("temp dir");
            let root = temp.path().join("local");
            fs::create_dir_all(&root).expect("root");
            let root = vapor_shared::paths::canonicalize(&root).expect("canonical root");
            Self {
                state_db: DurableStateDb::open(temp.path().join("state/vapor.sqlite"))
                    .expect("open state db"),
                _temp: temp,
                root,
                renamed: Vec::new(),
            }
        }

        /// A file on this device with its index row, as a sync left it.
        fn synced(&mut self, name: &str, body: &[u8]) -> PathBuf {
            let path = self.root.join(name);
            fs::create_dir_all(path.parent().expect("parent")).expect("folders");
            fs::write(&path, body).expect("seed");
            let modified = fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok();
            self.state_db
                .set_sync_index(
                    &path,
                    "hash",
                    body.len() as u64,
                    modified,
                    None,
                    "op",
                    now(),
                )
                .expect("index");
            path
        }

        fn renames(&mut self, sync_mode: SyncMode) -> DeviceRenames<'_> {
            DeviceRenames {
                state_db: &mut self.state_db,
                renamed: &mut self.renamed,
                tags: None,
                local_root: &self.root,
                sync_mode,
                now: now(),
            }
        }

        fn has_row(&self, name: &str) -> bool {
            self.state_db
                .sync_index(&self.root.join(name))
                .expect("read")
                .is_some()
        }
    }

    #[test]
    fn a_folder_is_renamed_with_everything_in_it_and_the_rows_follow() {
        let mut fixture = Fixture::new();
        fixture.synced("Photos/a.jpg", b"aaa");
        fixture.synced("Photos/2026/b.jpg", b"bbbb");
        let root = fixture.root.clone();
        fs::write(root.join("Photos/.DS_Store"), b"view").expect("ignored file");
        fixture
            .state_db
            .record_synced_folder(&root.join("Photos"), "id-photos", now())
            .expect("folder row");

        let outcome = fixture
            .renames(SyncMode::TwoWay)
            .settle(&root.join("Photos"), &root.join("Archive/Pictures"), true)
            .expect("settle");

        assert_eq!(outcome, CloudMove::RenamedOnDevice);
        assert_eq!(
            fixture.renamed,
            vec![(root.join("Photos"), root.join("Archive/Pictures"))],
            "the caller learns of the rename, for the echo of the watcher"
        );
        assert!(!root.join("Photos").exists());
        assert!(root.join("Archive/Pictures/.DS_Store").exists());
        assert!(root.join("Archive/Pictures/2026/b.jpg").exists());
        assert!(fixture.has_row("Archive/Pictures/a.jpg"));
        assert!(fixture.has_row("Archive/Pictures/2026/b.jpg"));
        assert!(!fixture.has_row("Photos/a.jpg"));
        assert_eq!(
            fixture
                .state_db
                .path_of_remote_id("id-photos")
                .expect("read"),
            Some((root.join("Archive/Pictures"), true))
        );
    }

    #[test]
    fn a_new_path_that_is_taken_is_never_replaced() {
        let mut fixture = Fixture::new();
        fixture.synced("Photos/a.jpg", b"aaa");
        fixture.synced("report.txt", b"report");
        let root = fixture.root.clone();
        fs::create_dir(root.join("Pictures")).expect("a folder that is not synced");
        fs::write(root.join("Pictures/mine.txt"), b"mine").expect("content");
        fs::write(root.join("notes.txt"), b"not synced").expect("a file that is not synced");

        let folder = fixture
            .renames(SyncMode::TwoWay)
            .settle(&root.join("Photos"), &root.join("Pictures"), true)
            .expect("settle");
        let file = fixture
            .renames(SyncMode::TwoWay)
            .settle(&root.join("report.txt"), &root.join("notes.txt"), false)
            .expect("settle");

        assert!(matches!(folder, CloudMove::FileByFile(_)), "{folder:?}");
        assert!(matches!(file, CloudMove::FileByFile(_)), "{file:?}");
        assert!(root.join("Photos/a.jpg").exists());
        assert_eq!(
            fs::read(root.join("Pictures/mine.txt")).expect("kept"),
            b"mine"
        );
        assert_eq!(
            fs::read(root.join("notes.txt")).expect("kept"),
            b"not synced"
        );
        assert_eq!(fs::read(root.join("report.txt")).expect("kept"), b"report");
        assert!(fixture.has_row("Photos/a.jpg") && fixture.has_row("report.txt"));
    }

    #[test]
    fn work_that_is_queued_under_the_old_path_stops_the_rename_and_a_removal_is_withdrawn() {
        let mut fixture = Fixture::new();
        let file = fixture.synced("Photos/a.jpg", b"aaa");
        let root = fixture.root.clone();
        let (photos, pictures) = (root.join("Photos"), root.join("Pictures"));

        fixture
            .state_db
            .enqueue_intents_coalesced(
                &[(file.clone(), PendingIntentKind::Upload, now())],
                crate::safeguards::IntentSource::Fresh,
            )
            .expect("an upload waits");
        let busy = fixture
            .renames(SyncMode::TwoWay)
            .settle(&photos, &pictures, true)
            .expect("settle");
        assert!(matches!(busy, CloudMove::FileByFile(_)), "{busy:?}");
        assert!(photos.exists());

        let upload = fixture
            .state_db
            .lease_next_ready(now())
            .expect("lease")
            .expect("the upload");
        fixture.state_db.complete_leased(upload.id).expect("done");
        // A scan read the same move as a removal of each file.
        fixture
            .state_db
            .enqueue_intents_coalesced(
                &[(file, PendingIntentKind::ApplyRemoteDelete, now())],
                crate::safeguards::IntentSource::ReconcileBacklog,
            )
            .expect("a removal waits");
        let renamed = fixture
            .renames(SyncMode::TwoWay)
            .settle(&photos, &pictures, true)
            .expect("settle");
        assert_eq!(renamed, CloudMove::RenamedOnDevice);
        assert_eq!(
            fixture.state_db.queue_depth().expect("depth"),
            0,
            "the removal is withdrawn"
        );
        assert!(pictures.join("a.jpg").exists());
    }

    #[test]
    fn a_removal_that_is_in_work_under_the_old_path_stops_the_rename() {
        // A scan read the move as a removal of each file, and a worker
        // has one of them: the file is in the trash already, and its
        // index row is still under the old path.
        let mut fixture = Fixture::new();
        let file = fixture.synced("Photos/a.jpg", b"aaa");
        fixture.synced("Photos/b.jpg", b"bbb");
        let root = fixture.root.clone();
        let (photos, pictures) = (root.join("Photos"), root.join("Pictures"));
        fixture
            .state_db
            .enqueue_intents_coalesced(
                &[(file.clone(), PendingIntentKind::ApplyRemoteDelete, now())],
                crate::safeguards::IntentSource::ReconcileBacklog,
            )
            .expect("a removal waits");
        fixture
            .state_db
            .lease_next_ready(now())
            .expect("lease")
            .expect("the removal is in work");
        fs::remove_file(&file).expect("the worker took the file");

        let settled = fixture
            .renames(SyncMode::TwoWay)
            .settle(&photos, &pictures, true)
            .expect("settle");

        assert!(matches!(settled, CloudMove::FileByFile(_)), "{settled:?}");
        assert!(
            photos.join("b.jpg").exists() && !pictures.exists(),
            "nothing is renamed"
        );
        // A row under the new path with no file there would read as a
        // file that this device deleted, and the cloud copy would go.
        assert!(fixture.has_row("Photos/a.jpg"));
        assert!(!fixture.has_row("Pictures/a.jpg"));
        assert_eq!(
            fixture.state_db.queue_depth().expect("depth"),
            1,
            "the removal in work keeps its row in the queue"
        );
    }

    #[test]
    fn a_name_that_both_sides_changed_stays_as_the_sync_mode_says() {
        for (mode, expected_on_device) in [
            (SyncMode::TwoWay, "Pics"),
            (SyncMode::PushOnly, "Pics"),
            (SyncMode::PullOnly, "Pictures"),
        ] {
            let mut fixture = Fixture::new();
            fixture.synced("Photos/a.jpg", b"aaa");
            fixture.synced("Photos/b.jpg", b"bbbb");
            let root = fixture.root.clone();
            // This device renamed the folder, and the cloud did too.
            fs::rename(root.join("Photos"), root.join("Pics")).expect("local rename");

            let outcome = fixture
                .renames(mode)
                .settle(&root.join("Photos"), &root.join("Pictures"), true)
                .expect("settle");

            assert!(
                root.join(expected_on_device).join("a.jpg").exists(),
                "{mode:?}"
            );
            if mode == SyncMode::PullOnly {
                assert_eq!(outcome, CloudMove::RenamedOnDevice);
                assert!(fixture.has_row("Pictures/a.jpg"));
                assert!(fixture.state_db.name_carries().expect("read").is_empty());
            } else {
                assert_eq!(outcome, CloudMove::OwedToCloud(root.join("Pics")));
                // The rows wait where they are for the move in the cloud.
                assert!(fixture.has_row("Photos/a.jpg"), "{mode:?}");
                assert_eq!(
                    fixture.state_db.name_carries().expect("read"),
                    vec![NameCarry {
                        new: root.join("Pics"),
                        rows_at: root.join("Photos"),
                        cloud_at: root.join("Pictures"),
                    }]
                );
                let intent = fixture
                    .state_db
                    .lease_next_ready(now())
                    .expect("lease")
                    .expect("the intent that makes the move");
                assert_eq!(intent.path, root.join("Pics"));
            }
        }
    }

    #[test]
    fn push_only_owes_the_cloud_a_move_back_when_only_the_cloud_changed() {
        let mut fixture = Fixture::new();
        fixture.synced("Photos/a.jpg", b"aaa");
        let root = fixture.root.clone();

        let outcome = fixture
            .renames(SyncMode::PushOnly)
            .settle(&root.join("Photos"), &root.join("Pictures"), true)
            .expect("settle");

        assert_eq!(outcome, CloudMove::OwedToCloud(root.join("Photos")));
        assert!(root.join("Photos/a.jpg").exists(), "nothing moved here");
        assert_eq!(
            fixture.state_db.name_carries().expect("read"),
            vec![NameCarry {
                new: root.join("Photos"),
                rows_at: root.join("Photos"),
                cloud_at: root.join("Pictures"),
            }]
        );
    }

    #[test]
    fn an_entry_that_this_device_removed_or_that_two_entries_could_be_is_not_paired() {
        let mut fixture = Fixture::new();
        fixture.synced("report.txt", b"the report");
        let root = fixture.root.clone();
        // The file is gone from this device, and nothing took its place.
        fs::remove_file(root.join("report.txt")).expect("removed here");
        let removed = fixture
            .renames(SyncMode::TwoWay)
            .settle(&root.join("report.txt"), &root.join("final.txt"), false)
            .expect("settle");
        assert!(matches!(removed, CloudMove::FileByFile(_)), "{removed:?}");
        assert!(fixture.has_row("report.txt"), "the row is not moved");

        // Two copies with the size and the time of the row.
        let mut fixture = Fixture::new();
        let original = fixture.synced("report.txt", b"the report");
        let root = fixture.root.clone();
        let modified = fs::metadata(&original)
            .and_then(|metadata| metadata.modified())
            .expect("time");
        for name in ["one.txt", "two.txt"] {
            fs::write(root.join(name), b"the report").expect("copy");
            fs::File::options()
                .write(true)
                .open(root.join(name))
                .and_then(|file| file.set_modified(modified))
                .expect("same time");
        }
        fs::remove_file(&original).expect("removed here");
        let unclear = fixture
            .renames(SyncMode::TwoWay)
            .settle(&original, &root.join("final.txt"), false)
            .expect("settle");
        assert!(matches!(unclear, CloudMove::FileByFile(_)), "{unclear:?}");
        assert!(fixture.state_db.name_carries().expect("read").is_empty());
    }
}
