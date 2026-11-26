//! A file or a folder that gets another name or another place in the
//! cloud, on a provider that has an id for each object.
//!
//! The provider of these tests is the filesystem provider with what
//! Google Drive adds to it: an id that stays with an object, and a
//! changes feed that names a moved object by its old and its new path.
//! Its state outlives a daemon, as a cloud does, so a test can stop
//! the daemon, change the cloud, and start a daemon again.

use super::tests::{
    LocalTreeState, OfflineFixture, local_tree_state, names_in, offline_fixture_with_synced,
    timestamp_ms,
};
use super::*;
use crate::fs_events::{FsEventKind, FsEventRecord, FsEventRecording};
use crate::sync_directories::SyncScope;
use std::collections::BTreeMap;
use tempfile::TempDir;
use vapor_providers::{
    ChangesPoll, DownloadRequest, FilesystemProvider, ProviderCapabilities, ProviderError,
    RemoteChange, RemoteChangeKind, RemoteChangesPage, RemoteEntry, RemoteEntryKind,
    RemoteIdentity, RemotePath, TransferSession, TransferStep, UploadRequest,
};
use vapor_shared::SyncMode;

/// What the cloud keeps: the id of each object, the changes feed, and
/// a count of what a daemon asked of it.
#[derive(Default)]
struct CloudBook {
    ids: BTreeMap<String, String>,
    next_id: u64,
    feed: Vec<RemoteChange>,
    /// Goes up when the feed loses its history: a cursor of an earlier
    /// time is expired then.
    feed_epoch: u64,
    uploads: usize,
    downloads: usize,
    deletes: usize,
    moves: Vec<(String, String)>,
}

impl CloudBook {
    fn id_of(&mut self, path: &str) -> String {
        if let Some(id) = self.ids.get(path) {
            return id.clone();
        }
        self.next_id += 1;
        let id = format!("id-{}", self.next_id);
        self.ids.insert(path.to_string(), id.clone());
        id
    }

    /// The paths at `path` and under it.
    fn at_or_under(&self, path: &str) -> Vec<String> {
        let below = format!("{path}/");
        self.ids
            .keys()
            .filter(|known| *known == path || known.starts_with(&below))
            .cloned()
            .collect()
    }

    fn rekey(&mut self, from: &str, to: &str) {
        for old in self.at_or_under(from) {
            if let Some(id) = self.ids.remove(&old) {
                self.ids.insert(format!("{to}{}", &old[from.len()..]), id);
            }
        }
    }

    fn forget(&mut self, path: &str) {
        for old in self.at_or_under(path) {
            self.ids.remove(&old);
        }
    }
}

/// The cloud of a test: a folder, and the book that goes with it.
#[derive(Clone)]
struct IdCloud {
    root: PathBuf,
    book: Arc<Mutex<CloudBook>>,
}

impl IdCloud {
    fn book(&self) -> std::sync::MutexGuard<'_, CloudBook> {
        self.book.lock().expect("cloud book")
    }

    /// Another device gives `from` the name or the place `to`. The
    /// feed says so with one entry that names both.
    fn rename(&self, from: &str, to: &str) {
        self.rename_with_no_feed_entry(from, to);
        self.announce(from, to);
    }

    /// A rename of a file after which the cloud reports a new
    /// modification time for it, and the hash of its content.
    fn rename_file_with_a_new_time(&self, from: &str, to: &str) -> SystemTime {
        self.rename_with_no_feed_entry(from, to);
        let target = self.root.join(to);
        let later = std::fs::metadata(&target)
            .and_then(|metadata| metadata.modified())
            .expect("time")
            + Duration::from_secs(90);
        std::fs::File::options()
            .write(true)
            .open(&target)
            .and_then(|file| file.set_modified(later))
            .expect("new time");
        self.announce(from, to);
        let hash = vapor_providers::filesystem::hash_hex_of_bytes(
            &std::fs::read(&target).expect("content"),
        );
        self.book().feed.last_mut().expect("the entry").content_hash = Some(hash);
        later
    }

    /// The feed entry for a rename that was made: the object at `to`
    /// was at `from`.
    fn announce(&self, from: &str, to: &str) {
        let target = self.root.join(to);
        let metadata = std::fs::metadata(&target).expect("renamed entry");
        let mut book = self.book();
        let id = book.id_of(to);
        book.feed.push(RemoteChange {
            path: RemotePath::new(to).expect("path"),
            kind: RemoteChangeKind::Moved {
                from: RemotePath::new(from).expect("path"),
                entry_kind: if metadata.is_dir() {
                    RemoteEntryKind::Directory
                } else {
                    RemoteEntryKind::File
                },
            },
            // The clock of these tests starts at the epoch. A change
            // seen "now" would be work for a far future.
            observed_at: SystemTime::UNIX_EPOCH,
            op_id: None,
            content_hash: None,
            size_bytes: metadata.is_file().then_some(metadata.len()),
            modified_at: metadata
                .is_file()
                .then(|| metadata.modified().ok())
                .flatten(),
            id: Some(id),
        });
    }

    /// The same rename, with nothing in the feed for it.
    fn rename_with_no_feed_entry(&self, from: &str, to: &str) {
        let target = self.root.join(to);
        std::fs::create_dir_all(target.parent().expect("parent")).expect("folders");
        std::fs::rename(self.root.join(from), &target).expect("cloud rename");
        self.book().rekey(from, to);
    }

    /// The feed loses what it held: the cursor of a daemon expires.
    fn lose_the_feed_history(&self) {
        let mut book = self.book();
        book.feed_epoch += 1;
        book.feed.clear();
    }

    /// What a daemon asked of the cloud so far: uploads, downloads,
    /// deletions, and moves.
    fn traffic(&self) -> (usize, usize, usize, Vec<(String, String)>) {
        let book = self.book();
        (
            book.uploads,
            book.downloads,
            book.deletes,
            book.moves.clone(),
        )
    }
}

struct IdProvider {
    inner: FilesystemProvider,
    cloud: IdCloud,
    restored: Arc<Mutex<Vec<RemoteIdentity>>>,
}

impl IdProvider {
    fn with_id(&self, mut entry: RemoteEntry) -> RemoteEntry {
        entry.id = Some(self.cloud.book().id_of(entry.path.as_str()));
        entry
    }
}

/// A transfer that reports the id of its object when it ends.
struct IdSession {
    inner: Box<dyn TransferSession>,
    cloud: IdCloud,
    path: String,
}

impl TransferSession for IdSession {
    fn step(&mut self, max_bytes: u64) -> Result<TransferStep, ProviderError> {
        Ok(match self.inner.step(max_bytes)? {
            TransferStep::Completed(mut outcome) => {
                outcome.remote_id = Some(self.cloud.book().id_of(&self.path));
                TransferStep::Completed(outcome)
            }
            step => step,
        })
    }
    fn step_bytes(&self) -> u64 {
        self.inner.step_bytes()
    }
    fn remaining_bytes(&self) -> Option<u64> {
        self.inner.remaining_bytes()
    }
    fn abort(&mut self) {
        self.inner.abort();
    }
}

impl Provider for IdProvider {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }
    fn content_hash_algorithm(&self) -> vapor_providers::HashAlgorithm {
        self.inner.content_hash_algorithm()
    }
    fn ensure_cloud_root(&self, dir: &str) -> Result<(), ProviderError> {
        self.inner.ensure_cloud_root(dir)
    }
    fn root_identity(&self, dir: &str) -> Result<Option<String>, ProviderError> {
        self.inner.root_identity(dir)
    }
    fn adopt_root(&self, dir: &str, device_id: &str) -> Result<Option<String>, ProviderError> {
        self.inner.adopt_root(dir, device_id)
    }
    fn enumerate(&self, directory: &RemotePath) -> Result<Vec<RemoteEntry>, ProviderError> {
        Ok(self
            .inner
            .enumerate(directory)?
            .into_iter()
            .map(|entry| self.with_id(entry))
            .collect())
    }
    fn stat(&self, path: &RemotePath) -> Result<Option<RemoteEntry>, ProviderError> {
        Ok(self.inner.stat(path)?.map(|entry| self.with_id(entry)))
    }
    fn content_hash(&self, path: &RemotePath) -> Result<String, ProviderError> {
        self.inner.content_hash(path)
    }
    fn begin_upload(
        &self,
        request: UploadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError> {
        let path = request.remote_path.as_str().to_string();
        let inner = self.inner.begin_upload(request)?;
        self.cloud.book().uploads += 1;
        Ok(Box::new(IdSession {
            inner,
            cloud: self.cloud.clone(),
            path,
        }))
    }
    fn begin_download(
        &self,
        request: DownloadRequest,
    ) -> Result<Box<dyn TransferSession>, ProviderError> {
        let path = request.remote_path.as_str().to_string();
        let inner = self.inner.begin_download(request)?;
        self.cloud.book().downloads += 1;
        Ok(Box::new(IdSession {
            inner,
            cloud: self.cloud.clone(),
            path,
        }))
    }
    fn delete(&self, path: &RemotePath, op_id: &str) -> Result<(), ProviderError> {
        self.inner.delete(path, op_id)?;
        let mut book = self.cloud.book();
        book.forget(path.as_str());
        book.deletes += 1;
        Ok(())
    }
    fn move_object(
        &self,
        from: &RemotePath,
        to: &RemotePath,
        op_id: &str,
    ) -> Result<(), ProviderError> {
        self.inner.move_object(from, to, op_id)?;
        let mut book = self.cloud.book();
        book.rekey(from.as_str(), to.as_str());
        book.moves
            .push((from.as_str().to_string(), to.as_str().to_string()));
        Ok(())
    }
    fn poll_changes(
        &self,
        cursor: Option<&str>,
        _max_changes: usize,
    ) -> Result<ChangesPoll, ProviderError> {
        let book = self.cloud.book();
        let head = |book: &CloudBook| format!("{}:{}", book.feed_epoch, book.feed.len());
        let Some(cursor) = cursor else {
            return Ok(ChangesPoll::Page(RemoteChangesPage {
                changes: Vec::new(),
                next_cursor: head(&book),
            }));
        };
        let seen = cursor
            .split_once(':')
            .filter(|(epoch, _)| *epoch == book.feed_epoch.to_string())
            .and_then(|(_, seen)| seen.parse::<usize>().ok())
            .filter(|seen| *seen <= book.feed.len());
        let Some(seen) = seen else {
            return Ok(ChangesPoll::CursorExpired);
        };
        Ok(ChangesPoll::Page(RemoteChangesPage {
            changes: book.feed[seen..].to_vec(),
            next_cursor: head(&book),
        }))
    }
    fn restore_remote_ids(&self, known: Vec<RemoteIdentity>) {
        *self.restored.lock().expect("restored") = known;
    }
}

/// One device with its cloud. The daemon of the device can be stopped
/// and started; the two folders and the state database stay.
struct Device {
    temp: TempDir,
    watch_root: PathBuf,
    cloud: IdCloud,
    sync_mode: SyncMode,
    now_ms: u64,
    running: Option<Running>,
}

struct Running {
    runtime: DaemonRuntime,
    clock: Arc<crate::clock::ManualClock>,
    /// The local tree as the stand-in watcher last saw it.
    watched: LocalTreeState,
    /// What this start handed back to the provider.
    restored: Arc<Mutex<Vec<RemoteIdentity>>>,
    suppressed_echoes: usize,
}

impl Device {
    fn new(sync_mode: SyncMode) -> Self {
        let temp = TempDir::new().expect("temp dir");
        let watch_root = temp.path().join("watch");
        let cloud_root = temp.path().join("cloud");
        std::fs::create_dir_all(&watch_root).expect("watch root");
        std::fs::create_dir_all(&cloud_root).expect("cloud root");
        let watch_root =
            vapor_shared::paths::canonicalize(&watch_root).expect("canonical watch root");
        let cloud_root =
            vapor_shared::paths::canonicalize(&cloud_root).expect("canonical cloud root");
        Self {
            temp,
            watch_root,
            cloud: IdCloud {
                root: cloud_root,
                book: Arc::default(),
            },
            sync_mode,
            now_ms: 0,
            running: None,
        }
    }

    /// A device whose folder holds `files`, synced, with the ids of
    /// the files and of the folders in the index.
    fn synced(sync_mode: SyncMode, files: &[(&str, &[u8])]) -> Self {
        // The first sync runs in two-way: a mirror would remove what
        // only one side holds.
        let mut device = Self::new(SyncMode::TwoWay);
        for (name, body) in files {
            let path = device.watch_root.join(name);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("folders");
            std::fs::write(&path, body).expect("seed");
        }
        device.start();
        device.converge();
        // The folders are on both sides now: a scan records their ids.
        device.scan();
        device.stop();
        device.sync_mode = sync_mode;
        device
    }

    fn start(&mut self) {
        let state_db = DurableStateDb::open(self.temp.path().join("state/vapor.sqlite"))
            .expect("open durable state db");
        let clock = Arc::new(crate::clock::ManualClock::at_now());
        let caps: Arc<dyn vapor_platform::fs_caps::FilesystemCapabilities> =
            Arc::new(vapor_platform::fs_caps::NativeFilesystemCapabilities::for_current_host());
        let (inner, _feed) = FilesystemProvider::with_manual_feed(&self.cloud.root, caps)
            .expect("manual-feed provider");
        let restored = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = DaemonRuntime::build(
            SyncScope {
                local_root: Some(self.watch_root.clone()),
                cloud_root: self.cloud.root.to_string_lossy().into_owned(),
                sync_mode: self.sync_mode,
            },
            EventPathFilterOptions::default(),
            state_db,
            Box::new(IdProvider {
                inner,
                cloud: self.cloud.clone(),
                restored: restored.clone(),
            }),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            false,
        )
        .expect("runtime");
        runtime.attach_trash(crate::trash::LocalTrash::new(
            "default",
            self.temp.path().join("trash/default"),
            crate::trash::TrashSettings::default(),
            Arc::new(vapor_platform::InMemoryTrashBin::unsupported()),
        ));
        if self.sync_mode != SyncMode::TwoWay {
            runtime.acknowledge_one_way_mode(timestamp_ms(self.now_ms));
        }
        runtime
            .enqueue_startup_reconstruction_reconcile(timestamp_ms(self.now_ms))
            .expect("queue the scan of the start");
        self.running = Some(Running {
            runtime,
            clock,
            watched: local_tree_state(&self.watch_root),
            restored,
            suppressed_echoes: 0,
        });
    }

    fn stop(&mut self) {
        self.running = None;
    }

    fn running(&mut self) -> &mut Running {
        self.running.as_mut().expect("the daemon runs")
    }

    fn runtime(&self) -> &DaemonRuntime {
        &self.running.as_ref().expect("the daemon runs").runtime
    }

    /// Delivers what changed in the local tree the way a watcher does:
    /// a path that went away, and a file that appeared or changed.
    /// That includes what the daemon did itself.
    fn deliver_local_tree_changes(&mut self) {
        let (watch_root, now_ms) = (self.watch_root.clone(), self.now_ms);
        let running = self.running();
        let current = local_tree_state(&watch_root);
        let filter = running.runtime.path_filter.clone();
        let hidden = |path: &Path| {
            filter
                .as_ref()
                .is_some_and(|filter| filter.should_ignore(path))
        };
        let recorder = running.runtime.recorder.clone().expect("recorder");
        let record = |path: &Path, kind: FsEventKind| {
            FsEventRecording::record_event(
                recorder.as_ref(),
                FsEventRecord {
                    path: path.to_path_buf(),
                    kind,
                    observed_at: timestamp_ms(now_ms),
                },
            );
        };
        for path in running
            .watched
            .keys()
            .filter(|path| !current.contains_key(*path) && !hidden(path))
        {
            record(path, FsEventKind::Renamed);
        }
        for (path, state) in &current {
            if running.watched.get(path) != Some(state) && !hidden(path) {
                // A folder that appeared is one event, as a watcher
                // reports a folder that was renamed into place.
                record(path, FsEventKind::Renamed);
                if !state.0 {
                    record(path, FsEventKind::Modified);
                }
            }
        }
        running.watched = current;
    }

    fn tick(&mut self, advance_ms: u64) -> RuntimeTickReport {
        self.deliver_local_tree_changes();
        self.now_ms += advance_ms;
        let now = timestamp_ms(self.now_ms);
        let running = self.running();
        running.clock.advance(Duration::from_millis(advance_ms));
        let report = running
            .runtime
            .tick_with_inputs(now, ThrottleInputs::default())
            .expect("runtime tick");
        running.suppressed_echoes += report.suppressed_local_echoes;
        self.deliver_local_tree_changes();
        report
    }

    /// Ticks until the queue is empty and nothing is in work, with a
    /// few more ticks for what a watcher reports late.
    fn converge(&mut self) {
        let mut quiet_ticks = 0;
        for _ in 0..200 {
            let report = self.tick(6_000);
            let quiet = report.staged_executor.active_total == 0
                && self.runtime().state_db().queue_depth().expect("depth") == 0;
            quiet_ticks = if quiet { quiet_ticks + 1 } else { 0 };
            if quiet_ticks >= 3 {
                return;
            }
        }
        let queue: Vec<_> = self
            .runtime()
            .state_db()
            .list_queue_intents(16)
            .expect("queue")
            .into_iter()
            .map(|intent| {
                (
                    intent.kind,
                    intent.path,
                    intent.attempt_count,
                    intent.last_error,
                )
            })
            .collect();
        let active = self
            .running
            .as_ref()
            .expect("the daemon runs")
            .runtime
            .staged_executor
            .active_stages();
        panic!("the device did not come to rest: queue {queue:?}, in work {active:?}");
    }

    /// A scan of everything, as `vapor reconcile` asks for one.
    fn scan(&mut self) {
        let (root, now) = (self.watch_root.clone(), timestamp_ms(self.now_ms));
        self.running()
            .runtime
            .state_db_mut()
            .enqueue_intents_coalesced(
                &[(root, PendingIntentKind::ReconcileSubtree, now)],
                crate::safeguards::IntentSource::Fresh,
            )
            .expect("queue a scan");
        self.converge();
    }

    fn local(&self, name: &str) -> PathBuf {
        self.watch_root.join(name)
    }

    fn has_row(&self, name: &str) -> bool {
        self.runtime()
            .state_db()
            .sync_index(&self.local(name))
            .expect("read")
            .is_some()
    }

    fn trash_is_empty(&self) -> bool {
        self.runtime().trash().expect("trash").list().is_empty()
    }
}

/// The files under `root`, as paths with `/`, without Vapor's own.
fn files_under(root: &Path) -> Vec<String> {
    let mut files: Vec<String> = local_tree_state(root)
        .into_iter()
        .filter(|(_, state)| !state.0)
        .filter_map(|(path, _)| {
            let below = path.strip_prefix(root).ok()?;
            let text = below.to_string_lossy().replace('\\', "/");
            (!text.contains(".vapor")).then_some(text)
        })
        .collect();
    files.sort();
    files
}

/// The folders under `root`, as paths with `/`.
fn folders_under(root: &Path) -> Vec<String> {
    let mut folders: Vec<String> = local_tree_state(root)
        .into_iter()
        .filter(|(_, state)| state.0)
        .filter_map(|(path, _)| {
            Some(
                path.strip_prefix(root)
                    .ok()?
                    .to_string_lossy()
                    .replace('\\', "/"),
            )
        })
        .collect();
    folders.sort();
    folders
}

const PHOTOS: &[(&str, &[u8])] = &[
    ("Photos/a.jpg", b"the first photo"),
    ("Photos/2026/b.jpg", b"the second photo, a longer one"),
    ("report.txt", b"quarterly numbers"),
];

/// A device with [`PHOTOS`] synced, a file that an OS wrote into the
/// folder, and a build tree that an ignore rule hides.
fn device_with_photos(sync_mode: SyncMode) -> Device {
    let device = Device::synced(sync_mode, PHOTOS);
    std::fs::write(device.local("Photos/.DS_Store"), b"the view").expect("ignored file");
    std::fs::create_dir_all(device.local("Photos/node_modules/pkg")).expect("ignored folder");
    std::fs::write(device.local("Photos/node_modules/pkg/index.js"), b"x").expect("ignored");
    device
}

#[test]
fn a_folder_renamed_in_the_cloud_is_one_rename_on_this_device() {
    let mut device = device_with_photos(SyncMode::TwoWay);
    device.start();
    device.converge();
    let before = device.cloud.traffic();

    device.cloud.rename("Photos", "Pictures");
    device.converge();

    // The same folder under the new name, with everything in it.
    assert_eq!(
        files_under(&device.watch_root),
        vec![
            "Pictures/.DS_Store",
            "Pictures/2026/b.jpg",
            "Pictures/a.jpg",
            "Pictures/node_modules/pkg/index.js",
            "report.txt",
        ]
    );
    assert!(!device.local("Photos").exists());
    // Nothing was sent, fetched, or removed for it.
    assert_eq!(device.cloud.traffic(), before, "no request for the rename");
    assert!(device.trash_is_empty());
    assert_eq!(
        files_under(&device.cloud.root),
        vec!["Pictures/2026/b.jpg", "Pictures/a.jpg", "report.txt"]
    );
    // The index followed.
    assert!(device.has_row("Pictures/a.jpg") && device.has_row("Pictures/2026/b.jpg"));
    assert!(!device.has_row("Photos/a.jpg"));
    assert!(
        !device
            .runtime()
            .state_db()
            .sync_index_has_rows_under(&device.local("Photos"))
            .expect("read")
    );
    // The watcher saw the folder leave and come: none of it went back.
    assert!(device.running().suppressed_echoes > 0);

    // A scan of everything finds nothing to do.
    device.scan();
    assert_eq!(device.cloud.traffic(), before);
    assert_eq!(
        device.runtime().state_db().failed_depth().expect("failed"),
        0
    );
    assert!(
        device
            .runtime()
            .state_db()
            .decisions(false)
            .expect("read")
            .is_empty()
    );
}

#[test]
fn a_folder_moved_into_another_folder_in_the_cloud_is_one_rename_on_this_device() {
    let mut device = device_with_photos(SyncMode::TwoWay);
    device.start();
    device.converge();
    let before = device.cloud.traffic();

    // The folder it moves into does not exist on this device yet.
    device.cloud.rename("Photos", "Archive/2025/Photos");
    device.converge();

    assert_eq!(
        files_under(&device.watch_root),
        vec![
            "Archive/2025/Photos/.DS_Store",
            "Archive/2025/Photos/2026/b.jpg",
            "Archive/2025/Photos/a.jpg",
            "Archive/2025/Photos/node_modules/pkg/index.js",
            "report.txt",
        ]
    );
    assert_eq!(device.cloud.traffic(), before);
    assert!(device.trash_is_empty());
    assert!(device.has_row("Archive/2025/Photos/2026/b.jpg"));
    device.scan();
    assert_eq!(device.cloud.traffic(), before);
}

#[test]
fn a_file_renamed_in_the_cloud_is_one_rename_on_this_device() {
    let mut device = Device::synced(SyncMode::TwoWay, PHOTOS);
    device.start();
    device.converge();
    let before = device.cloud.traffic();

    device.cloud.rename("report.txt", "Reports/2026/final.txt");
    device.converge();

    assert_eq!(
        std::fs::read(device.local("Reports/2026/final.txt")).expect("renamed file"),
        b"quarterly numbers"
    );
    assert!(!device.local("report.txt").exists());
    assert_eq!(device.cloud.traffic(), before, "no request for the rename");
    assert!(device.trash_is_empty());
    assert!(device.has_row("Reports/2026/final.txt") && !device.has_row("report.txt"));
    assert!(device.running().suppressed_echoes > 0);
    device.scan();
    assert_eq!(device.cloud.traffic(), before);
}

#[test]
fn a_renamed_file_that_got_a_new_time_in_the_cloud_is_still_no_change() {
    let mut device = Device::synced(SyncMode::TwoWay, PHOTOS);
    device.start();
    device.converge();
    let before = device.cloud.traffic();

    let new_time = device
        .cloud
        .rename_file_with_a_new_time("report.txt", "final.txt");
    device.converge();

    assert!(device.local("final.txt").exists() && !device.local("report.txt").exists());
    assert_eq!(device.cloud.traffic(), before, "no request for the rename");
    // The index holds the object as the cloud reports it now, so a
    // scan compares the pair as unchanged.
    let row = device
        .runtime()
        .state_db()
        .sync_index(&device.local("final.txt"))
        .expect("read")
        .expect("row");
    assert!(row.matches_remote(17, new_time), "{row:?}");
    device.scan();
    assert_eq!(device.cloud.traffic(), before);
    assert_eq!(
        std::fs::read(device.local("final.txt")).expect("local"),
        b"quarterly numbers"
    );
}

#[test]
fn a_file_changed_under_a_folder_right_after_its_rename_still_syncs() {
    let mut device = Device::synced(SyncMode::TwoWay, PHOTOS);
    device.start();
    device.converge();

    device.cloud.rename("Photos", "Pictures");
    // One tick applies the rename. The user then saves a file in the
    // folder, inside the time the daemon remembers its own rename.
    device.tick(6_000);
    assert!(device.local("Pictures/a.jpg").exists());
    std::fs::write(device.local("Pictures/a.jpg"), b"edited after the rename").expect("edit");
    std::fs::write(device.local("Pictures/new.jpg"), b"a new photo").expect("new file");
    device.converge();

    assert_eq!(
        std::fs::read(device.cloud.root.join("Pictures/a.jpg")).expect("cloud"),
        b"edited after the rename"
    );
    assert_eq!(
        std::fs::read(device.cloud.root.join("Pictures/new.jpg")).expect("cloud"),
        b"a new photo"
    );
}

#[test]
fn a_rename_made_while_the_daemon_was_stopped_is_applied_at_the_next_start() {
    let mut device = device_with_photos(SyncMode::TwoWay);
    let before = device.cloud.traffic();

    // No daemon runs. The feed keeps the entry for the next one.
    device.cloud.rename("Photos", "Pictures");
    device.cloud.rename("report.txt", "final.txt");
    device.start();

    // The provider of this start got the ids of what was synced, so
    // its feed can say which path an entry is about.
    let mut restored: Vec<(String, RemoteEntryKind)> = device
        .running()
        .restored
        .lock()
        .expect("restored")
        .iter()
        .map(|known| (known.path.as_str().to_string(), known.kind))
        .collect();
    restored.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        restored,
        vec![
            ("Photos".to_string(), RemoteEntryKind::Directory),
            ("Photos/2026".to_string(), RemoteEntryKind::Directory),
            ("Photos/2026/b.jpg".to_string(), RemoteEntryKind::File),
            ("Photos/a.jpg".to_string(), RemoteEntryKind::File),
            ("report.txt".to_string(), RemoteEntryKind::File),
        ]
    );

    device.converge();
    assert_eq!(
        files_under(&device.watch_root),
        vec![
            "Pictures/.DS_Store",
            "Pictures/2026/b.jpg",
            "Pictures/a.jpg",
            "Pictures/node_modules/pkg/index.js",
            "final.txt",
        ]
    );
    assert_eq!(device.cloud.traffic(), before, "no request for the renames");
    assert!(device.trash_is_empty());
    assert!(device.has_row("Pictures/a.jpg") && device.has_row("final.txt"));
}

#[test]
fn a_rename_that_the_feed_no_longer_holds_is_paired_by_id_in_the_scan_of_the_start() {
    let mut device = device_with_photos(SyncMode::TwoWay);
    let before = device.cloud.traffic();

    device
        .cloud
        .rename_with_no_feed_entry("Photos", "Archive/Pictures");
    device
        .cloud
        .rename_with_no_feed_entry("report.txt", "final.txt");
    device.cloud.lose_the_feed_history();
    device.start();
    device.converge();

    assert_eq!(
        files_under(&device.watch_root),
        vec![
            "Archive/Pictures/.DS_Store",
            "Archive/Pictures/2026/b.jpg",
            "Archive/Pictures/a.jpg",
            "Archive/Pictures/node_modules/pkg/index.js",
            "final.txt",
        ]
    );
    assert_eq!(device.cloud.traffic(), before, "no request for the renames");
    assert!(device.trash_is_empty());
    assert!(device.has_row("Archive/Pictures/2026/b.jpg") && device.has_row("final.txt"));
    assert!(!device.local("Photos").exists());
}

#[test]
fn a_new_name_that_is_taken_on_this_device_is_merged_file_by_file() {
    a_taken_name_is_merged_file_by_file(true);
}

#[test]
fn a_new_name_that_is_taken_is_merged_file_by_file_when_the_scan_finds_the_rename() {
    a_taken_name_is_merged_file_by_file(false);
}

fn a_taken_name_is_merged_file_by_file(the_feed_has_the_renames: bool) {
    let mut device = Device::synced(SyncMode::TwoWay, PHOTOS);
    // While no daemon runs: a folder and a file that are not synced
    // take the names that the cloud is about to give.
    std::fs::create_dir(device.local("Pictures")).expect("a folder of this device");
    std::fs::write(device.local("Pictures/mine.txt"), b"only on this device").expect("content");
    std::fs::write(device.local("final.txt"), b"another file of this device").expect("file");
    if the_feed_has_the_renames {
        device.cloud.rename("Photos", "Pictures");
        device.cloud.rename("report.txt", "final.txt");
    } else {
        device.cloud.rename_with_no_feed_entry("Photos", "Pictures");
        device
            .cloud
            .rename_with_no_feed_entry("report.txt", "final.txt");
        device.cloud.lose_the_feed_history();
    }
    device.start();
    device.converge();

    // Nothing was replaced.
    assert_eq!(
        std::fs::read(device.local("Pictures/mine.txt")).expect("kept"),
        b"only on this device"
    );
    // The two folders are one: the device's file went up, and the
    // synced files are under the new name on both sides.
    for side in [&device.watch_root, &device.cloud.root] {
        let files = files_under(side);
        for expected in ["Pictures/mine.txt", "Pictures/a.jpg", "Pictures/2026/b.jpg"] {
            assert!(
                files.contains(&expected.to_string()),
                "{expected} in {files:?}"
            );
        }
        assert!(
            !files.iter().any(|file| file.starts_with("Photos/")),
            "{files:?}"
        );
        // Two different files with one name: both are kept.
        let finals: Vec<&String> = files
            .iter()
            .filter(|file| file.starts_with("final"))
            .collect();
        assert_eq!(finals.len(), 2, "{files:?}");
    }
    let mut bodies: Vec<Vec<u8>> = files_under(&device.watch_root)
        .iter()
        .filter(|file| file.starts_with("final"))
        .map(|file| std::fs::read(device.local(file)).expect("read"))
        .collect();
    bodies.sort();
    assert_eq!(
        bodies,
        vec![
            b"another file of this device".to_vec(),
            b"quarterly numbers".to_vec()
        ]
    );
    assert_eq!(
        device.runtime().state_db().failed_depth().expect("failed"),
        0
    );
}

/// Both sides give the folder and the file another name while no
/// daemon runs: this device `Pics` and `mine.txt`, the cloud
/// `Pictures` and `theirs.txt`.
fn device_where_both_sides_renamed(sync_mode: SyncMode) -> Device {
    let device = device_with_photos(sync_mode);
    std::fs::rename(device.local("Photos"), device.local("Pics")).expect("local rename");
    std::fs::rename(device.local("report.txt"), device.local("mine.txt")).expect("local rename");
    device.cloud.rename("Photos", "Pictures");
    device.cloud.rename("report.txt", "theirs.txt");
    device
}

#[test]
fn two_way_keeps_the_name_of_this_device_when_both_sides_renamed() {
    let mut device = device_where_both_sides_renamed(SyncMode::TwoWay);
    let before = device.cloud.traffic();
    device.start();
    device.converge();

    let (uploads, downloads, deletes, moves) = device.cloud.traffic();
    assert_eq!(
        (uploads, downloads, deletes),
        (before.0, before.1, before.2),
        "no file was sent, fetched, or removed"
    );
    // One move of each cloud object, to the name on this device.
    let mut moves = moves[before.3.len()..].to_vec();
    moves.sort();
    assert_eq!(
        moves,
        vec![
            ("Pictures".to_string(), "Pics".to_string()),
            ("theirs.txt".to_string(), "mine.txt".to_string()),
        ]
    );
    assert_eq!(
        files_under(&device.cloud.root),
        vec!["Pics/2026/b.jpg", "Pics/a.jpg", "mine.txt"]
    );
    assert_eq!(folders_under(&device.cloud.root), vec!["Pics", "Pics/2026"]);
    assert_eq!(
        files_under(&device.watch_root),
        vec![
            "Pics/.DS_Store",
            "Pics/2026/b.jpg",
            "Pics/a.jpg",
            "Pics/node_modules/pkg/index.js",
            "mine.txt",
        ]
    );
    assert!(device.trash_is_empty());
    assert!(device.has_row("Pics/a.jpg") && device.has_row("mine.txt"));
    assert!(
        device
            .runtime()
            .state_db()
            .name_carries()
            .expect("read")
            .is_empty()
    );
    assert_eq!(device.runtime().conflict_count(), 0);

    device.scan();
    assert_eq!(
        files_under(&device.cloud.root),
        vec!["Pics/2026/b.jpg", "Pics/a.jpg", "mine.txt"]
    );
}

#[test]
fn pull_only_keeps_the_name_of_the_cloud_when_both_sides_renamed() {
    let mut device = device_where_both_sides_renamed(SyncMode::PullOnly);
    let before = device.cloud.traffic();
    device.start();
    device.converge();

    assert_eq!(device.cloud.traffic(), before, "the cloud is not changed");
    assert_eq!(
        files_under(&device.watch_root),
        vec![
            "Pictures/.DS_Store",
            "Pictures/2026/b.jpg",
            "Pictures/a.jpg",
            "Pictures/node_modules/pkg/index.js",
            "theirs.txt",
        ]
    );
    assert!(device.trash_is_empty(), "the rename removed nothing");
    assert!(device.has_row("Pictures/a.jpg") && device.has_row("theirs.txt"));
}

#[test]
fn push_only_keeps_the_name_of_this_device_when_both_sides_renamed() {
    let mut device = device_where_both_sides_renamed(SyncMode::PushOnly);
    let before = device.cloud.traffic();
    device.start();
    device.converge();

    let (uploads, downloads, deletes, _) = device.cloud.traffic();
    assert_eq!(
        (uploads, downloads, deletes),
        (before.0, before.1, before.2)
    );
    assert_eq!(
        files_under(&device.cloud.root),
        vec!["Pics/2026/b.jpg", "Pics/a.jpg", "mine.txt"]
    );
    assert_eq!(folders_under(&device.cloud.root), vec!["Pics", "Pics/2026"]);
    assert!(device.local("Pics/a.jpg").exists() && device.local("mine.txt").exists());
}

#[test]
fn pull_only_renames_on_this_device_and_push_only_moves_the_cloud_object_back() {
    // Only the cloud renamed.
    let mut pull = device_with_photos(SyncMode::PullOnly);
    let before = pull.cloud.traffic();
    pull.cloud.rename("Photos", "Pictures");
    pull.start();
    pull.converge();
    assert!(pull.local("Pictures/.DS_Store").exists() && !pull.local("Photos").exists());
    assert_eq!(pull.cloud.traffic(), before);
    assert!(pull.trash_is_empty());

    let mut push = device_with_photos(SyncMode::PushOnly);
    let before = push.cloud.traffic();
    push.cloud.rename("Photos", "Pictures");
    push.start();
    push.converge();
    let (uploads, downloads, deletes, moves) = push.cloud.traffic();
    assert_eq!(
        (uploads, downloads, deletes),
        (before.0, before.1, before.2)
    );
    assert_eq!(
        moves[before.3.len()..],
        [("Pictures".to_string(), "Photos".to_string())],
        "the rename in the cloud is undone with one move"
    );
    assert_eq!(
        files_under(&push.cloud.root),
        vec!["Photos/2026/b.jpg", "Photos/a.jpg", "report.txt"]
    );
    assert!(push.local("Photos/a.jpg").exists());
}

#[test]
fn a_rename_on_this_device_that_meets_a_rename_in_the_cloud_while_the_daemon_runs_ends_as_one_folder()
 {
    let mut device = Device::synced(SyncMode::TwoWay, PHOTOS);
    device.start();
    device.converge();
    let before = device.cloud.traffic();

    // The watcher reports the rename of this device first. The cloud
    // has the folder under another name by then, and its feed entry
    // comes with a later poll.
    std::fs::rename(device.local("Photos"), device.local("Pics")).expect("local rename");
    device.cloud.rename_with_no_feed_entry("Photos", "Pictures");
    for _ in 0..3 {
        device.tick(2_000);
    }
    device.cloud.announce("Photos", "Pictures");
    device.converge();

    assert_eq!(
        files_under(&device.cloud.root),
        vec!["Pics/2026/b.jpg", "Pics/a.jpg", "report.txt"]
    );
    assert_eq!(folders_under(&device.cloud.root), vec!["Pics", "Pics/2026"]);
    assert_eq!(folders_under(&device.watch_root), vec!["Pics", "Pics/2026"]);
    let (uploads, downloads, _, _) = device.cloud.traffic();
    assert_eq!(
        (uploads, downloads),
        (before.0, before.1),
        "no file was sent or fetched"
    );
    assert!(device.trash_is_empty());
}

/// A folder with two synced files on the filesystem provider, which
/// has no ids: a rename of the folder in the cloud is worked file by
/// file, and the old folder is left behind on this device.
fn offline_fixture_with_albums() -> OfflineFixture {
    let fixture = OfflineFixture::new();
    std::fs::create_dir(fixture.watch_root.join("albums")).expect("dir");
    std::fs::write(
        fixture.watch_root.join("albums/first.txt"),
        b"the first photo",
    )
    .expect("seed");
    std::fs::write(
        fixture.watch_root.join("albums/second.txt"),
        b"the second photo, a longer one",
    )
    .expect("seed");
    drop(fixture.run(&[]));
    assert_eq!(names_in(&fixture.cloud_root), vec!["albums"]);
    fixture
}

#[test]
fn a_folder_left_with_only_the_files_an_os_wrote_is_removed_and_they_go_to_the_trash() {
    let fixture = offline_fixture_with_albums();
    // While no daemon runs: Finder and Explorer write their files into
    // the folder, and the cloud gives the folder another name.
    for name in [".DS_Store", "Thumbs.db", "desktop.ini", ".directory"] {
        std::fs::write(fixture.watch_root.join("albums").join(name), b"a view").expect("os file");
    }
    std::fs::rename(
        fixture.cloud_root.join("albums"),
        fixture.cloud_root.join("archive"),
    )
    .expect("cloud rename");

    let runtime = fixture.run(&[]);

    assert_eq!(names_in(&fixture.watch_root), vec!["archive"]);
    assert_eq!(
        names_in(&fixture.watch_root.join("archive")),
        vec!["first.txt", "second.txt"]
    );
    // The synced files were moved, so only the four files are in the
    // trash, and each can be put back.
    let mut trashed: Vec<String> = runtime
        .trash()
        .expect("trash")
        .list()
        .into_iter()
        .map(|entry| entry.file_name)
        .collect();
    trashed.sort();
    assert_eq!(
        trashed,
        vec![".DS_Store", ".directory", "Thumbs.db", "desktop.ini"]
    );
    assert_eq!(names_in(&fixture.cloud_root), vec!["archive"]);
    assert_eq!(runtime.state_db().failed_depth().expect("failed"), 0);
    assert!(
        runtime
            .state_db()
            .decisions(false)
            .expect("read")
            .is_empty()
    );
}

#[test]
fn a_folder_left_with_anything_else_stays_as_it_is() {
    for kept in [
        "notes.tmp",
        "node_modules/pkg/index.js",
        "not-synced-yet.txt",
    ] {
        let fixture = offline_fixture_with_albums();
        let albums = fixture.watch_root.join("albums");
        std::fs::write(albums.join(".DS_Store"), b"a view").expect("os file");
        std::fs::create_dir_all(albums.join(kept).parent().expect("parent")).expect("folders");
        std::fs::write(albums.join(kept), b"of the user").expect("another entry");
        std::fs::rename(
            fixture.cloud_root.join("albums"),
            fixture.cloud_root.join("archive"),
        )
        .expect("cloud rename");

        let runtime = fixture.run(&[]);

        assert_eq!(
            names_in(&fixture.watch_root.join("archive"))
                .into_iter()
                .filter(|name| name.ends_with(".txt"))
                .collect::<Vec<_>>(),
            vec!["first.txt", "second.txt"],
            "{kept}"
        );
        // The folder is not changed: the file of the OS is still in it.
        assert_eq!(
            std::fs::read(albums.join(".DS_Store")).expect("still there"),
            b"a view",
            "{kept}"
        );
        assert_eq!(
            std::fs::read(albums.join(kept)).expect("still there"),
            b"of the user",
            "{kept}"
        );
        assert!(
            runtime.trash().expect("trash").list().is_empty(),
            "nothing of the folder went to the trash: {kept}"
        );
        assert_eq!(runtime.state_db().failed_depth().expect("failed"), 0);
    }
}

#[test]
fn a_folder_removed_in_the_cloud_leaves_no_folder_with_a_file_of_the_os_behind() {
    // The device cannot tell a folder that was renamed away from one
    // that was removed: the rule is the same for both.
    let fixture = offline_fixture_with_albums();
    std::fs::write(fixture.watch_root.join("albums/.DS_Store"), b"a view").expect("os file");
    std::fs::remove_dir_all(fixture.cloud_root.join("albums")).expect("cloud removal");

    let runtime = fixture.run(&[]);

    assert!(!fixture.watch_root.join("albums").exists());
    let mut trashed: Vec<String> = runtime
        .trash()
        .expect("trash")
        .list()
        .into_iter()
        .map(|entry| entry.file_name)
        .collect();
    trashed.sort();
    assert_eq!(trashed, vec![".DS_Store", "first.txt", "second.txt"]);
    assert!(
        !fixture.cloud_root.join("albums").exists(),
        "the file of the OS never goes to the cloud"
    );
}

#[test]
fn when_both_sides_changed_the_spelling_of_a_folder_the_spelling_of_this_device_stays() {
    let fixture = OfflineFixture::new();
    std::fs::create_dir(fixture.watch_root.join("photos")).expect("dir");
    std::fs::write(fixture.watch_root.join("photos/a.jpg"), b"first").expect("seed");
    drop(fixture.run(&[]));
    if std::fs::symlink_metadata(fixture.watch_root.join("PHOTOS")).is_err() {
        // A volume that keeps spellings apart sees two plain renames
        // to two names; there is no one name to settle.
        return;
    }
    std::fs::rename(
        fixture.watch_root.join("photos"),
        fixture.watch_root.join("PHOTOS"),
    )
    .expect("local rename");
    std::fs::rename(
        fixture.cloud_root.join("photos"),
        fixture.cloud_root.join("Photos"),
    )
    .expect("cloud rename");

    let runtime = fixture.run(&[]);

    assert_eq!(names_in(&fixture.watch_root), vec!["PHOTOS"]);
    assert_eq!(names_in(&fixture.cloud_root), vec!["PHOTOS"]);
    assert_eq!(
        std::fs::read(fixture.cloud_root.join("PHOTOS/a.jpg")).expect("cloud"),
        b"first"
    );
    assert!(runtime.trash().expect("trash").list().is_empty());
    assert_eq!(runtime.conflict_count(), 0);
    assert_eq!(runtime.state_db().failed_depth().expect("failed"), 0);
    assert!(
        runtime
            .state_db()
            .sync_index(&fixture.watch_root.join("PHOTOS/a.jpg"))
            .expect("read")
            .is_some()
    );
}

#[test]
fn a_one_way_mode_keeps_the_spelling_of_its_source_when_both_sides_changed_it() {
    for (mode, expected) in [
        (SyncMode::PullOnly, "Draft.txt"),
        (SyncMode::PushOnly, "DRAFT.txt"),
    ] {
        let fixture = offline_fixture_with_synced("draft.txt", b"chapter one");
        if std::fs::symlink_metadata(fixture.watch_root.join("DRAFT.TXT")).is_err() {
            return;
        }
        std::fs::rename(
            fixture.watch_root.join("draft.txt"),
            fixture.watch_root.join("DRAFT.txt"),
        )
        .expect("local rename");
        std::fs::rename(
            fixture.cloud_root.join("draft.txt"),
            fixture.cloud_root.join("Draft.txt"),
        )
        .expect("cloud rename");

        let runtime = fixture.run_in_mode(mode);

        assert_eq!(names_in(&fixture.watch_root), vec![expected], "{mode:?}");
        assert_eq!(names_in(&fixture.cloud_root), vec![expected], "{mode:?}");
        assert_eq!(runtime.state_db().failed_depth().expect("failed"), 0);
    }
}

#[test]
fn an_ignored_file_that_stays_in_a_folder_the_cloud_removed_is_kept_and_not_sent() {
    let mut device = Device::synced(SyncMode::TwoWay, PHOTOS);
    std::fs::write(device.local("Photos/notes.tmp"), b"a scratch file").expect("ignored");
    device.start();
    device.converge();
    let before = device.cloud.traffic();

    // The folder is removed in the cloud. The feed has one entry for
    // it, and none for the files it held.
    std::fs::remove_dir_all(device.cloud.root.join("Photos")).expect("cloud removal");
    {
        let mut book = device.cloud.book();
        let id = book.id_of("Photos");
        book.forget("Photos");
        book.feed.push(RemoteChange {
            path: RemotePath::new("Photos").expect("path"),
            kind: RemoteChangeKind::Removed,
            observed_at: SystemTime::UNIX_EPOCH,
            op_id: None,
            content_hash: None,
            size_bytes: None,
            modified_at: None,
            id: Some(id),
        });
    }
    device.converge();

    // The synced files went to the trash. The ignored file is the
    // user's: it stays, with its folder, and it never goes to the
    // cloud, where it would bring the folder back.
    assert_eq!(
        files_under(&device.watch_root),
        vec!["Photos/notes.tmp", "report.txt"]
    );
    assert_eq!(files_under(&device.cloud.root), vec!["report.txt"]);
    assert_eq!(device.cloud.traffic(), before, "nothing was sent");
    assert_eq!(device.runtime().trash().expect("trash").list().len(), 2);
}
