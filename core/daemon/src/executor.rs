//! Provider-driven staged executor.
//!
//! Every stage performs real work against the local filesystem and
//! the injected [`Provider`].
//! Stage transitions happen on work completion, never on synthetic
//! timers. Local work (hashing) is chunked so one advance call never
//! exceeds its per-tick byte budget; provider work (remote probes,
//! transfer sessions, deletes) is dispatched to the provider-job pool
//! and runs off the tick thread, with outcomes harvested on later
//! ticks: the slice-budget interruptibility discipline (`AGENTS.md
//! §3`) applied without letting provider RTT stall the runtime.
//!
//! Pipeline routes by intent kind:
//! - `Upload` / `Rename` → Planner (probe) → Hash → Upload session job
//! - `Delete` → Planner (probe) → Upload slot (remote-delete job)
//! - `Download` → Planner (probe) → Download session job → atomic local
//!   apply (temp file + rename + op-id tag + self-write record)
//! - `ApplyRemoteDelete` → Planner (probe; local delete + self-write
//!   record)
//!
//! Loop prevention: every completed provider write records into the
//! remote echo cache; every completed local apply records into the
//! local echo cache.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use sha2::{Digest, Sha256};
use vapor_providers::tags::OpIdTagStore;
use vapor_providers::{
    DownloadRequest, ProviderError, RemotePath, RemotePrecondition, TransferSession, UploadRequest,
};
use vapor_shared::constants;

use crate::clock::{Clock, SystemClock};
use crate::event_intents::PendingIntentKind;
use crate::local_hash::{DisplacedFileCheck, LocalFileHash, LocalHashes, TouchedFileCheck};
use crate::provider_jobs::{
    JobContext, ProbeHash, ProbeRequest, ProbeResult, ProviderJobKind, ProviderJobOutcome,
    TransferDirection, TransferPhase,
};
use crate::retry::RetryFailureKind;
use crate::self_write_cache::SelfWriteCache;
use crate::state_db::{DurableIntentRecord, DurableStateDb, StateDbError};
use crate::workgate::WorkgateSnapshot;
use crate::{DaemonApp, workgate::WorkClass};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionStage {
    Planner,
    WaitingForHash,
    Hash,
    WaitingForUpload,
    Upload,
    WaitingForDownload,
    Download,
}

impl ExecutionStage {
    /// The stage name `vapor diagnostics` prints.
    pub fn wire_name(self) -> &'static str {
        use vapor_shared::constants::intent;
        match self {
            Self::Planner => intent::STAGE_PLANNING,
            Self::WaitingForHash => intent::STAGE_WAITING_TO_HASH,
            Self::Hash => intent::STAGE_HASHING,
            Self::WaitingForUpload => intent::STAGE_WAITING_TO_UPLOAD,
            Self::Upload => intent::STAGE_UPLOADING,
            Self::WaitingForDownload => intent::STAGE_WAITING_TO_DOWNLOAD,
            Self::Download => intent::STAGE_DOWNLOADING,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StagedExecutorSnapshot {
    pub active_total: usize,
    pub planner_running: usize,
    pub waiting_for_hash: usize,
    pub hash_running: usize,
    pub waiting_for_upload: usize,
    pub upload_running: usize,
    pub waiting_for_download: usize,
    pub download_running: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StagedExecutorReport {
    pub started: usize,
    pub completed: usize,
    pub retried: usize,
    pub failed: usize,
    /// Downloads discarded at apply time because their target would
    /// alias a differently-cased local file (`(target, existing)`).
    pub name_collisions: Vec<(PathBuf, PathBuf)>,
    /// Strict-mirror local removals performed this advance (pull-only
    /// restore path found no remote counterpart).
    pub mirror_deletes: usize,
    /// The files this advance kept both versions of: one path for each
    /// conflict copy it created.
    pub conflict_paths: Vec<PathBuf>,
    /// Renames carried out as server-side moves instead of re-uploads.
    pub moves: usize,
    /// What this advance removed from the cloud, each with the cause.
    pub cloud_deletes: Vec<(PathBuf, &'static str)>,
    /// A deletion found the local root to be another folder than the
    /// one the profile adopted, and went back to the queue. The runtime
    /// checks the root at once.
    pub local_root_suspect: bool,
    /// A cloud deletion found the cloud root to be another folder than
    /// the one the profile adopted (or found no root, or could not
    /// ask), and went back to the queue before it removed a local
    /// file. The runtime checks the root at once.
    pub cloud_root_suspect: bool,
    /// Files whose name cannot be carried to the cloud, found when an
    /// intent for one was planned. The runtime asks about each one time.
    pub unsyncable_names: Vec<crate::unsyncable::UnsyncableName>,
    /// The provider's message when the cloud had no room for an upload
    /// during this advance. The upload went back to the queue to be
    /// tried later; the runtime tells the user.
    pub cloud_storage_full: Option<String>,
    /// Provider calls that failed because the cloud sync root itself is
    /// gone. The runtime reacts by blocking admission and re-ensuring
    /// the root (self-healing), so these intents retry rather than fail.
    pub cloud_root_unavailable: usize,
    /// The provider's message when it refused the sign-in during this
    /// advance. The runtime holds the profile until the user signs in
    /// again; the intents involved went back to the queue.
    pub authentication_required: Option<String>,
    /// An intent waits for the changes feed to say where a cloud
    /// object went. The runtime polls the feed at once.
    pub remote_change_expected: bool,
}

/// What the queue says about renames in flight, read at most once a
/// tick. A deletion asks whether a queued transfer could be the new
/// name of its file; a folder renamed with ten thousand files plans
/// ten thousand deletions, and each one reading and checking the queue
/// again would cost the tick thread a quadratic amount of stats.
#[derive(Default)]
pub struct MoveHints {
    /// Sizes of the local files that queued uploads name and the sync
    /// index has no row for: a rename's new name looks like that.
    upload_sizes: Option<std::collections::HashSet<u64>>,
    /// Queued downloads whose target is not on this device and has no
    /// index row: a cloud-side rename's new name looks like that.
    download_targets: Option<Vec<PathBuf>>,
    /// For each folder asked about: the folder on the way to it that
    /// the sync index holds under another spelling, when there is one.
    folder_respellings: std::collections::HashMap<PathBuf, Option<Respelling>>,
}

/// A local entry whose name goes to the cloud as one move of the cloud
/// object: the sync index still holds its rows under `old`, and the
/// device stores it as `new`.
///
/// Two cases give one. The name changed only in spelling since its
/// last sync (`Folder` to `FOLDER` on a volume that folds case), which
/// the index shows by itself. Or a row of `name_carries` says so: both
/// sides gave the entry another name, or the cloud gave it one that
/// this profile does not keep.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Respelling {
    old: PathBuf,
    new: PathBuf,
    /// The local path that mirrors where the cloud object is, when a
    /// row of `name_carries` gave this move. `None` when the object is
    /// at the mirror of `old`.
    owed_from: Option<PathBuf>,
}

impl MoveHints {
    fn upload_of_size_is_queued(&mut self, state_db: &DurableStateDb, size_bytes: u64) -> bool {
        self.upload_sizes
            .get_or_insert_with(|| {
                [PendingIntentKind::Upload, PendingIntentKind::Rename]
                    .into_iter()
                    .flat_map(|kind| state_db.queued_paths_of_kind(kind).unwrap_or_default())
                    .filter(|path| state_db.sync_index(path).ok().flatten().is_none())
                    .filter_map(|path| {
                        let metadata = fs::symlink_metadata(&path).ok()?;
                        (metadata.is_file() && crate::name_collision::exactly_present(&path))
                            .then_some(metadata.len())
                    })
                    .collect()
            })
            .contains(&size_bytes)
    }

    /// The respelled folder on the way from the root down to `folder`,
    /// `folder` included. The first folder that has no row under its
    /// own spelling decides: it has rows under another spelling, or
    /// nothing below it ever synced.
    fn folder_respelling(
        &mut self,
        state_db: &DurableStateDb,
        local_root: &Path,
        folder: &Path,
    ) -> Option<Respelling> {
        if let Some(known) = self.folder_respellings.get(folder) {
            return known.clone();
        }
        let mut found = None;
        if let Ok(below) = folder.strip_prefix(local_root) {
            let mut prefix = local_root.to_path_buf();
            for name in below.components() {
                prefix.push(name);
                if state_db.sync_index_has_rows_under(&prefix).unwrap_or(true) {
                    continue;
                }
                found = respelling_of(state_db, &prefix);
                break;
            }
        }
        self.folder_respellings
            .insert(folder.to_path_buf(), found.clone());
        found
    }

    fn download_could_be_a_move(&mut self, state_db: &DurableStateDb, except: &Path) -> bool {
        self.download_targets
            .get_or_insert_with(|| {
                state_db
                    .queued_paths_of_kind(PendingIntentKind::Download)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|path| {
                        !crate::name_collision::exactly_present(path)
                            && state_db.sync_index(path).ok().flatten().is_none()
                    })
                    .collect()
            })
            .iter()
            .any(|path| path != except)
    }
}

/// Everything stage work needs beyond the app + durable queue. The
/// runtime assembles one per tick; tests assemble their own.
pub struct ExecutionEnv<'a> {
    /// Canonical local sync root; `None` means no local scope is
    /// configured and every local-touching intent fails permanent.
    pub local_root: Option<&'a Path>,
    /// The cloud root as the profile configures it. A worker reads its
    /// identity before a cloud deletion removes a local file; `None`
    /// (a fixture with no runtime around it) asks for no such read.
    pub cloud_root: Option<&'a str>,
    /// Sync direction for the scope. Direction gates live in
    /// the planner so no intent kind can bypass them; intents enqueued
    /// before a mode change complete as logged no-ops, which is what
    /// makes a mid-run mode change converge deterministically.
    pub sync_mode: vapor_shared::SyncMode,
    /// Stable device identifier: the conflict-suffix component
    /// and the op-id prefix.
    pub device_id: &'a str,
    /// The profile this executor works for. Each profile numbers its
    /// intents from one, so two profiles of one daemon on one cloud
    /// folder would write under the same op-id without it: one would
    /// take the other's upload for its own echo, and both would stage
    /// their bytes in the same temp file.
    pub profile_id: &'a str,
    /// The provider's content-hash algorithm. Every local hash compared
    /// against an index/outcome/echo hash must use this algorithm: the
    /// index and transfer outcomes carry provider-algorithm hashes (MD5
    /// for Google Drive), so hashing local files with a hard-coded
    /// SHA-256 would never match.
    pub hash_algorithm: vapor_providers::HashAlgorithm,
    /// Daemon-wide bandwidth state: every transfer step asks its
    /// direction's bucket for a byte grant, and a dry bucket holds the
    /// session at its checkpoint until tokens refill.
    pub bandwidth: &'a Arc<crate::bandwidth::TransferBandwidth>,
    /// The tick's view of renames in flight.
    pub move_hints: &'a mut MoveHints,
    /// Op-id tag store for the local side (downloads tag the applied
    /// file so watcher echoes correlate).
    pub tags: &'a OpIdTagStore,
    /// Echo cache keyed by local path (suppresses watcher echoes).
    pub local_echoes: &'a mut SelfWriteCache,
    /// Echo cache keyed by remote path (suppresses feed echoes).
    pub remote_echoes: &'a mut SelfWriteCache,
    /// Where a file Vapor removes on this device goes; `None` unlinks
    /// (the harness fixtures that have no runtime around them). Shared,
    /// because a discard that needs a copy runs on a worker.
    pub trash: Option<&'a Arc<crate::trash::LocalTrash>>,
    /// Changes on this device that are not in the durable queue yet
    /// (events inside their quiet window). The create half of a rename
    /// may be among them, so a deletion waits for it.
    pub local_changes_pending: bool,
    /// Changes from the cloud that the feed has not handed over yet (a
    /// poll in flight, a page that was full). The create half of a
    /// rename made in the cloud may be among them.
    pub remote_changes_pending: bool,
    /// The ignore rules of the scope. An intent that the executor
    /// queues by itself (the upload of a file that a removal kept)
    /// asks them first: an ignored name never syncs. `None` in a
    /// fixture with no rules.
    pub path_filter: Option<&'a crate::fs_events::SharedEventPathFilter>,
}

/// One row of [`StagedExecutor::active_stages`].
pub type ActiveStageDiagnostic = (
    i64,
    PathBuf,
    PendingIntentKind,
    ExecutionStage,
    u64,
    u32,
    String,
);

pub struct StagedExecutor {
    active: BTreeMap<i64, ActiveExecution>,
    active_paths: BTreeSet<PathBuf>,
    clock: Arc<dyn Clock>,
    /// Runs every blocking provider call (probes, begin/step/delete)
    /// off the tick thread. Inline (synchronous) by default for
    /// deterministic tests; the daemon bootstrap switches it to worker
    /// threads.
    jobs: crate::provider_jobs::ProviderJobPool,
    /// The old paths of the spelling-only renames whose move in the
    /// cloud is in flight: one intent carries each, the others wait.
    respells_running: BTreeSet<PathBuf>,
    /// The reads of the cloud root's identity, shared by the workers.
    root_identity: Arc<crate::provider_jobs::RootIdentityReads>,
}

/// Why `try_start` declined an intent. Path-busy is per-path: the
/// runtime keeps admitting the rest of its lease batch, while the
/// capacity variants are global and end the batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartDecision {
    Started,
    /// Another execution is in flight for the same path (per-path
    /// serialization); only this intent must wait.
    PathBusy,
    /// The executor or planner permits are exhausted; no further intent
    /// can start this tick.
    AtCapacity,
}

impl std::fmt::Debug for StagedExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagedExecutor")
            .field("active_count", &self.active.len())
            .finish()
    }
}

struct ActiveExecution {
    intent: DurableIntentRecord,
    stage: ActiveStage,
    stage_started_inst: Instant,
}

/// Post-planner route for work that still needs stages.
#[derive(Debug)]
struct TransferPlan {
    remote_path: RemotePath,
    op_id: String,
    local_path: PathBuf,
    /// Download staging file inside the local root (hidden from the
    /// ingest path filter by its `.vapor-tmp-` prefix).
    staging_path: Option<PathBuf>,
    /// Filled by the hash stage for uploads.
    content_hash: Option<String>,
    /// Write guard chosen by the planner (two-way conflict safety).
    precondition: RemotePrecondition,
    /// Two-way upload whose remote may have changed since the last
    /// sync: compare content hashes at the upload gate. Identical
    /// content converges silently; with `last_synced_hash` known, a
    /// local copy still equal to it means the change is remote-only
    /// (download instead), a remote copy still equal to it means the
    /// change is local-only (guarded overwrite); anything else is a
    /// conflict.
    verify_remote_before_upload: bool,
    /// The content hash the sync index recorded at the last transfer,
    /// when the path has one.
    last_synced_hash: Option<String>,
    /// The planner found no remote object where the index says one was
    /// synced. A local copy still equal to `last_synced_hash` then
    /// means the cloud deleted the file and this device has not
    /// touched it: the deletion applies here, nothing is re-uploaded.
    /// A changed local copy is a modification, which wins.
    remote_gone_since_sync: bool,
    /// The planner found the object still carrying this device's tag,
    /// size, and time, and had no hash to guard an overwrite with (the
    /// index recorded none). The object is what this device wrote: the
    /// upload gate reads its hash and guards the overwrite with that.
    remote_unchanged_since_sync: bool,
    /// (size, mtime) captured when the hash stage opened the file. Lets
    /// the post-upload index write detect a mid-transfer edit and decline
    /// to record a stale mtime.
    hashed_local_state: Option<(u64, Option<SystemTime>)>,
    /// For a download, the op-id tag on the remote object being fetched
    /// (the id of whichever device wrote it). Recorded as the sync
    /// index's `last_op_id` so a later upload of this path can correlate
    /// by op-id instead of falling back to a full-remote-read hash
    /// comparison. `None` for uploads (they record their own op-id).
    remote_op_id: Option<String>,
    /// For a download, the size of the remote object as the planner's
    /// probe saw it. It decides whether the download is a small one.
    remote_size: Option<u64>,
}

enum ActiveStage {
    Planner {
        permit: crate::workgate::WorkPermit,
    },
    /// Remote probe job in flight; the planner permit is held so
    /// planner concurrency bounds in-flight probes exactly as it
    /// bounded inline stats.
    PlannerProbe {
        permit: crate::workgate::WorkPermit,
        pending: PendingPlan,
    },
    WaitingForHash {
        plan: TransferPlan,
    },
    /// The local file is being hashed on a provider-job worker (hash
    /// permit held).
    HashRunning {
        permit: crate::workgate::WorkPermit,
        plan: TransferPlan,
    },
    /// Hashing handed back at a step boundary because the throttle
    /// stopped it; dispatched again when hashing is allowed.
    HashHeld {
        permit: crate::workgate::WorkPermit,
        plan: TransferPlan,
        hasher: StreamingFileHash,
    },
    WaitingForUpload {
        plan: TransferPlan,
    },
    /// Upload-gate content-hash verification job in flight (upload
    /// permit held).
    UploadPreflight {
        permit: crate::workgate::WorkPermit,
        plan: TransferPlan,
    },
    /// Upload session or remote delete running on a provider-job
    /// worker (upload permit held).
    UploadRunning {
        permit: crate::workgate::WorkPermit,
        plan: TransferPlan,
    },
    /// A server-side move standing in for the upload of a file that
    /// vanished at `source` and reappeared at the plan's path with the
    /// same content (upload permit held). Falls back to the upload when
    /// the source is gone or the destination is taken.
    MoveRunning {
        permit: crate::workgate::WorkPermit,
        plan: TransferPlan,
        source: crate::state_db::SyncIndexEntry,
    },
    /// Upload session handed back at its checkpoint (throttle gate
    /// closed or bandwidth dry); re-dispatched when the gate reopens,
    /// and not before `resume_at` when the bandwidth bucket set one.
    UploadHeld {
        permit: crate::workgate::WorkPermit,
        plan: TransferPlan,
        session: Box<dyn TransferSession>,
        resume_at: Option<Instant>,
    },
    /// The move in the cloud of an entry whose name changed only in
    /// spelling on this device (planner permit held). The intent that
    /// found it is planned again when the move is back.
    RespellRunning {
        permit: crate::workgate::WorkPermit,
        respelling: Respelling,
        to: RemotePath,
        op_id: String,
    },
    /// A worker copies a discarded item to the trash on another
    /// volume. `mirror_delete` says whether the removal counts as one
    /// of a strict mirror, and `cloud_folder_waits` that the cloud
    /// holds a folder under the name, to be compared once the item is
    /// gone.
    DiscardRunning {
        mirror_delete: bool,
        cloud_folder_waits: bool,
    },
    WaitingForDownload {
        plan: TransferPlan,
    },
    DownloadRunning {
        permit: crate::workgate::WorkPermit,
        plan: TransferPlan,
    },
    DownloadHeld {
        permit: crate::workgate::WorkPermit,
        plan: TransferPlan,
        session: Box<dyn TransferSession>,
        resume_at: Option<Instant>,
    },
}

/// Planner continuation state carried across a remote probe: which
/// decision tree resumes when the probe result arrives.
enum PendingPlan {
    /// Two-way upload guard: waiting on the remote stat (+ conditional
    /// hash) to pick the write precondition.
    Upload {
        plan: TransferPlan,
        index: Option<crate::state_db::SyncIndexEntry>,
    },
    /// Two-way delete guard: waiting on the remote stat to decide
    /// delete vs preserve-and-download.
    Delete {
        plan: TransferPlan,
        index: Option<crate::state_db::SyncIndexEntry>,
    },
    /// Download planning: best-effort stat for the remote writer's
    /// op-id.
    Download { plan: TransferPlan },
    /// A download whose remote object looks like a synced local file
    /// that moved: waiting on the remote hash to confirm before the
    /// local file is renamed instead of the object downloaded.
    DownloadMoveCheck {
        plan: TransferPlan,
        candidate: crate::state_db::SyncIndexEntry,
    },
    /// A download whose remote object has the size of the content last
    /// synced at this path, while the local file is here: waiting on
    /// the remote hash, because an object that still holds that content
    /// was touched and not changed, and there is nothing to download.
    DownloadUnchangedCheck {
        plan: TransferPlan,
        index: crate::state_db::SyncIndexEntry,
    },
    /// Two-way remote-delete apply: stat decides whether the deletion
    /// is stale (remote recreated) before the local guard runs.
    ApplyRemoteDelete,
    /// One-way remote-delete apply: the mirror removes the local file
    /// once the cloud root is known to be the adopted one.
    MirrorRemoteDelete,
}

/// Outcome of the planner stage.
enum PlanOutcome {
    /// Nothing to do (file vanished, directory event, already
    /// converged). Completes the intent successfully.
    Noop(&'static str),
    /// Upload route (Upload / Rename intents): hash first.
    Upload(TransferPlan),
    /// Remote delete route (Delete intents): straight to the upload
    /// slot for the provider call.
    RemoteDelete(TransferPlan),
    /// Download route (Download intents).
    Download(TransferPlan),
    /// ApplyRemoteDelete completed inline (local deletes are cheap).
    AppliedLocally,
    /// A cloud-side rename applied as a local rename: the synced file
    /// at `source` became the plan's local path, no bytes moved.
    MovedLocally,
    /// The path has a name the remote path model refuses. Nothing can
    /// be done with the intent: it completes, and the user is asked
    /// about the name one time.
    Unsyncable(crate::unsyncable::UnsyncableName),
    /// Not now: the intent goes back to the queue for `delay` without
    /// counting as an attempt (a deletion waiting for the create it may
    /// be the other half of).
    Defer {
        delay: Duration,
        reason: &'static str,
    },
    /// The local removal that a cloud deletion asked for needs a copy:
    /// the volume of the item has no trash folder, so the item goes to
    /// the trash of the runtime directory on another volume. A copy
    /// reads the file, so a worker makes it. `cloud_folder_waits` says
    /// that the cloud holds a folder under the name: its files come
    /// down when the name is free here.
    DiscardOnWorker { cloud_folder_waits: bool },
    /// A cloud deletion is about to remove a local file, and the cloud
    /// root is not known to be the folder the profile adopted. The
    /// intent goes back to the queue, and the runtime checks the root
    /// at once.
    CloudRootSuspect,
    /// A keep-both conflict was detected and resolved during planning:
    /// the local loser moved to its conflict-copy path, the follow-up
    /// intents are durably enqueued, and the original intent completes.
    ConflictResolved,
    /// Planning needs remote information; dispatch the probe and resume
    /// with the matching [`PendingPlan`] when it completes.
    Probe {
        /// Boxed: a request carries the index rows of the local files
        /// its worker reads, and this is the largest outcome by far.
        request: Box<crate::provider_jobs::ProbeRequest>,
        pending: PendingPlan,
    },
    Fail {
        failure: RetryFailureKind,
        message: String,
    },
}

impl StagedExecutor {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            active: BTreeMap::new(),
            active_paths: BTreeSet::new(),
            clock,
            jobs: crate::provider_jobs::ProviderJobPool::inline(),
            respells_running: BTreeSet::new(),
            root_identity: Arc::default(),
        }
    }

    /// Switches provider I/O onto worker threads (production mode).
    /// `waker` is notified on every job completion so the tick loop
    /// harvests promptly. Must be called before any intent starts.
    pub fn enable_worker_threads(&mut self, waker: Option<Arc<crate::runtime::TickWaker>>) {
        debug_assert!(self.active.is_empty());
        // Twice the Idle tier so upload + download can both run at
        // full width, bounded so a many-core machine cannot spawn an
        // unreasonable thread count (threads are lazy + parked anyway).
        let worker_cap = (2
            * crate::throttle::idle_concurrency()
            * (1 + constants::engine::SMALL_TRANSFERS_PER_SLOT))
            .clamp(
                constants::engine::PROVIDER_JOB_WORKERS_MIN,
                constants::engine::PROVIDER_JOB_WORKERS_MAX,
            );
        self.jobs = crate::provider_jobs::ProviderJobPool::threaded(worker_cap, waker);
    }

    pub fn snapshot(&self) -> StagedExecutorSnapshot {
        let mut snapshot = StagedExecutorSnapshot::default();
        for execution in self.active.values() {
            snapshot.active_total += 1;
            match execution.stage_name() {
                ExecutionStage::Planner => snapshot.planner_running += 1,
                ExecutionStage::WaitingForHash => snapshot.waiting_for_hash += 1,
                ExecutionStage::Hash => snapshot.hash_running += 1,
                ExecutionStage::WaitingForUpload => snapshot.waiting_for_upload += 1,
                ExecutionStage::Upload => snapshot.upload_running += 1,
                ExecutionStage::WaitingForDownload => snapshot.waiting_for_download += 1,
                ExecutionStage::Download => snapshot.download_running += 1,
            }
        }
        snapshot
    }

    /// Intent ids of every execution currently held in-process. The
    /// runtime renews their leases before the stale-lease sweep so a long
    /// transfer (or a Suspended stall past the lease timeout) is not
    /// reclaimed out from under a live execution.
    pub fn active_intent_ids(&self) -> Vec<i64> {
        self.active.keys().copied().collect()
    }

    /// Diagnostic view of every active execution: (intent id, path,
    /// kind, stage, elapsed-in-stage, attempt count, last error).
    /// Consumed by the IPC diagnostics surface.
    pub fn active_stages(&self) -> Vec<ActiveStageDiagnostic> {
        let now_inst = self.clock.now();
        self.active
            .values()
            .map(|execution| {
                (
                    execution.intent.id,
                    execution.intent.path.clone(),
                    execution.intent.kind,
                    execution.stage_name(),
                    now_inst
                        .saturating_duration_since(execution.stage_started_inst)
                        .as_millis() as u64,
                    execution.intent.attempt_count,
                    execution.intent.last_error.clone().unwrap_or_default(),
                )
            })
            .collect()
    }

    pub fn try_start(
        &mut self,
        app: &mut DaemonApp,
        intent: DurableIntentRecord,
        _now: SystemTime,
    ) -> StartDecision {
        if self.active.len() >= max_in_flight_items(app.workgate_snapshot()) {
            return StartDecision::AtCapacity;
        }
        // Per-path serialization: two intents on the same path must not
        // run concurrently (a local upload racing its own remote apply
        // would corrupt the loop-prevention bookkeeping). The blocked
        // intent requeues and runs after the active one finishes.
        if self.active_paths.contains(&intent.path) {
            return StartDecision::PathBusy;
        }

        let Ok(permit) = app.try_acquire_work(WorkClass::Planner) else {
            return StartDecision::AtCapacity;
        };

        self.active_paths.insert(intent.path.clone());
        let stage_started_inst = self.clock.now();
        self.active.insert(
            intent.id,
            ActiveExecution {
                intent,
                stage: ActiveStage::Planner { permit },
                stage_started_inst,
            },
        );
        StartDecision::Started
    }

    pub fn advance(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        now: SystemTime,
    ) -> Result<StagedExecutorReport, StateDbError> {
        let mut report = StagedExecutorReport::default();
        let intent_ids: Vec<i64> = self.active.keys().copied().collect();
        self.advance_intents(app, state_db, env, now, &intent_ids, &mut report)?;
        Ok(report)
    }

    /// Advances the given executions (ids that are no longer active are
    /// skipped) and harvests provider-job outcomes before and after, so
    /// chained transitions land in the same call. The runtime also calls
    /// this after queue admission so a freshly-leased intent starts
    /// planning within its lease tick instead of waiting a full tick.
    pub(crate) fn advance_intents(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        now: SystemTime,
        intent_ids: &[i64],
        report: &mut StagedExecutorReport,
    ) -> Result<(), StateDbError> {
        // Push the current throttle gates to the workers so in-flight
        // transfer loops observe Suspended/allow flips between steps.
        let caps = app.workgate_snapshot().caps;
        self.jobs
            .update_gates(caps.allow_uploads, caps.allow_downloads, caps.allow_hashing);

        self.harvest_jobs(app, state_db, env, now, report)?;

        let now_inst = self.clock.now();
        for intent_id in intent_ids {
            let Some(execution) = self.active.remove(intent_id) else {
                continue;
            };
            let path = execution.intent.path.clone();
            let next = self.advance_one(app, state_db, env, execution, now, now_inst, report)?;
            match next {
                Some(execution) => {
                    self.active.insert(*intent_id, execution);
                }
                None => {
                    self.active_paths.remove(&path);
                }
            }
        }

        self.harvest_jobs(app, state_db, env, now, report)?;
        Ok(())
    }

    /// Drains completed provider jobs and applies their outcomes.
    /// Applying an outcome can dispatch a follow-up job; with the
    /// inline (test) pool that follow-up completes immediately, so the
    /// drain loops until no outcome is ready.
    fn harvest_jobs(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        now: SystemTime,
        report: &mut StagedExecutorReport,
    ) -> Result<(), StateDbError> {
        loop {
            let completed = self.jobs.harvest();
            if completed.is_empty() {
                return Ok(());
            }
            let now_inst = self.clock.now();
            for job in completed {
                let Some(execution) = self.active.remove(&job.intent_id) else {
                    // The execution was dropped while its job ran; the
                    // durable row is requeued or re-leased elsewhere.
                    abort_outcome_session(job.outcome);
                    continue;
                };
                let path = execution.intent.path.clone();
                let next = self.apply_job_outcome(
                    app,
                    state_db,
                    env,
                    execution,
                    job.outcome,
                    now,
                    now_inst,
                    report,
                )?;
                match next {
                    Some(execution) => {
                        self.active.insert(execution.intent.id, execution);
                    }
                    None => {
                        self.active_paths.remove(&path);
                    }
                }
            }
        }
    }

    /// Advances one execution. Stages waiting on an in-flight provider
    /// job pass through unchanged; held sessions re-dispatch when their
    /// gate reopens; local stages perform at most one budgeted unit of
    /// work, chaining through zero-cost transitions (permit acquisition,
    /// job dispatch) in the same call.
    /// Returns `None` when the execution finished (completed, retried,
    /// or failed terminally: the durable queue owns it again).
    #[allow(clippy::too_many_arguments)]
    fn advance_one(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        execution: ActiveExecution,
        now: SystemTime,
        now_inst: Instant,
        report: &mut StagedExecutorReport,
    ) -> Result<Option<ActiveExecution>, StateDbError> {
        let ActiveExecution {
            intent,
            stage,
            stage_started_inst,
        } = execution;
        match stage {
            ActiveStage::Planner { permit } => {
                if let Some((respelling, from, to)) =
                    respelling_to_carry(app, state_db, env, &intent)
                {
                    if !self.respells_running.insert(respelling.old.clone()) {
                        // Another intent carries this rename right now.
                        app.release_work(permit);
                        state_db.requeue_leased(
                            intent.id,
                            now + RESPELL_WAIT,
                            Some("waiting for the rename of its folder to reach the cloud"),
                        )?;
                        report.retried += 1;
                        return Ok(None);
                    }
                    let op_id = allocate_op_id(env, &intent, now);
                    self.jobs.dispatch(
                        intent.id,
                        job_context(app, env, &self.clock, &self.root_identity),
                        ProviderJobKind::Move {
                            from,
                            to: to.clone(),
                            op_id: op_id.clone(),
                            cloud_root_if_missing: env.cloud_root.map(str::to_string),
                        },
                    );
                    return Ok(Some(ActiveExecution {
                        intent,
                        stage: ActiveStage::RespellRunning {
                            permit,
                            respelling,
                            to,
                            op_id,
                        },
                        stage_started_inst: now_inst,
                    }));
                }
                if cloud_place_is_about_to_move(state_db, env, app, &intent) {
                    app.release_work(permit);
                    state_db.defer_leased(
                        intent.id,
                        now + RESPELL_WAIT,
                        "waiting for the move of its folder in the cloud",
                    )?;
                    report.retried += 1;
                    return Ok(None);
                }
                let outcome = plan_intent(env, state_db, &intent, now);
                if let PlanOutcome::Probe { request, pending } = outcome {
                    // The planner permit stays held across the probe so
                    // planner concurrency bounds in-flight probes.
                    self.jobs.dispatch(
                        intent.id,
                        job_context(app, env, &self.clock, &self.root_identity),
                        ProviderJobKind::Probe(*request),
                    );
                    return Ok(Some(ActiveExecution {
                        intent,
                        stage: ActiveStage::PlannerProbe { permit, pending },
                        stage_started_inst: now_inst,
                    }));
                }
                app.release_work(permit);
                self.apply_plan_outcome(app, state_db, env, intent, outcome, now, now_inst, report)
            }

            // Waiting on an in-flight provider job: nothing to do until
            // the harvest applies its outcome.
            stage @ (ActiveStage::PlannerProbe { .. }
            | ActiveStage::HashRunning { .. }
            | ActiveStage::UploadPreflight { .. }
            | ActiveStage::UploadRunning { .. }
            | ActiveStage::MoveRunning { .. }
            | ActiveStage::RespellRunning { .. }
            | ActiveStage::DiscardRunning { .. }
            | ActiveStage::DownloadRunning { .. }) => Ok(Some(ActiveExecution {
                intent,
                stage,
                stage_started_inst,
            })),

            ActiveStage::WaitingForHash { plan } => self.step_waiting_for_hash(
                app,
                state_db,
                env,
                intent,
                plan,
                stage_started_inst,
                now,
                now_inst,
                report,
            ),

            ActiveStage::HashHeld {
                permit,
                plan,
                hasher,
            } => Ok(Some(self.dispatch_hash(
                app,
                env,
                intent,
                permit,
                plan,
                hasher,
                stage_started_inst,
            ))),

            ActiveStage::WaitingForUpload { plan } => self.step_waiting_for_upload(
                app,
                state_db,
                env,
                intent,
                plan,
                stage_started_inst,
                now,
                now_inst,
                report,
            ),

            ActiveStage::WaitingForDownload { plan } => self.step_waiting_for_download(
                app,
                state_db,
                env,
                intent,
                plan,
                stage_started_inst,
                now_inst,
            ),

            ActiveStage::UploadHeld {
                permit,
                plan,
                session,
                resume_at,
            } => {
                if !app.workgate_snapshot().caps.allow_uploads
                    || resume_at.is_some_and(|at| now_inst < at)
                {
                    return Ok(Some(ActiveExecution {
                        intent,
                        stage: ActiveStage::UploadHeld {
                            permit,
                            plan,
                            session,
                            resume_at,
                        },
                        stage_started_inst,
                    }));
                }
                self.jobs.dispatch(
                    intent.id,
                    job_context(app, env, &self.clock, &self.root_identity),
                    ProviderJobKind::ResumeTransfer {
                        session,
                        direction: TransferDirection::Upload,
                        displaced: None,
                    },
                );
                Ok(Some(ActiveExecution {
                    intent,
                    stage: ActiveStage::UploadRunning { permit, plan },
                    stage_started_inst,
                }))
            }

            ActiveStage::DownloadHeld {
                permit,
                plan,
                session,
                resume_at,
            } => {
                if !app.workgate_snapshot().caps.allow_downloads
                    || resume_at.is_some_and(|at| now_inst < at)
                {
                    return Ok(Some(ActiveExecution {
                        intent,
                        stage: ActiveStage::DownloadHeld {
                            permit,
                            plan,
                            session,
                            resume_at,
                        },
                        stage_started_inst,
                    }));
                }
                self.jobs.dispatch(
                    intent.id,
                    job_context(app, env, &self.clock, &self.root_identity),
                    ProviderJobKind::ResumeTransfer {
                        session,
                        direction: TransferDirection::Download,
                        displaced: displaced_file_check(env, state_db, &plan),
                    },
                );
                Ok(Some(ActiveExecution {
                    intent,
                    stage: ActiveStage::DownloadRunning { permit, plan },
                    stage_started_inst,
                }))
            }
        }
    }

    /// Routes a planner outcome (from the local planner or a probe
    /// continuation) into the pipeline, chaining straight into the next
    /// stage's admission so zero-cost transitions do not cost a tick.
    #[allow(clippy::too_many_arguments)]
    fn apply_plan_outcome(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        intent: DurableIntentRecord,
        outcome: PlanOutcome,
        now: SystemTime,
        now_inst: Instant,
        report: &mut StagedExecutorReport,
    ) -> Result<Option<ActiveExecution>, StateDbError> {
        match outcome {
            PlanOutcome::Noop(reason) => {
                crate::logging::debug(
                    "Intent completed as a no-op during planning",
                    &[
                        ("intent_id", intent.id.to_string()),
                        ("reason", reason.to_string()),
                    ],
                );
                self.complete(state_db, &intent, report)?;
                Ok(None)
            }
            PlanOutcome::AppliedLocally => {
                self.complete(state_db, &intent, report)?;
                Ok(None)
            }
            PlanOutcome::MovedLocally => {
                report.moves += 1;
                self.complete(state_db, &intent, report)?;
                Ok(None)
            }
            PlanOutcome::Unsyncable(name) => {
                report.unsyncable_names.push(name);
                self.complete(state_db, &intent, report)?;
                Ok(None)
            }
            PlanOutcome::Defer { delay, reason } => {
                crate::logging::debug(
                    "Intent deferred during planning",
                    &[
                        ("path", intent.path.display().to_string()),
                        ("reason", reason.to_string()),
                        ("delay_ms", delay.as_millis().to_string()),
                    ],
                );
                state_db.defer_leased(intent.id, now + delay, reason)?;
                report.retried += 1;
                Ok(None)
            }
            PlanOutcome::DiscardOnWorker { cloud_folder_waits } => {
                if self.discard_copy_must_wait(app, state_db, &intent, now, report)? {
                    return Ok(None);
                }
                Ok(self.dispatch_discard(app, env, intent, false, cloud_folder_waits, now_inst))
            }
            PlanOutcome::CloudRootSuspect => {
                self.hold_for_cloud_root(state_db, &intent, now, report)?;
                Ok(None)
            }
            PlanOutcome::ConflictResolved => {
                report.conflict_paths.push(intent.path.clone());
                self.complete(state_db, &intent, report)?;
                Ok(None)
            }
            PlanOutcome::Upload(plan) => self.step_waiting_for_hash(
                app, state_db, env, intent, plan, now_inst, now, now_inst, report,
            ),
            PlanOutcome::RemoteDelete(plan) => {
                // The last moment before the cloud copy goes. The file
                // is gone from this device, and that is the user's
                // deletion only while the root is still the folder this
                // profile adopted. In a root that was swapped, every
                // file is "gone".
                if let Some(root) = env.local_root
                    && !crate::root_identity::local_root_is_adopted(state_db, root)
                {
                    crate::logging::info(
                        "Held a cloud deletion: the local root is not the folder this profile adopted",
                        &[("path", intent.path.display().to_string())],
                    );
                    state_db.defer_leased(
                        intent.id,
                        now + Duration::from_secs(constants::engine::ROOT_CHECK_INTERVAL_SECONDS),
                        "the local root is not the folder this profile adopted",
                    )?;
                    report.retried += 1;
                    report.local_root_suspect = true;
                    return Ok(None);
                }
                self.step_waiting_for_upload(
                    app, state_db, env, intent, plan, now_inst, now, now_inst, report,
                )
            }
            PlanOutcome::Download(plan) => {
                self.step_waiting_for_download(app, state_db, env, intent, plan, now_inst, now_inst)
            }
            PlanOutcome::Probe { .. } => {
                // Continuations carry every remote fact the planner asked
                // for; asking again would loop.
                self.fail_internal(
                    app,
                    state_db,
                    &intent,
                    "probe continuation requested another probe",
                    now,
                    report,
                )
            }
            PlanOutcome::Fail { failure, message } => {
                self.resolve_failure(app, state_db, &intent, failure, &message, now, report)?;
                Ok(None)
            }
        }
    }

    /// Applies a harvested provider-job outcome to its execution.
    #[allow(clippy::too_many_arguments)]
    fn apply_job_outcome(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        execution: ActiveExecution,
        outcome: ProviderJobOutcome,
        now: SystemTime,
        now_inst: Instant,
        report: &mut StagedExecutorReport,
    ) -> Result<Option<ActiveExecution>, StateDbError> {
        let ActiveExecution {
            intent,
            stage,
            stage_started_inst,
        } = execution;
        match stage {
            ActiveStage::HashRunning { permit, plan } => self.apply_hash_outcome(
                app,
                state_db,
                env,
                intent,
                permit,
                plan,
                outcome,
                stage_started_inst,
                now,
                now_inst,
                report,
            ),
            ActiveStage::PlannerProbe { permit, pending } => {
                let ProviderJobOutcome::Probe(probe) = outcome else {
                    app.release_work(permit);
                    return self.fail_unexpected_outcome(
                        app, state_db, &intent, outcome, "probing", now, report,
                    );
                };
                // Probe results carry raw provider errors whose kind the
                // continuations collapse into a retry classification:
                // surface root-unavailability before it is lost.
                if probe_saw_cloud_root_unavailable(&probe) {
                    report.cloud_root_unavailable += 1;
                }
                let plan_outcome = match pending {
                    PendingPlan::Upload { plan, index } => {
                        continue_plan_upload(state_db, plan, index, &probe, now)
                    }
                    PendingPlan::Delete { plan, index } => continue_plan_delete(
                        state_db,
                        env.sync_mode,
                        &intent,
                        plan,
                        index,
                        &probe,
                        now,
                    ),
                    PendingPlan::Download { plan } => {
                        continue_plan_download(env, state_db, plan, &probe, now)
                    }
                    PendingPlan::DownloadMoveCheck { plan, candidate } => {
                        continue_download_move_check(env, state_db, plan, candidate, &probe, now)
                    }
                    PendingPlan::DownloadUnchangedCheck { plan, index } => {
                        continue_download_unchanged_check(state_db, plan, index, &probe, now)
                    }
                    PendingPlan::ApplyRemoteDelete => {
                        continue_apply_remote_delete(env, state_db, &intent, &probe, now)
                    }
                    PendingPlan::MirrorRemoteDelete => {
                        continue_mirror_remote_delete(env, state_db, &intent, &probe, now)
                    }
                };
                // One continuation may ask for one more fact: a download
                // hashes the remote object after the stat found a local
                // file it may be the moved copy of, or the content it
                // may still hold. The permit stays held across it; the
                // check stage itself never asks again.
                if let PlanOutcome::Probe {
                    request,
                    pending:
                        pending @ (PendingPlan::DownloadMoveCheck { .. }
                        | PendingPlan::DownloadUnchangedCheck { .. }),
                } = plan_outcome
                {
                    self.jobs.dispatch(
                        intent.id,
                        job_context(app, env, &self.clock, &self.root_identity),
                        ProviderJobKind::Probe(*request),
                    );
                    return Ok(Some(ActiveExecution {
                        intent,
                        stage: ActiveStage::PlannerProbe { permit, pending },
                        stage_started_inst: now_inst,
                    }));
                }
                app.release_work(permit);
                self.apply_plan_outcome(
                    app,
                    state_db,
                    env,
                    intent,
                    plan_outcome,
                    now,
                    now_inst,
                    report,
                )
            }

            ActiveStage::UploadPreflight { permit, plan } => match outcome {
                ProviderJobOutcome::Probe(probe) => {
                    // Whatever the local file holds now: the object
                    // may be an upload of this device that no index
                    // row records.
                    if let Some(Ok(remote_hash)) = &probe.content_hash {
                        note_own_write_the_index_does_not_know(
                            app,
                            state_db,
                            env,
                            &plan,
                            remote_hash,
                            probe
                                .stat
                                .as_ref()
                                .and_then(|stat| stat.as_ref().ok())
                                .and_then(|entry| entry.as_ref())
                                .and_then(|entry| entry.op_id.as_deref()),
                        );
                    }
                    match probe.content_hash {
                        Some(Ok(remote_hash))
                            if Some(&remote_hash) == plan.content_hash.as_ref() =>
                        {
                            // Identical content converges silently.
                            app.release_work(permit);
                            let remote_entry = probe
                                .stat
                                .as_ref()
                                .and_then(|stat| stat.as_ref().ok())
                                .and_then(|entry| entry.as_ref());
                            // Nothing was written, so the row takes the
                            // tag that the object carries.
                            let op_id = op_id_of_an_object_left_as_it_is(
                                state_db,
                                &plan,
                                remote_entry.map(|entry| entry.op_id.as_deref()),
                            );
                            record_upload_index(
                                state_db,
                                &plan,
                                &remote_hash,
                                local_size(&plan.local_path),
                                remote_entry.map(|entry| entry.modified_at),
                                &op_id,
                                now,
                            );
                            self.complete(state_db, &intent, report)?;
                            Ok(None)
                        }
                        Some(Ok(remote_hash)) if plan.remote_unchanged_since_sync => {
                            // The object is what this device last wrote (its
                            // tag, size, and time say so), and the index had
                            // no hash to guard with. The hash the object has
                            // now is that guard.
                            let mut plan = plan;
                            plan.precondition = RemotePrecondition::HashEquals(remote_hash);
                            self.dispatch_upload(app, env, intent, permit, plan, now_inst)
                        }
                        Some(Ok(remote_hash))
                            if plan.last_synced_hash.is_some()
                                && plan.content_hash == plan.last_synced_hash =>
                        {
                            // The local copy is exactly what was last synced,
                            // so the change is remote-only: fetch it. Never
                            // a conflict copy of an unchanged file.
                            app.release_work(permit);
                            crate::logging::info(
                                "Remote changed while the local copy stayed at the last sync; downloading instead of uploading",
                                &[
                                    ("path", intent.path.display().to_string()),
                                    ("remote_hash", remote_hash),
                                ],
                            );
                            state_db.enqueue_intent(
                                &intent.path,
                                PendingIntentKind::Download,
                                now,
                            )?;
                            self.complete(state_db, &intent, report)?;
                            Ok(None)
                        }
                        Some(Ok(remote_hash))
                            if plan.last_synced_hash.is_some()
                                && Some(&remote_hash) == plan.last_synced_hash.as_ref() =>
                        {
                            // The remote is exactly what was last synced, so
                            // the change is local-only: a guarded overwrite.
                            let mut plan = plan;
                            plan.precondition = RemotePrecondition::HashEquals(remote_hash);
                            self.dispatch_upload(app, env, intent, permit, plan, now_inst)
                        }
                        Some(Ok(_)) => {
                            // Both copies moved away from the last sync (or
                            // there was none): a genuine conflict.
                            app.release_work(permit);
                            self.finish_as_conflict(app, state_db, env, &intent, now, report)?;
                            Ok(None)
                        }
                        Some(Err(error))
                            if error.kind == vapor_shared::ProviderErrorKind::NotFound =>
                        {
                            // Remote vanished since planning: proceed as a
                            // fresh create, guarded as such so a concurrent
                            // re-creation is caught rather than overwritten.
                            let mut plan = plan;
                            plan.precondition = RemotePrecondition::Absent;
                            self.dispatch_upload(app, env, intent, permit, plan, now_inst)
                        }
                        Some(Err(error)) => {
                            app.release_work(permit);
                            self.resolve_provider_failure(
                                app, state_db, &intent, error, now, report,
                            )?;
                            Ok(None)
                        }
                        None => {
                            app.release_work(permit);
                            self.fail_internal(
                                app,
                                state_db,
                                &intent,
                                "upload preflight probe returned no content hash",
                                now,
                                report,
                            )
                        }
                    }
                }
                other => {
                    app.release_work(permit);
                    self.fail_unexpected_outcome(
                        app,
                        state_db,
                        &intent,
                        other,
                        "preflight",
                        now,
                        report,
                    )
                }
            },

            ActiveStage::MoveRunning {
                permit,
                plan,
                source,
            } => match outcome {
                ProviderJobOutcome::Move(Ok(())) => {
                    app.release_work(permit);
                    let from_remote = state_db
                        .alias_remote_for_local(&source.path)
                        .ok()
                        .flatten()
                        .or_else(|| {
                            RemotePath::from_local(env.local_root?, &source.path)
                                .map(|path| path.as_str().to_string())
                        })
                        .unwrap_or_default();
                    env.remote_echoes.record_delete(&from_remote, now);
                    env.remote_echoes.record_write(
                        plan.remote_path.as_str(),
                        Some(plan.op_id.clone()),
                        Some(source.content_hash.clone()),
                        Some(source.size_bytes),
                        source.remote_modified_at,
                        now,
                    );
                    // The object is the same one: its remote mtime and
                    // content carry over; the local mtime is the new
                    // file's own.
                    let local_modified_at = fs::symlink_metadata(&plan.local_path)
                        .and_then(|metadata| metadata.modified())
                        .ok();
                    write_sync_index_entry(
                        state_db,
                        &plan,
                        &source.content_hash,
                        source.size_bytes,
                        local_modified_at,
                        source.remote_modified_at,
                        &plan.op_id,
                        now,
                    );
                    forget_deleted_path(state_db, &source.path);
                    // The object is the same one, so its id is too.
                    record_remote_id(state_db, &plan.local_path, source.remote_id.as_deref());
                    crate::logging::info(
                        "Moved the cloud object instead of re-uploading a renamed file",
                        &[
                            ("from", source.path.display().to_string()),
                            ("to", plan.local_path.display().to_string()),
                            ("bytes", source.size_bytes.to_string()),
                        ],
                    );
                    report.moves += 1;
                    self.complete(state_db, &intent, report)?;
                    Ok(None)
                }
                ProviderJobOutcome::Move(Err(error))
                    if error.kind == vapor_shared::ProviderErrorKind::NotFound
                        && source.remote_id.is_some()
                        && intent.attempt_count < constants::engine::MOVE_SETTLE_MAX_DEFERRALS =>
                {
                    // The cloud has no object under the old name, and
                    // the provider knows its objects by id: the object
                    // may have another name there that the changes
                    // feed has not said yet. An upload now would put
                    // this device's name in the cloud as a second
                    // object. The feed is read at once, and what it
                    // says pairs the two names.
                    app.release_work(permit);
                    state_db.defer_leased(
                        intent.id,
                        now + Duration::from_secs(constants::engine::MOVE_SETTLE_DELAY_SECONDS),
                        "the cloud object of the old name is not where the index holds it; waiting for the changes feed",
                    )?;
                    report.retried += 1;
                    report.remote_change_expected = true;
                    Ok(None)
                }
                ProviderJobOutcome::Move(Err(error))
                    if matches!(
                        error.kind,
                        vapor_shared::ProviderErrorKind::NotFound
                            | vapor_shared::ProviderErrorKind::PreconditionFailed
                            | vapor_shared::ProviderErrorKind::Permanent
                    ) =>
                {
                    // The source is already gone, the destination is
                    // taken, or the backend cannot move: the plain
                    // upload is always correct.
                    crate::logging::debug(
                        "Server-side move not possible; uploading instead",
                        &[
                            ("path", plan.local_path.display().to_string()),
                            ("error", error.message),
                        ],
                    );
                    self.dispatch_upload(app, env, intent, permit, plan, now_inst)
                }
                ProviderJobOutcome::Move(Err(error)) => {
                    app.release_work(permit);
                    self.resolve_provider_failure(app, state_db, &intent, error, now, report)?;
                    Ok(None)
                }
                other => {
                    app.release_work(permit);
                    self.fail_unexpected_outcome(app, state_db, &intent, other, "move", now, report)
                }
            },

            ActiveStage::RespellRunning {
                permit,
                respelling,
                to,
                op_id,
            } => {
                app.release_work(permit);
                self.respells_running.remove(&respelling.old);
                let result = match outcome {
                    ProviderJobOutcome::Move(result) => result,
                    ProviderJobOutcome::MoveSourceMissing {
                        error,
                        cloud_root_identity,
                    } => {
                        // "No object under the old name" is the word of
                        // whatever folder is at the cloud root now. A
                        // root that is away or swapped holds none of the
                        // files: the index rows stay, and the rename
                        // waits for the root. Dropped rows would leave
                        // the next walk with two spellings and nothing
                        // that says which side changed, and it would
                        // take the cloud's, which undoes the rename.
                        if !cloud_root_is_adopted(state_db, Some(&cloud_root_identity)) {
                            crate::logging::info(
                                "Held a rename by spelling: the cloud root is not confirmed as the folder this profile adopted",
                                &[("path", intent.path.display().to_string())],
                            );
                            state_db.defer_leased(
                                intent.id,
                                now + Duration::from_secs(
                                    constants::engine::ROOT_CHECK_INTERVAL_SECONDS,
                                ),
                                "the cloud root is not confirmed as the folder this profile adopted",
                            )?;
                            report.retried += 1;
                            report.cloud_root_suspect = true;
                            return Ok(None);
                        }
                        Err(error)
                    }
                    other => {
                        return self.fail_unexpected_outcome(
                            app, state_db, &intent, other, "renaming", now, report,
                        );
                    }
                };
                match result {
                    Ok(()) => {
                        finish_respelling(env, state_db, &respelling, &to, &op_id, now)?;
                        report.moves += 1;
                        if respelling.owed_from.is_some()
                            && intent.kind == PendingIntentKind::Rename
                            && intent.path == respelling.new
                        {
                            // This intent was queued to make the move,
                            // and the move is made.
                            self.complete(state_db, &intent, report)?;
                            return Ok(None);
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind,
                            vapor_shared::ProviderErrorKind::NotFound
                                | vapor_shared::ProviderErrorKind::PreconditionFailed
                                | vapor_shared::ProviderErrorKind::Permanent
                        ) =>
                    {
                        abandon_respelling(state_db, &respelling, &error, now)?;
                    }
                    Err(error) => {
                        self.resolve_provider_failure(app, state_db, &intent, error, now, report)?;
                        return Ok(None);
                    }
                }
                // The rows are where the planner looks for them now.
                state_db.requeue_leased(intent.id, now, None)?;
                report.retried += 1;
                Ok(None)
            }

            ActiveStage::DiscardRunning {
                mirror_delete,
                cloud_folder_waits,
            } => match outcome {
                ProviderJobOutcome::LocalWork(Ok(())) => {
                    // What an inline discard does after the move.
                    let _ = env.tags.remove(&intent.path);
                    env.local_echoes.record_delete(path_key(&intent.path), now);
                    forget_deleted_path(state_db, &intent.path);
                    ask_about_emptied_folder(env, state_db, &intent.path, now);
                    if cloud_folder_waits {
                        state_db.enqueue_intents_coalesced(
                            &[(
                                intent.path.clone(),
                                PendingIntentKind::ReconcileSubtree,
                                now,
                            )],
                            crate::safeguards::IntentSource::Fresh,
                        )?;
                    }
                    if mirror_delete {
                        report.mirror_deletes += 1;
                    }
                    self.complete(state_db, &intent, report)?;
                    Ok(None)
                }
                ProviderJobOutcome::LocalWork(Err(message)) => {
                    self.resolve_failure(
                        app,
                        state_db,
                        &intent,
                        RetryFailureKind::Transient,
                        &message,
                        now,
                        report,
                    )?;
                    Ok(None)
                }
                other => self.fail_unexpected_outcome(
                    app,
                    state_db,
                    &intent,
                    other,
                    "discarding",
                    now,
                    report,
                ),
            },

            ActiveStage::UploadRunning { permit, plan } => match outcome {
                ProviderJobOutcome::RemoteDelete(result) => {
                    app.release_work(permit);
                    match result {
                        Ok(()) => {
                            env.remote_echoes
                                .record_delete(plan.remote_path.as_str(), now);
                            forget_deleted_path(state_db, &plan.local_path);
                            reconcile_local_directory_left_behind(state_db, &plan.local_path, now);
                            let cause = cloud_delete_cause(&intent, env.sync_mode);
                            crate::logging::info(
                                "Removed from the cloud",
                                &[
                                    ("path", plan.local_path.display().to_string()),
                                    ("remote_path", plan.remote_path.as_str().to_string()),
                                    ("cause", cause.to_string()),
                                ],
                            );
                            report.cloud_deletes.push((plan.local_path.clone(), cause));
                            self.complete(state_db, &intent, report)?;
                            Ok(None)
                        }
                        Err(error) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
                            // Deleting something already gone is convergence.
                            env.remote_echoes
                                .record_delete(plan.remote_path.as_str(), now);
                            forget_deleted_path(state_db, &plan.local_path);
                            reconcile_local_directory_left_behind(state_db, &plan.local_path, now);
                            self.complete(state_db, &intent, report)?;
                            Ok(None)
                        }
                        Err(error) => {
                            self.resolve_provider_failure(
                                app, state_db, &intent, error, now, report,
                            )?;
                            Ok(None)
                        }
                    }
                }
                ProviderJobOutcome::TransferCompleted { outcome, .. } => {
                    app.release_work(permit);
                    env.remote_echoes.record_write(
                        plan.remote_path.as_str(),
                        Some(plan.op_id.clone()),
                        Some(outcome.content_hash.clone()),
                        Some(outcome.bytes_total),
                        outcome.remote_modified_at,
                        now,
                    );
                    // An upload records its own op-id: the tag that it
                    // just put on the object.
                    record_upload_index(
                        state_db,
                        &plan,
                        &outcome.content_hash,
                        outcome.bytes_total,
                        outcome.remote_modified_at,
                        &plan.op_id,
                        now,
                    );
                    record_remote_id(state_db, &plan.local_path, outcome.remote_id.as_deref());
                    self.complete(state_db, &intent, report)?;
                    Ok(None)
                }
                ProviderJobOutcome::TransferHeld {
                    session,
                    resume_after,
                } => {
                    // Hold at the checkpoint; advance re-dispatches when
                    // the gate reopens. The stage timer keeps the original
                    // upload start so diagnostics show total elapsed.
                    Ok(Some(ActiveExecution {
                        intent,
                        stage: ActiveStage::UploadHeld {
                            permit,
                            plan,
                            session,
                            resume_at: resume_after.map(|wait| now_inst + wait),
                        },
                        stage_started_inst,
                    }))
                }
                ProviderJobOutcome::TransferFailed { error, phase } => {
                    app.release_work(permit);
                    if phase == TransferPhase::Step
                        && error.kind == vapor_shared::ProviderErrorKind::PreconditionFailed
                        && env.sync_mode == vapor_shared::SyncMode::TwoWay
                    {
                        // The remote changed underneath the guarded
                        // upload: a race lost by this side. Keep both.
                        self.finish_as_conflict(app, state_db, env, &intent, now, report)?;
                        return Ok(None);
                    }
                    if intent.kind != PendingIntentKind::Delete
                        && error.kind == vapor_shared::ProviderErrorKind::NotFound
                        && fs::symlink_metadata(&intent.path).is_err()
                    {
                        // The local source went away between planning
                        // and the transfer (a save followed by a rename or
                        // delete). The watcher has already reported the
                        // new state of that path; this upload has nothing
                        // left to carry.
                        crate::logging::debug(
                            "Local file vanished before its upload could start; completing as a no-op",
                            &[("path", intent.path.display().to_string())],
                        );
                        self.complete(state_db, &intent, report)?;
                        return Ok(None);
                    }
                    self.resolve_provider_failure(app, state_db, &intent, error, now, report)?;
                    Ok(None)
                }
                other => {
                    app.release_work(permit);
                    self.fail_unexpected_outcome(
                        app, state_db, &intent, other, "upload", now, report,
                    )
                }
            },

            ActiveStage::DownloadRunning { permit, plan } => match outcome {
                ProviderJobOutcome::TransferCompleted { outcome, displaced } => {
                    app.release_work(permit);
                    // Last line of defence for name collisions: between
                    // planning and apply another download may have
                    // landed a differently-cased sibling this filesystem
                    // cannot hold next to the target. Applying would
                    // rewrite that sibling's file and, through its next
                    // upload, the cloud object it came from.
                    if let Some(existing) =
                        crate::name_collision::colliding_local_path(&plan.local_path)
                    {
                        if let Some(staging) = &plan.staging_path {
                            let _ = fs::remove_file(staging);
                        }
                        // The walk over the folder has both listings and
                        // settles it: one file under two spellings, or
                        // two cloud objects this filesystem cannot hold.
                        if let Some(parent) = plan.local_path.parent() {
                            state_db.enqueue_intents_coalesced(
                                &[(
                                    parent.to_path_buf(),
                                    PendingIntentKind::ReconcileSubtree,
                                    now,
                                )],
                                crate::safeguards::IntentSource::Fresh,
                            )?;
                        }
                        crate::logging::info(
                            "Downloaded object would alias a differently-cased local file; discarding the payload",
                            &[
                                ("path", plan.local_path.display().to_string()),
                                ("existing", existing.display().to_string()),
                            ],
                        );
                        report
                            .name_collisions
                            .push((plan.local_path.clone(), existing));
                        self.complete(state_db, &intent, report)?;
                        return Ok(None);
                    }
                    // Two-way keep-both must not lose a local edit that
                    // lands while the download applies. The apply
                    // captures the current local bytes (rename aside)
                    // before renaming the payload in, then decides
                    // divergence on the displaced file itself: a hash
                    // the download worker read counts only while that
                    // file has the size and the time the worker saw.
                    let apply_result = if env.sync_mode == vapor_shared::SyncMode::TwoWay {
                        apply_downloaded_payload_keep_both(
                            env,
                            state_db,
                            &intent,
                            &plan,
                            &outcome.content_hash,
                            displaced.as_ref(),
                            now,
                        )
                    } else {
                        apply_downloaded_payload(env, &plan)
                            .map(|()| DownloadApply::Applied)
                            .map_err(|error| {
                                format!("local apply of downloaded payload failed: {error}")
                            })
                    };
                    match apply_result {
                        Ok(DownloadApply::RemoteUnchanged(index)) => {
                            // The payload is the content last synced:
                            // the object was only touched. The local
                            // file stays, and its edit goes up.
                            match record_unchanged_remote(
                                state_db,
                                &plan.local_path,
                                &index,
                                outcome.remote_modified_at.unwrap_or(now),
                                plan.remote_op_id.as_deref(),
                                now,
                            ) {
                                Ok(()) => self.complete(state_db, &intent, report)?,
                                Err(message) => self.resolve_failure(
                                    app,
                                    state_db,
                                    &intent,
                                    RetryFailureKind::Transient,
                                    &message,
                                    now,
                                    report,
                                )?,
                            }
                            Ok(None)
                        }
                        Ok(applied) => {
                            if applied == DownloadApply::KeptBoth {
                                report.conflict_paths.push(plan.local_path.clone());
                            }
                            note_own_write_the_index_does_not_know(
                                app,
                                state_db,
                                env,
                                &plan,
                                &outcome.content_hash,
                                plan.remote_op_id.as_deref(),
                            );
                            // The size and the modification time of the
                            // file as applied: what the watcher's event
                            // for this write is checked against.
                            env.local_echoes.record_write(
                                path_key(&plan.local_path),
                                Some(plan.op_id.clone()),
                                Some(outcome.content_hash.clone()),
                                Some(outcome.bytes_total),
                                fs::symlink_metadata(&plan.local_path)
                                    .and_then(|metadata| metadata.modified())
                                    .ok(),
                                now,
                            );
                            record_download_index(
                                state_db,
                                &plan,
                                &outcome.content_hash,
                                outcome.bytes_total,
                                outcome.remote_modified_at,
                                now,
                            );
                            record_remote_id(
                                state_db,
                                &plan.local_path,
                                outcome.remote_id.as_deref(),
                            );
                            self.complete(state_db, &intent, report)?;
                            Ok(None)
                        }
                        Err(message) => {
                            discard_staged_payload(&plan);
                            self.resolve_failure(
                                app,
                                state_db,
                                &intent,
                                RetryFailureKind::Transient,
                                &message,
                                now,
                                report,
                            )?;
                            Ok(None)
                        }
                    }
                }
                ProviderJobOutcome::TransferHeld {
                    session,
                    resume_after,
                } => Ok(Some(ActiveExecution {
                    intent,
                    stage: ActiveStage::DownloadHeld {
                        permit,
                        plan,
                        session,
                        resume_at: resume_after.map(|wait| now_inst + wait),
                    },
                    stage_started_inst,
                })),
                ProviderJobOutcome::DownloadSourceMissing {
                    cloud_root_identity,
                } => {
                    app.release_work(permit);
                    // "No object" is the word of whatever folder is at
                    // the cloud root now, and a swapped root holds none
                    // of the files.
                    if !cloud_root_is_adopted(state_db, Some(&cloud_root_identity)) {
                        self.hold_for_cloud_root(state_db, &intent, now, report)?;
                        return Ok(None);
                    }
                    self.remove_local_only_content(app, state_db, env, &intent, now, report)
                }
                ProviderJobOutcome::TransferFailed { error, phase }
                    if error.kind == vapor_shared::ProviderErrorKind::NotFound =>
                {
                    app.release_work(permit);
                    discard_staged_payload(&plan);
                    if phase == TransferPhase::Begin
                        && env.sync_mode == vapor_shared::SyncMode::PullOnly
                    {
                        return self
                            .remove_local_only_content(app, state_db, env, &intent, now, report);
                    }
                    // Otherwise the remote object vanished between the
                    // feed event and now; the Removed change follows.
                    self.complete(state_db, &intent, report)?;
                    Ok(None)
                }
                ProviderJobOutcome::TransferFailed { error, .. } => {
                    app.release_work(permit);
                    discard_staged_payload(&plan);
                    self.resolve_provider_failure(app, state_db, &intent, error, now, report)?;
                    Ok(None)
                }
                other => {
                    app.release_work(permit);
                    discard_staged_payload(&plan);
                    self.fail_unexpected_outcome(
                        app, state_db, &intent, other, "download", now, report,
                    )
                }
            },

            // No provider job is in flight for these stages; an outcome
            // arriving for them is a logic error. Drop the outcome and
            // keep the execution untouched.
            stage => {
                abort_outcome_session(outcome);
                crate::logging::warning(
                    "Dropped a provider-job outcome for an execution not waiting on one",
                    &[("intent_id", intent.id.to_string())],
                );
                Ok(Some(ActiveExecution {
                    intent,
                    stage,
                    stage_started_inst,
                }))
            }
        }
    }

    /// Strict mirror: a pull-only restore that finds no remote
    /// counterpart means the local file is local-only content, and the
    /// mirror removes it.
    fn remove_local_only_content(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        intent: &DurableIntentRecord,
        now: SystemTime,
        report: &mut StagedExecutorReport,
    ) -> Result<Option<ActiveExecution>, StateDbError> {
        match apply_remote_delete_locally(env, state_db, &intent.path, &LocalHashes::default(), now)
        {
            PlanOutcome::AppliedLocally => report.mirror_deletes += 1,
            PlanOutcome::DiscardOnWorker { cloud_folder_waits } => {
                if self.discard_copy_must_wait(app, state_db, intent, now, report)? {
                    return Ok(None);
                }
                let now_inst = self.clock.now();
                return Ok(self.dispatch_discard(
                    app,
                    env,
                    intent.clone(),
                    true,
                    cloud_folder_waits,
                    now_inst,
                ));
            }
            PlanOutcome::Noop(_) => {}
            PlanOutcome::Fail { failure, message } => {
                self.resolve_failure(app, state_db, intent, failure, &message, now, report)?;
                return Ok(None);
            }
            _ => unreachable!("local delete apply has no other outcomes"),
        }
        self.complete(state_db, intent, report)?;
        Ok(None)
    }

    /// Puts back the removal of a local file that a cloud state asked
    /// for, while the cloud root is not known to be the folder the
    /// profile adopted. The runtime checks the root at once, and holds
    /// the profile if the root is missing or was replaced.
    fn hold_for_cloud_root(
        &mut self,
        state_db: &mut DurableStateDb,
        intent: &DurableIntentRecord,
        now: SystemTime,
        report: &mut StagedExecutorReport,
    ) -> Result<(), StateDbError> {
        let reason = "the cloud root is not confirmed as the folder this profile adopted";
        crate::logging::info(
            "Held a local removal: the cloud root is not confirmed as the folder this profile adopted",
            &[("path", intent.path.display().to_string())],
        );
        state_db.defer_leased(
            intent.id,
            now + Duration::from_secs(constants::engine::ROOT_CHECK_INTERVAL_SECONDS),
            reason,
        )?;
        report.retried += 1;
        report.cloud_root_suspect = true;
        Ok(())
    }

    /// An internal state-machine mismatch (never provider behavior).
    /// Requeue transiently so the intent replans from scratch instead of
    /// wedging.
    /// An outcome the waiting stage cannot apply. A panic inside the
    /// provider call fails the intent permanently with the panic message
    /// (the worker already survived it); anything else is a stage/outcome
    /// mismatch, which is an engine bug.
    #[allow(clippy::too_many_arguments)]
    fn fail_unexpected_outcome(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        intent: &DurableIntentRecord,
        outcome: ProviderJobOutcome,
        stage: &str,
        now: SystemTime,
        report: &mut StagedExecutorReport,
    ) -> Result<Option<ActiveExecution>, StateDbError> {
        match outcome {
            ProviderJobOutcome::Panicked(message) => {
                self.resolve_failure(
                    app,
                    state_db,
                    intent,
                    RetryFailureKind::Permanent,
                    &format!("provider call panicked during the {stage} stage: {message}"),
                    now,
                    report,
                )?;
                Ok(None)
            }
            other => {
                abort_outcome_session(other);
                self.fail_internal(
                    app,
                    state_db,
                    intent,
                    &format!("provider-job outcome did not match the {stage} stage"),
                    now,
                    report,
                )
            }
        }
    }

    fn fail_internal(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        intent: &DurableIntentRecord,
        message: &str,
        now: SystemTime,
        report: &mut StagedExecutorReport,
    ) -> Result<Option<ActiveExecution>, StateDbError> {
        debug_assert!(false, "executor internal error: {message}");
        self.resolve_failure(
            app,
            state_db,
            intent,
            RetryFailureKind::Transient,
            &format!("internal error: {message}"),
            now,
            report,
        )?;
        Ok(None)
    }

    /// WaitingForHash admission: acquire a hash permit, open the file,
    /// and hand the hashing to a provider-job worker.
    #[allow(clippy::too_many_arguments)]
    fn step_waiting_for_hash(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        intent: DurableIntentRecord,
        mut plan: TransferPlan,
        stage_started_inst: Instant,
        now: SystemTime,
        now_inst: Instant,
        report: &mut StagedExecutorReport,
    ) -> Result<Option<ActiveExecution>, StateDbError> {
        let Ok(permit) = app.try_acquire_work(WorkClass::Hash) else {
            return Ok(Some(ActiveExecution {
                intent,
                stage: ActiveStage::WaitingForHash { plan },
                stage_started_inst,
            }));
        };
        match StreamingFileHash::open(&plan.local_path, env.hash_algorithm) {
            Ok(hasher) => {
                // Capture (size, mtime) as the hash begins so a
                // mid-transfer edit can be detected at index-write
                // time (a same-length edit would otherwise pair a
                // post-edit mtime with the pre-edit content hash and
                // let the mtime fast-path skip a needed re-hash).
                plan.hashed_local_state = fs::symlink_metadata(&plan.local_path)
                    .map(|metadata| (metadata.len(), metadata.modified().ok()))
                    .ok();
                Ok(Some(self.dispatch_hash(
                    app, env, intent, permit, plan, hasher, now_inst,
                )))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // The file vanished after planning: the pending
                // Delete intent (or reconcile) owns convergence.
                app.release_work(permit);
                self.complete(state_db, &intent, report)?;
                Ok(None)
            }
            Err(error) => {
                app.release_work(permit);
                self.resolve_failure(
                    app,
                    state_db,
                    &intent,
                    RetryFailureKind::Transient,
                    &format!("cannot open local file for hashing: {error}"),
                    now,
                    report,
                )?;
                Ok(None)
            }
        }
    }

    /// Sends the hashing to a worker, or holds it while the throttle
    /// allows no hashing (under Suspended no new bytes are read).
    #[allow(clippy::too_many_arguments)]
    fn dispatch_hash(
        &mut self,
        app: &mut DaemonApp,
        env: &mut ExecutionEnv<'_>,
        intent: DurableIntentRecord,
        permit: crate::workgate::WorkPermit,
        plan: TransferPlan,
        hasher: StreamingFileHash,
        stage_started_inst: Instant,
    ) -> ActiveExecution {
        if !app.workgate_snapshot().caps.allow_hashing {
            return ActiveExecution {
                intent,
                stage: ActiveStage::HashHeld {
                    permit,
                    plan,
                    hasher,
                },
                stage_started_inst,
            };
        }
        self.jobs.dispatch(
            intent.id,
            job_context(app, env, &self.clock, &self.root_identity),
            ProviderJobKind::Hash(hasher),
        );
        ActiveExecution {
            intent,
            stage: ActiveStage::HashRunning { permit, plan },
            stage_started_inst,
        }
    }

    /// The worker's answer for a hash: on success the execution chains
    /// straight into the upload slot.
    #[allow(clippy::too_many_arguments)]
    fn apply_hash_outcome(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        intent: DurableIntentRecord,
        permit: crate::workgate::WorkPermit,
        mut plan: TransferPlan,
        outcome: ProviderJobOutcome,
        stage_started_inst: Instant,
        now: SystemTime,
        now_inst: Instant,
        report: &mut StagedExecutorReport,
    ) -> Result<Option<ActiveExecution>, StateDbError> {
        match outcome {
            ProviderJobOutcome::Hashed(Ok(content_hash)) => {
                app.release_work(permit);
                plan.content_hash = Some(content_hash);
                self.step_waiting_for_upload(
                    app, state_db, env, intent, plan, now_inst, now, now_inst, report,
                )
            }
            ProviderJobOutcome::HashHeld(hasher) => Ok(Some(ActiveExecution {
                intent,
                stage: ActiveStage::HashHeld {
                    permit,
                    plan,
                    hasher,
                },
                stage_started_inst,
            })),
            ProviderJobOutcome::Hashed(Err(error)) => {
                app.release_work(permit);
                if error.kind() == std::io::ErrorKind::NotFound {
                    // The file vanished: the pending Delete intent (or
                    // reconcile) owns convergence.
                    self.complete(state_db, &intent, report)?;
                } else {
                    self.resolve_failure(
                        app,
                        state_db,
                        &intent,
                        RetryFailureKind::Transient,
                        &format!("hash stage failed: {error}"),
                        now,
                        report,
                    )?;
                }
                Ok(None)
            }
            other => {
                app.release_work(permit);
                self.fail_unexpected_outcome(app, state_db, &intent, other, "hashing", now, report)
            }
        }
    }

    /// Upload-slot admission: acquire the permit and dispatch the
    /// provider job (remote delete, preflight verification, or the
    /// upload session) in the same call.
    #[allow(clippy::too_many_arguments)]
    fn step_waiting_for_upload(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        intent: DurableIntentRecord,
        plan: TransferPlan,
        stage_started_inst: Instant,
        now: SystemTime,
        now_inst: Instant,
        report: &mut StagedExecutorReport,
    ) -> Result<Option<ActiveExecution>, StateDbError> {
        // A deletion in the cloud moves no bytes.
        let small = intent.kind == PendingIntentKind::Delete
            || plan
                .hashed_local_state
                .is_some_and(|(size, _)| size <= constants::engine::SMALL_TRANSFER_MAX_BYTES);
        let Ok(permit) = acquire_transfer(app, WorkClass::Upload, small) else {
            return Ok(Some(ActiveExecution {
                intent,
                stage: ActiveStage::WaitingForUpload { plan },
                stage_started_inst,
            }));
        };
        if intent.kind == PendingIntentKind::Delete {
            self.jobs.dispatch(
                intent.id,
                job_context(app, env, &self.clock, &self.root_identity),
                ProviderJobKind::RemoteDelete {
                    remote_path: plan.remote_path.clone(),
                    op_id: plan.op_id.clone(),
                },
            );
            return Ok(Some(ActiveExecution {
                intent,
                stage: ActiveStage::UploadRunning { permit, plan },
                stage_started_inst: now_inst,
            }));
        }
        if plan.remote_gone_since_sync
            && !intent.approved
            && env.sync_mode == vapor_shared::SyncMode::TwoWay
            && plan.content_hash.is_some()
            && plan.content_hash == plan.last_synced_hash
        {
            // The cloud removed a file this device has not changed
            // since the last sync (a second event for an already
            // synced file arrived just as the cloud deleted it). That
            // is a deletion to apply here, through the two-way deletion
            // guard and into the trash, never a re-upload that would
            // undo it. An approved upload is the user's own merge and
            // goes through.
            app.release_work(permit);
            crate::logging::info(
                "Cloud copy is gone and the local copy is unchanged since the last sync; applying the deletion here instead of re-uploading",
                &[("path", plan.local_path.display().to_string())],
            );
            state_db.enqueue_intents_coalesced(
                &[(
                    plan.local_path.clone(),
                    PendingIntentKind::ApplyRemoteDelete,
                    now,
                )],
                crate::safeguards::IntentSource::Fresh,
            )?;
            self.complete(state_db, &intent, report)?;
            return Ok(None);
        }
        if let RemotePrecondition::HashEquals(expected) = &plan.precondition
            && plan.content_hash.as_deref() == Some(expected.as_str())
        {
            // The local bytes are exactly what the cloud holds (a
            // rewrite with the same content, a second event for a file
            // a move just placed): nothing to transfer. The index
            // keeps the current local mtime so the quick check stays
            // cheap.
            app.release_work(permit);
            let hash = expected.clone();
            let remote_modified_at = state_db
                .sync_index(&plan.local_path)
                .ok()
                .flatten()
                .and_then(|index| index.remote_modified_at);
            // The object was not looked at here: the row keeps the
            // tag it has.
            let op_id = op_id_of_an_object_left_as_it_is(state_db, &plan, None);
            record_upload_index(
                state_db,
                &plan,
                &hash,
                local_size(&plan.local_path),
                remote_modified_at,
                &op_id,
                now,
            );
            crate::logging::debug(
                "Upload skipped: the cloud already holds these bytes",
                &[("path", plan.local_path.display().to_string())],
            );
            self.complete(state_db, &intent, report)?;
            return Ok(None);
        }
        if let Some(source) = move_source_for(state_db, env, app, &intent, &plan) {
            // The same bytes were synced under a path that is gone
            // now: a rename or move. One provider call instead of a
            // re-upload; the stale Delete of the old path converges as
            // a no-op when it runs.
            let from = match state_db
                .alias_remote_for_local(&source.path)
                .ok()
                .flatten()
                .and_then(|text| RemotePath::new(text).ok())
                .or_else(|| RemotePath::from_local(env.local_root?, &source.path))
            {
                Some(from) => from,
                None => return self.dispatch_upload(app, env, intent, permit, plan, now_inst),
            };
            self.jobs.dispatch(
                intent.id,
                job_context(app, env, &self.clock, &self.root_identity),
                ProviderJobKind::Move {
                    from,
                    to: plan.remote_path.clone(),
                    op_id: plan.op_id.clone(),
                    // A source that is gone only means a plain upload.
                    cloud_root_if_missing: None,
                },
            );
            return Ok(Some(ActiveExecution {
                intent,
                stage: ActiveStage::MoveRunning {
                    permit,
                    plan,
                    source,
                },
                stage_started_inst: now_inst,
            }));
        }
        if plan.verify_remote_before_upload {
            // Two-way upload onto an unindexed remote object:
            // identical content is silent convergence; divergent
            // content is a genuine conflict (deterministic resolution
            // for first-sync overlaps and index loss).
            self.jobs.dispatch(
                intent.id,
                job_context(app, env, &self.clock, &self.root_identity),
                ProviderJobKind::Probe(crate::provider_jobs::ProbeRequest {
                    remote_path: plan.remote_path.clone(),
                    // The stat carries the remote mtime the index records
                    // when the content turns out to be identical.
                    want_stat: true,
                    hash: crate::provider_jobs::ProbeHash::Always,
                    want_subtree_listing: false,
                    stat_also: None,
                    local_hashes_if_absent: None,
                    cloud_root_identity_of: None,
                }),
            );
            return Ok(Some(ActiveExecution {
                intent,
                stage: ActiveStage::UploadPreflight { permit, plan },
                stage_started_inst: now_inst,
            }));
        }
        self.dispatch_upload(app, env, intent, permit, plan, now_inst)
    }

    /// A copy to the trash reads and writes the whole item. Under
    /// `Suspended` nothing that costs runs, so the intent waits; `true`
    /// says that it was put back.
    fn discard_copy_must_wait(
        &mut self,
        app: &DaemonApp,
        state_db: &mut DurableStateDb,
        intent: &DurableIntentRecord,
        now: SystemTime,
        report: &mut StagedExecutorReport,
    ) -> Result<bool, StateDbError> {
        if app.snapshot().throttle_state != vapor_shared::ThrottleState::Suspended {
            return Ok(false);
        }
        state_db.defer_leased(
            intent.id,
            now + Duration::from_secs(constants::engine::SUSPENDED_COPY_WAIT_SECONDS),
            "the copy to the trash waits while sync is suspended",
        )?;
        report.retried += 1;
        Ok(true)
    }

    /// Hands the discard of the intent's path to a worker, which may
    /// copy it to the trash of another volume. With no trash to copy
    /// to, the intent is tried again: the settings changed under it.
    fn dispatch_discard(
        &mut self,
        app: &mut DaemonApp,
        env: &ExecutionEnv<'_>,
        intent: DurableIntentRecord,
        mirror_delete: bool,
        cloud_folder_waits: bool,
        now_inst: Instant,
    ) -> Option<ActiveExecution> {
        let trash = env.trash.cloned();
        let path = intent.path.clone();
        // The planner judged the file as it is now. The worker may get
        // to it later, and a save in between is a file nobody judged:
        // the worker looks again and leaves such a file alone.
        let judged = file_size_and_time(&path);
        let reason = if env.sync_mode == vapor_shared::SyncMode::TwoWay {
            constants::trash::REASON_CLOUD_DELETE
        } else {
            constants::trash::REASON_MIRROR_DELETE
        };
        let at = self.clock.now_system();
        self.jobs.dispatch(
            intent.id,
            job_context(app, env, &self.clock, &self.root_identity),
            ProviderJobKind::LocalWork(Box::new(move || match trash {
                Some(_) if judged.is_some() && file_size_and_time(&path) != judged => Err(format!(
                    "{} changed while its removal waited for a worker",
                    path.display()
                )),
                Some(trash) => match trash.discard(&path, reason, at) {
                    Ok(_) => Ok(()),
                    // Gone in the meantime: nothing is left to discard.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(format!(
                        "cannot move {} to the trash: {error}",
                        path.display()
                    )),
                },
                None => Err("no trash is configured for the copy of a discard".to_string()),
            })),
        );
        Some(ActiveExecution {
            intent,
            stage: ActiveStage::DiscardRunning {
                mirror_delete,
                cloud_folder_waits,
            },
            stage_started_inst: now_inst,
        })
    }

    fn dispatch_upload(
        &mut self,
        app: &mut DaemonApp,
        env: &mut ExecutionEnv<'_>,
        intent: DurableIntentRecord,
        permit: crate::workgate::WorkPermit,
        plan: TransferPlan,
        now_inst: Instant,
    ) -> Result<Option<ActiveExecution>, StateDbError> {
        let request = UploadRequest {
            local_source: plan.local_path.clone(),
            remote_path: plan.remote_path.clone(),
            op_id: plan.op_id.clone(),
            precondition: plan.precondition.clone(),
        };
        self.jobs.dispatch(
            intent.id,
            job_context(app, env, &self.clock, &self.root_identity),
            ProviderJobKind::Upload(request),
        );
        Ok(Some(ActiveExecution {
            intent,
            stage: ActiveStage::UploadRunning { permit, plan },
            stage_started_inst: now_inst,
        }))
    }

    /// Download-slot admission: acquire the permit and dispatch the
    /// download job (begin + step loop) in the same call.
    #[allow(clippy::too_many_arguments)]
    fn step_waiting_for_download(
        &mut self,
        app: &mut DaemonApp,
        state_db: &DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        intent: DurableIntentRecord,
        plan: TransferPlan,
        stage_started_inst: Instant,
        now_inst: Instant,
    ) -> Result<Option<ActiveExecution>, StateDbError> {
        let small = plan
            .remote_size
            .is_some_and(|size| size <= constants::engine::SMALL_TRANSFER_MAX_BYTES);
        let Ok(permit) = acquire_transfer(app, WorkClass::Download, small) else {
            return Ok(Some(ActiveExecution {
                intent,
                stage: ActiveStage::WaitingForDownload { plan },
                stage_started_inst,
            }));
        };
        let staging = plan
            .staging_path
            .clone()
            .expect("download plans always carry a staging path");
        self.jobs.dispatch(
            intent.id,
            job_context(app, env, &self.clock, &self.root_identity),
            ProviderJobKind::Download {
                request: DownloadRequest {
                    remote_path: plan.remote_path.clone(),
                    destination: staging,
                },
                displaced: displaced_file_check(env, state_db, &plan),
                cloud_root_if_missing: (env.sync_mode == vapor_shared::SyncMode::PullOnly)
                    .then(|| env.cloud_root.map(str::to_owned))
                    .flatten(),
            },
        );
        Ok(Some(ActiveExecution {
            intent,
            stage: ActiveStage::DownloadRunning { permit, plan },
            stage_started_inst: now_inst,
        }))
    }

    fn complete(
        &mut self,
        state_db: &mut DurableStateDb,
        intent: &DurableIntentRecord,
        report: &mut StagedExecutorReport,
    ) -> Result<(), StateDbError> {
        if !state_db.complete_leased(intent.id)? {
            // The row is gone or no longer leased: a recovered-elsewhere
            // or externally-mutated intent. Dropping this one execution is
            // safe (at-least-once semantics); killing the whole daemon
            // over one inconsistent row is not.
            crate::logging::warning(
                "Dropped staged execution whose durable intent was no longer leased",
                &[("intent_id", intent.id.to_string())],
            );
            return Ok(());
        }
        report.completed += 1;
        Ok(())
    }

    fn resolve_provider_failure(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        intent: &DurableIntentRecord,
        error: ProviderError,
        now: SystemTime,
        report: &mut StagedExecutorReport,
    ) -> Result<(), StateDbError> {
        if error.kind == vapor_shared::ProviderErrorKind::CloudRootUnavailable {
            report.cloud_root_unavailable += 1;
        }
        if error.kind == vapor_shared::ProviderErrorKind::StorageFull {
            // Nothing is wrong with the file, and no retry helps until
            // someone frees space: the upload waits with no attempt
            // spent, and comes back on its own.
            if report.cloud_storage_full.is_none() {
                report.cloud_storage_full = Some(error.message.clone());
            }
            state_db.requeue_leased(
                intent.id,
                now + Duration::from_secs(constants::engine::CLOUD_FULL_RETRY_SECONDS),
                Some(&format!(
                    "{}{}",
                    constants::engine::CLOUD_FULL_WAIT_PREFIX,
                    error.message
                )),
            )?;
            report.retried += 1;
            return Ok(());
        }
        let failure = error.kind.retry_classification();
        self.resolve_failure(app, state_db, intent, failure, &error.message, now, report)
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_failure(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        intent: &DurableIntentRecord,
        failure: RetryFailureKind,
        message: &str,
        now: SystemTime,
        report: &mut StagedExecutorReport,
    ) -> Result<(), StateDbError> {
        match failure {
            // The provider refused the sign-in. No retry fixes that and
            // the intent did not cause it: the runtime holds the profile
            // until the user signs in again, and the intent goes back to
            // the queue without spending an attempt.
            RetryFailureKind::Authentication => {
                if report.authentication_required.is_none() {
                    report.authentication_required = Some(message.to_string());
                }
                if !state_db.requeue_leased(intent.id, now, Some(message))? {
                    crate::logging::warning(
                        "Durable intent was not leased when its sign-in failed; dropping it",
                        &[("intent_id", intent.id.to_string())],
                    );
                }
                report.retried += 1;
            }
            // Retry budget exhausted: finalize as a permanent failure
            // rather than calling schedule_retry (which refuses past the
            // cap) and letting the stale-lease sweep re-pend it forever.
            RetryFailureKind::Transient | RetryFailureKind::RateLimited { .. }
                if intent.attempt_count >= constants::state::MAX_ATTEMPT_COUNT =>
            {
                let message = format!(
                    "{message} (retry budget exhausted after {} attempts)",
                    intent.attempt_count
                );
                app.finalize_failure(
                    state_db,
                    intent.id,
                    RetryFailureKind::Permanent,
                    &message,
                    now,
                )?;
                report.failed += 1;
                crate::logging::error(
                    "Intent failed terminally after exhausting its retry budget",
                    &[
                        ("intent_id", intent.id.to_string()),
                        ("path", intent.path.display().to_string()),
                        ("attempts", intent.attempt_count.to_string()),
                    ],
                );
            }
            RetryFailureKind::Transient | RetryFailureKind::RateLimited { .. } => {
                app.schedule_retry(state_db, intent.id, failure, message, now)?;
                report.retried += 1;
                if intent.attempt_count + 1 == constants::state::BLOCKED_AFTER_ATTEMPTS
                    && let Some(folder) = intent.path.parent()
                {
                    // The same failure, several times: the file is
                    // blocked. One comparison of its folder, with both
                    // listings, finds what an intent alone cannot see
                    // (a name that is a folder on the other side, a
                    // file that is gone), and asks the user when it is
                    // a question.
                    state_db.enqueue_intents_coalesced(
                        &[(
                            folder.to_path_buf(),
                            PendingIntentKind::ReconcileSubtree,
                            now,
                        )],
                        crate::safeguards::IntentSource::ReconcileBacklog,
                    )?;
                }
            }
            RetryFailureKind::Permanent => {
                app.finalize_failure(state_db, intent.id, failure, message, now)?;
                report.failed += 1;
                crate::logging::error(
                    "Intent failed terminally",
                    &[
                        ("intent_id", intent.id.to_string()),
                        ("path", intent.path.display().to_string()),
                        ("failure", failure.label().to_string()),
                        ("error", message.to_string()),
                    ],
                );
            }
        }
        Ok(())
    }

    pub fn admission_capacity(&self, workgate: WorkgateSnapshot) -> usize {
        max_in_flight_items(workgate).saturating_sub(self.active.len())
    }

    /// Drops every in-flight execution, releasing its shared-workgate
    /// permit and aborting any transfer session (held here or running
    /// on a provider-job worker). The leased durable rows stay leased
    /// and are recovered by the stale-lease sweep; this only reclaims
    /// the in-memory permits so a suspended/aborted profile does not
    /// leak daemon-wide concurrency slots to healthy profiles.
    pub fn abort_all(&mut self, app: &mut DaemonApp) {
        // Invalidate in-flight jobs: workers abort their sessions at the
        // next step boundary and the stale outcomes are dropped by the
        // pool's generation filter.
        self.jobs.abort_in_flight();
        for job in self.jobs.harvest() {
            abort_outcome_session(job.outcome);
        }
        let active = std::mem::take(&mut self.active);
        self.active_paths.clear();
        self.respells_running.clear();
        for (_id, execution) in active {
            match execution.stage {
                ActiveStage::Planner { permit }
                | ActiveStage::PlannerProbe { permit, .. }
                | ActiveStage::HashRunning { permit, .. }
                | ActiveStage::HashHeld { permit, .. }
                | ActiveStage::UploadPreflight { permit, .. }
                | ActiveStage::UploadRunning { permit, .. }
                | ActiveStage::MoveRunning { permit, .. }
                | ActiveStage::RespellRunning { permit, .. }
                | ActiveStage::DownloadRunning { permit, .. } => {
                    app.release_work(permit);
                }
                ActiveStage::UploadHeld {
                    permit,
                    mut session,
                    ..
                }
                | ActiveStage::DownloadHeld {
                    permit,
                    mut session,
                    ..
                } => {
                    session.abort();
                    app.release_work(permit);
                }
                // Waiting stages hold a plan but no permit.
                ActiveStage::WaitingForHash { .. }
                | ActiveStage::WaitingForUpload { .. }
                | ActiveStage::DiscardRunning { .. }
                | ActiveStage::WaitingForDownload { .. } => {}
            }
        }
    }
}

impl Default for StagedExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl ActiveExecution {
    fn stage_name(&self) -> ExecutionStage {
        match self.stage {
            ActiveStage::Planner { .. }
            | ActiveStage::PlannerProbe { .. }
            | ActiveStage::RespellRunning { .. }
            | ActiveStage::DiscardRunning { .. } => ExecutionStage::Planner,
            ActiveStage::WaitingForHash { .. } => ExecutionStage::WaitingForHash,
            ActiveStage::HashRunning { .. } | ActiveStage::HashHeld { .. } => ExecutionStage::Hash,
            ActiveStage::WaitingForUpload { .. } | ActiveStage::UploadPreflight { .. } => {
                ExecutionStage::WaitingForUpload
            }
            ActiveStage::UploadRunning { .. }
            | ActiveStage::MoveRunning { .. }
            | ActiveStage::UploadHeld { .. } => ExecutionStage::Upload,
            ActiveStage::WaitingForDownload { .. } => ExecutionStage::WaitingForDownload,
            ActiveStage::DownloadRunning { .. } | ActiveStage::DownloadHeld { .. } => {
                ExecutionStage::Download
            }
        }
    }
}

/// A permit for a transfer. A small one, on a provider whose commits
/// are shared by the transfers of the moment, takes a place in the
/// lane of small transfers; when the lane is full or closed it takes a
/// transfer slot like any other.
fn acquire_transfer(
    app: &mut DaemonApp,
    class: WorkClass,
    small: bool,
) -> Result<crate::workgate::WorkPermit, crate::workgate::WorkPermitDenied> {
    if small && app.provider().capabilities().small_transfers_share_a_commit {
        let lane = match class {
            WorkClass::Download => WorkClass::SmallDownload,
            _ => WorkClass::SmallUpload,
        };
        if let Ok(permit) = app.try_acquire_work(lane) {
            return Ok(permit);
        }
    }
    app.try_acquire_work(class)
}

/// How long an intent waits for the move that another intent carries
/// to the cloud for the folder it is in.
const RESPELL_WAIT: Duration = Duration::from_secs(1);

/// The respelling that `path` shows: the index holds a row for another
/// spelling of it, or rows under another spelling of it, and that
/// other spelling is not on this device. The entry of the first name
/// where the two differ is the one that was renamed.
fn respelling_of(state_db: &DurableStateDb, path: &Path) -> Option<Respelling> {
    let other = state_db
        .sync_index_row_spelled_otherwise(path)
        .ok()
        .flatten()?;
    let (mut old, mut new) = (PathBuf::new(), PathBuf::new());
    let mut differ = false;
    for (theirs, ours) in other.components().zip(path.components()) {
        old.push(theirs);
        new.push(ours);
        if theirs != ours {
            differ = true;
            break;
        }
    }
    // A spelling is a rename only where the volume folds names. Where
    // it does not, `Notes` and `notes` are two folders: one that is
    // gone and one that is new, and each is synced as what it is.
    let folds = new
        .parent()
        .is_some_and(crate::name_collision::folds_case_at);
    (differ && folds && !crate::name_collision::exactly_present(&old)).then_some(Respelling {
        old,
        new,
        owed_from: None,
    })
}

/// The move that the cloud is owed for `path`: a row of `name_carries`
/// for the path, or for a folder above it.
fn owed_cloud_move(state_db: &DurableStateDb, path: &Path) -> Option<Respelling> {
    state_db
        .name_carries()
        .ok()?
        .into_iter()
        .find(|carry| path.starts_with(&carry.new))
        .map(|carry| Respelling {
            old: carry.rows_at,
            new: carry.new,
            owed_from: Some(carry.cloud_at),
        })
}

/// Whether `path` is at or under the place of a cloud object that is
/// about to move to the name this device has for it. An intent for
/// such a path waits a moment: what it would do there (a download into
/// a folder that this device holds under another name) is settled by
/// the comparison that follows the move.
fn cloud_place_is_about_to_move(
    state_db: &DurableStateDb,
    env: &ExecutionEnv<'_>,
    app: &DaemonApp,
    intent: &DurableIntentRecord,
) -> bool {
    intent.kind != PendingIntentKind::ReconcileSubtree
        && intent.attempt_count < constants::engine::MOVE_SETTLE_MAX_DEFERRALS
        && env.sync_mode.allows_local_to_remote()
        && app.provider().capabilities().supports_server_side_move
        && state_db.name_carries().is_ok_and(|carries| {
            carries.iter().any(|carry| {
                carry.cloud_at != carry.new
                    && intent.path.starts_with(&carry.cloud_at)
                    && !intent.path.starts_with(&carry.new)
            })
        })
}

/// A rename that changed only the spelling of a name on this device,
/// found by the intent that is about to be planned, with the two cloud
/// paths of the move that carries it. While the index holds the rows
/// under the old spelling, no intent for the new one has a row to be
/// decided by: an edit would upload as a new file, a deletion would
/// find nothing to delete, and a change from the cloud would download
/// over the local file. So the rename goes first, as one move of the
/// file or the folder, and the rows follow it.
///
/// Only where local changes go to the cloud, and only on a backend
/// that can move an object. `None` for a path whose own row is there.
fn respelling_to_carry(
    app: &DaemonApp,
    state_db: &DurableStateDb,
    env: &mut ExecutionEnv<'_>,
    intent: &DurableIntentRecord,
) -> Option<(Respelling, RemotePath, RemotePath)> {
    let local_root = env.local_root?;
    if intent.kind == PendingIntentKind::ReconcileSubtree
        || intent.remote_path.is_some()
        || !env.sync_mode.allows_local_to_remote()
        || !app.provider().capabilities().supports_server_side_move
        || !intent.path.starts_with(local_root)
    {
        return None;
    }
    // The intent may name the entry by the spelling it had.
    let stored = match crate::name_collision::local_spelling(&intent.path) {
        crate::name_collision::LocalSpelling::Other(stored) => stored,
        _ => intent.path.clone(),
    };
    // A move that the cloud is owed goes before everything else at the
    // path, also when the path has a row of its own: the row is right,
    // and the cloud object is somewhere else.
    if let Some(owed) = owed_cloud_move(state_db, &stored) {
        let from = RemotePath::from_local(local_root, owed.owed_from.as_deref()?)?;
        let to = RemotePath::from_local(local_root, &owed.new)?;
        return Some((owed, from, to));
    }
    if state_db.sync_index(&stored).ok()?.is_some() {
        return None;
    }
    let respelling = stored
        .parent()
        .and_then(|folder| {
            env.move_hints
                .folder_respelling(state_db, local_root, folder)
        })
        .or_else(|| respelling_of(state_db, &stored))?;
    let from = RemotePath::from_local(local_root, &respelling.old)?;
    let to = RemotePath::from_local(local_root, &respelling.new)?;
    Some((respelling, from, to))
}

/// The cloud holds the entry under its new spelling: the index rows
/// follow, the file's row takes the op-id the move wrote, and what the
/// feed says about the move is this daemon's own change.
fn finish_respelling(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    respelling: &Respelling,
    to: &RemotePath,
    op_id: &str,
    now: SystemTime,
) -> Result<(), StateDbError> {
    state_db.rekey_sync_index_under(&respelling.old, &respelling.new)?;
    state_db.rekey_sync_index(&respelling.old, &respelling.new)?;
    if respelling.owed_from.is_some() {
        state_db.remove_name_carry(&respelling.new)?;
        // The two sides are under one name again. What changed inside
        // while the names differed is found by a comparison now, and
        // not at the next scan of everything: the folder is walked,
        // and a file gets the comparison of an upload. In `two-way`
        // that comparison sends what this device changed, fetches what
        // the cloud changed, and moves no bytes for a file that both
        // sides left as it was. The upload of a mirror compares
        // nothing and always sends, so it runs only for a file that
        // this device changed.
        let follow_up = match fs::symlink_metadata(&respelling.new) {
            Ok(metadata) if metadata.is_dir() => Some(PendingIntentKind::ReconcileSubtree),
            Ok(metadata)
                if metadata.is_file()
                    && env.sync_mode.allows_local_to_remote()
                    && (env.sync_mode == vapor_shared::SyncMode::TwoWay
                        || state_db.sync_index(&respelling.new)?.is_none_or(|row| {
                            !row.matches_local(metadata.len(), metadata.modified().ok())
                        })) =>
            {
                Some(PendingIntentKind::Upload)
            }
            _ => None,
        };
        if let Some(kind) = follow_up {
            state_db.enqueue_intents_coalesced(
                &[(respelling.new.clone(), kind, now)],
                crate::safeguards::IntentSource::Fresh,
            )?;
        }
    }
    let cloud_was_at = respelling.owed_from.as_ref().unwrap_or(&respelling.old);
    if let Some(from) = env
        .local_root
        .and_then(|root| RemotePath::from_local(root, cloud_was_at))
    {
        state_db.rekey_name_aliases_under(
            &respelling.old,
            &respelling.new,
            from.as_str(),
            to.as_str(),
        )?;
        env.remote_echoes.record_delete(from.as_str(), now);
    }
    if let Some(row) = state_db.sync_index(&respelling.new)? {
        if let Some(remote_modified_at) = row.remote_modified_at {
            state_db.set_sync_index_remote_state(&respelling.new, remote_modified_at, op_id)?;
        }
        env.remote_echoes.record_write(
            to.as_str(),
            Some(op_id.to_string()),
            known_hash(&row.content_hash).map(str::to_string),
            Some(row.size_bytes),
            row.remote_modified_at,
            now,
        );
    }
    crate::logging::info(
        "Moved the cloud object to the name that this device has for it",
        &[
            ("from", cloud_was_at.display().to_string()),
            ("to", respelling.new.display().to_string()),
        ],
    );
    Ok(())
}

/// The cloud has nothing under the old spelling, or has something
/// under the new one already: the rows of the old spelling describe
/// no object that can be found. They go, so every path under the new
/// spelling is compared as a path that never synced, which keeps the
/// data of both sides, and the walk of the folder settles the names.
fn abandon_respelling(
    state_db: &mut DurableStateDb,
    respelling: &Respelling,
    error: &ProviderError,
    now: SystemTime,
) -> Result<(), StateDbError> {
    if respelling.owed_from.is_some() {
        state_db.remove_name_carry(&respelling.new)?;
    }
    // Rows under the path of this device describe what this device
    // holds: they stay, and the comparison reads the cloud against
    // them.
    let forgotten = if respelling.old == respelling.new {
        0
    } else {
        state_db.remove_sync_index_tree(&respelling.old)?
    };
    if let Some(folder) = respelling.new.parent() {
        state_db.enqueue_intents_coalesced(
            &[(
                folder.to_path_buf(),
                PendingIntentKind::ReconcileSubtree,
                now,
            )],
            crate::safeguards::IntentSource::Fresh,
        )?;
    }
    crate::logging::info(
        "A name of this device could not be carried to the cloud as a move; comparing the folder instead",
        &[
            ("from", respelling.old.display().to_string()),
            ("to", respelling.new.display().to_string()),
            ("rows_forgotten", forgotten.to_string()),
            ("error", error.message.clone()),
        ],
    );
    Ok(())
}

/// Plans one leased intent into its execution route. Cheap by design:
/// a stat, a path derivation, an op-id allocation, and sync-index
/// reads, never a provider call. When the decision needs remote
/// facts it returns [`PlanOutcome::Probe`] and the matching
/// continuation resumes once the probe job completes.
fn plan_intent(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    now: SystemTime,
) -> PlanOutcome {
    let Some(local_root) = env.local_root else {
        return PlanOutcome::Fail {
            failure: RetryFailureKind::Permanent,
            message: "no local root configured".to_string(),
        };
    };
    // The cloud object of a local file normally lives at the mirror of
    // its path. An intent that names its remote path explicitly (a
    // download from a colliding name), or a local file that is the
    // alias of a colliding remote name, points elsewhere.
    let aliased = match intent.remote_path.clone() {
        Some(text) => Some(text),
        None => match state_db.alias_remote_for_local(&intent.path) {
            Ok(alias) => alias,
            Err(error) => {
                return PlanOutcome::Fail {
                    failure: RetryFailureKind::Transient,
                    message: format!("cannot read the name aliases: {error}"),
                };
            }
        },
    };
    let remote_path = match aliased {
        Some(text) => match RemotePath::new(text) {
            Ok(path) => path,
            Err(error) => {
                return PlanOutcome::Fail {
                    failure: RetryFailureKind::Permanent,
                    message: format!("intent names an invalid remote path: {error}"),
                };
            }
        },
        None => match RemotePath::from_local(local_root, &intent.path) {
            Some(path) => path,
            // Inside the root, and still no remote path: a name the path
            // model refuses. That is not a failure of the file, and no
            // retry changes it.
            None if intent
                .path
                .strip_prefix(local_root)
                .is_ok_and(|relative| relative.to_string_lossy().contains('\\')) =>
            {
                return PlanOutcome::Unsyncable(crate::unsyncable::UnsyncableName::backslash(
                    &intent.path,
                ));
            }
            None => {
                return PlanOutcome::Fail {
                    failure: RetryFailureKind::Permanent,
                    message: format!(
                        "intent path {} is not inside the local sync root",
                        intent.path.display()
                    ),
                };
            }
        },
    };
    let op_id = allocate_op_id(env, intent, now);

    // Direction gates: a one-way mode drops intents of
    // the gated direction as logged no-ops. This also absorbs stale
    // intents that were durably enqueued before a mode change.
    if matches!(
        intent.kind,
        PendingIntentKind::Upload | PendingIntentKind::Rename | PendingIntentKind::Delete
    ) && !env.sync_mode.allows_local_to_remote()
    {
        return PlanOutcome::Noop("local-to-remote propagation is gated off in pull-only mode");
    }
    if matches!(
        intent.kind,
        PendingIntentKind::Download | PendingIntentKind::ApplyRemoteDelete
    ) && !env.sync_mode.allows_remote_to_local()
    {
        return PlanOutcome::Noop("remote-to-local propagation is gated off in push-only mode");
    }

    match intent.kind {
        PendingIntentKind::Upload | PendingIntentKind::Rename => {
            if let crate::name_collision::LocalSpelling::Other(_) =
                crate::name_collision::local_spelling(&intent.path)
            {
                // The file is stored under another spelling now; that
                // name has its own intent. Uploading through this one
                // would write the old name back to the cloud.
                return PlanOutcome::Noop("local file is stored under another spelling now");
            }
            match fs::symlink_metadata(&intent.path) {
                Ok(metadata) if metadata.is_dir() => {
                    PlanOutcome::Noop("directories materialize through their children")
                }
                Ok(metadata) if !metadata.is_file() => {
                    // Symlinks, FIFOs, sockets, device nodes. Must be
                    // refused at planning: opening a FIFO for hashing
                    // blocks until a writer appears, and the intent
                    // otherwise sits in the queue forever (observed as
                    // a permanently-WaitingForHash row).
                    PlanOutcome::Noop("symlinks and special files are outside the sync contract")
                }
                Ok(_) => plan_upload(env, state_db, intent, remote_path, op_id),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    PlanOutcome::Noop("local file vanished before upload")
                }
                Err(error) => PlanOutcome::Fail {
                    failure: RetryFailureKind::Transient,
                    message: format!("cannot stat local file: {error}"),
                },
            }
        }
        PendingIntentKind::Delete => plan_delete(
            state_db,
            env.move_hints,
            env.local_changes_pending,
            intent,
            remote_path,
            op_id,
        ),
        PendingIntentKind::Download => {
            if let Err(reason) = verify_within_local_root(local_root, &intent.path) {
                return PlanOutcome::Fail {
                    failure: RetryFailureKind::Permanent,
                    message: reason,
                };
            }
            // One staging name for the intent, whatever the attempt: a
            // retry writes over the payload of the try before it. A
            // name for each attempt left a full copy behind for each
            // one that failed, in the user's folder.
            let staging_name = format!(
                "{}dl-{}-op{}",
                constants::provider::TEMP_FILE_PREFIX,
                sanitize_for_file_name(env.device_id),
                intent.id
            );
            let staging_path = intent
                .path
                .parent()
                .unwrap_or(local_root)
                .join(staging_name);
            let plan = TransferPlan {
                remote_path,
                op_id,
                local_path: intent.path.clone(),
                staging_path: Some(staging_path),
                content_hash: None,
                precondition: RemotePrecondition::None,
                verify_remote_before_upload: false,
                last_synced_hash: None,
                remote_gone_since_sync: false,
                remote_unchanged_since_sync: false,
                hashed_local_state: None,
                remote_op_id: None,
                remote_size: None,
            };
            // Best-effort: probe the remote object's op-id so the sync
            // index records the writer's id, not this device's download
            // id. A stat failure just leaves the op-id correlator
            // disabled for this path (the hash comparison still
            // converges).
            PlanOutcome::Probe {
                request: Box::new(ProbeRequest {
                    remote_path: plan.remote_path.clone(),
                    want_stat: true,
                    hash: ProbeHash::Never,
                    want_subtree_listing: false,
                    stat_also: None,
                    local_hashes_if_absent: None,
                    cloud_root_identity_of: None,
                }),
                pending: PendingPlan::Download { plan },
            }
        }
        PendingIntentKind::ApplyRemoteDelete => {
            if let Err(reason) = verify_within_local_root(local_root, &intent.path) {
                return PlanOutcome::Fail {
                    failure: RetryFailureKind::Permanent,
                    message: reason,
                };
            }
            if let crate::name_collision::LocalSpelling::Other(_) =
                crate::name_collision::local_spelling(&intent.path)
            {
                // The cloud removed a name; what this device stores
                // under another spelling of it is a different entry.
                return PlanOutcome::Noop(
                    "this device stores the entry under another spelling; nothing to remove",
                );
            }
            // A synced file the cloud removed may be the other half of
            // a cloud-side rename; the download's move detection needs
            // the local file still here. The removal waits while the
            // create may still come: on its first planning while the
            // feed has more to hand over, and on every planning while a
            // download that could be the new name is queued.
            if env.sync_mode == vapor_shared::SyncMode::TwoWay
                && intent.attempt_count < constants::engine::MOVE_SETTLE_MAX_DEFERRALS
                && let Ok(Some(_)) = state_db.sync_index(&intent.path)
                && fs::symlink_metadata(&intent.path).is_ok()
                && ((intent.attempt_count == 0 && env.remote_changes_pending)
                    || env
                        .move_hints
                        .download_could_be_a_move(state_db, &intent.path))
            {
                return PlanOutcome::Defer {
                    delay: Duration::from_secs(constants::engine::MOVE_SETTLE_DELAY_SECONDS),
                    reason: "a queued download may be this file renamed in the cloud",
                };
            }
            // Two-way deletion guard: "data preservation wins
            // over deletion". A remote deletion only applies when the
            // local copy is exactly what was last synced AND the sync
            // happened before the deletion was observed. A modified (or
            // unknown-provenance) local file survives; the pending
            // upload restores it remotely. One-way pull mirrors delete
            // unconditionally, that is their contract.
            if env.sync_mode == vapor_shared::SyncMode::TwoWay {
                // The Removed event may be stale: another device (or our
                // own re-upload) could have recreated the remote object
                // after the deletion was observed. Probe the remote
                // before the local guard runs.
                return PlanOutcome::Probe {
                    request: Box::new(ProbeRequest {
                        remote_path,
                        want_stat: true,
                        hash: ProbeHash::Never,
                        want_subtree_listing: false,
                        stat_also: None,
                        local_hashes_if_absent: Some(touched_file_check(
                            state_db,
                            &intent.path,
                            env.hash_algorithm,
                        )),
                        cloud_root_identity_of: env.cloud_root.map(str::to_owned),
                    }),
                    pending: PendingPlan::ApplyRemoteDelete,
                };
            }
            // A mirror removes whatever the cloud removed, but only on
            // the word of the folder it adopted.
            PlanOutcome::Probe {
                request: Box::new(ProbeRequest {
                    remote_path,
                    want_stat: false,
                    hash: ProbeHash::Never,
                    want_subtree_listing: false,
                    stat_also: None,
                    local_hashes_if_absent: None,
                    cloud_root_identity_of: env.cloud_root.map(str::to_owned),
                }),
                pending: PendingPlan::MirrorRemoteDelete,
            }
        }
        PendingIntentKind::ReconcileSubtree => PlanOutcome::Fail {
            failure: RetryFailureKind::Permanent,
            message: "reconcile intents are routed to the reconcile controller, not the executor"
                .to_string(),
        },
    }
}

/// Plans a remote delete with the two-way "modification wins over
/// deletion" guard: two-way mode probes the remote first (the decision
/// tree continues in [`continue_plan_delete`]); one-way push mirrors
/// delete whatever the cloud holds, which is its contract.
fn plan_delete(
    state_db: &mut DurableStateDb,
    move_hints: &mut MoveHints,
    local_changes_pending: bool,
    intent: &DurableIntentRecord,
    remote_path: RemotePath,
    op_id: String,
) -> PlanOutcome {
    let plan = TransferPlan {
        remote_path,
        op_id,
        local_path: intent.path.clone(),
        staging_path: None,
        content_hash: None,
        precondition: RemotePrecondition::None,
        verify_remote_before_upload: false,
        last_synced_hash: None,
        remote_gone_since_sync: false,
        remote_unchanged_since_sync: false,
        hashed_local_state: None,
        remote_op_id: None,
        remote_size: None,
    };
    // Every mode probes first: the stat tells a directory (which
    // expands into per-entry deletes, since no provider deletes a tree
    // in one call) from a file, and push-only still needs the listing
    // even though it never refuses a delete.
    let index = match state_db.sync_index(&intent.path) {
        Ok(index) => index,
        Err(error) => {
            return PlanOutcome::Fail {
                failure: RetryFailureKind::Transient,
                message: format!("cannot read sync index before delete: {error}"),
            };
        }
    };
    // A synced file that vanished may be the other half of a rename.
    // The upload's move detection needs the cloud object still there,
    // so the deletion waits while the create may still come: on its
    // first planning while local events sit in their quiet window (the
    // create of a rename is seen at the same moment as its delete, so
    // it is there or in the queue), and on every planning while an
    // upload of the same size is queued. With neither, nothing can turn
    // this deletion into a move, and waiting only costs the user time.
    if let Some(index) = &index
        && intent.attempt_count < constants::engine::MOVE_SETTLE_MAX_DEFERRALS
        && ((intent.attempt_count == 0 && local_changes_pending)
            || move_hints.upload_of_size_is_queued(state_db, index.size_bytes))
    {
        return PlanOutcome::Defer {
            delay: Duration::from_secs(constants::engine::MOVE_SETTLE_DELAY_SECONDS),
            reason: "waiting in case a queued upload is this file renamed",
        };
    }
    let hash = match &index {
        Some(index) => ProbeHash::IfDivergedFrom {
            index_op_id: index.last_op_id.clone(),
        },
        None => ProbeHash::Never,
    };
    PlanOutcome::Probe {
        request: Box::new(ProbeRequest {
            remote_path: plan.remote_path.clone(),
            want_stat: true,
            hash,
            // A deleted local directory arrives as one Delete intent;
            // the remote subtree tells the continuation what to expand.
            want_subtree_listing: true,
            stat_also: None,
            local_hashes_if_absent: None,
            cloud_root_identity_of: None,
        }),
        pending: PendingPlan::Delete { plan, index },
    }
}

/// A `Delete` intent says the path left this device. Seconds pass
/// between the event and the delete (the debounce, the settle window,
/// the probe), and the file can be back by then: the user undid the
/// removal, an app replaced the file with a delete and a slow create,
/// or the watcher reported a rename the daemon made itself. A file
/// that is here was not deleted, so the cloud file stays, and an
/// upload is queued, which compares the two and converges or
/// transfers. A local entry of another kind (a folder where the cloud
/// holds a file) does not count: there the file did go.
/// Returns `None` when the deletion may go on.
fn keep_cloud_file_of_present_path(
    state_db: &mut DurableStateDb,
    local_path: &Path,
    now: SystemTime,
) -> Option<PlanOutcome> {
    match fs::symlink_metadata(local_path) {
        // A file stored under another spelling is not this file.
        Ok(metadata)
            if metadata.is_file() && crate::name_collision::exactly_present(local_path) => {}
        Ok(_) => return None,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return None;
        }
        // An unreadable parent proves nothing about the file.
        Err(error) => {
            return Some(PlanOutcome::Fail {
                failure: RetryFailureKind::Transient,
                message: format!("cannot tell whether the local path is gone: {error}"),
            });
        }
    }
    if let Err(error) = state_db.enqueue_intents_coalesced(
        &[(local_path.to_path_buf(), PendingIntentKind::Upload, now)],
        crate::safeguards::IntentSource::Fresh,
    ) {
        return Some(PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: format!("cannot enqueue the upload of a file that came back: {error}"),
        });
    }
    crate::logging::info(
        "Dropped a deletion: the file is on this device again",
        &[("path", local_path.display().to_string())],
    );
    Some(PlanOutcome::Noop(
        "the file is on this device again; nothing to delete",
    ))
}

/// Why Vapor removes an object from the cloud, in words a user reads
/// in the log and the timeline.
fn cloud_delete_cause(
    intent: &DurableIntentRecord,
    sync_mode: vapor_shared::SyncMode,
) -> &'static str {
    if intent.approved {
        "you answered a decision with this deletion"
    } else if sync_mode == vapor_shared::SyncMode::PushOnly {
        "push-only mirror: it is not on this device"
    } else {
        "it was deleted on this device"
    }
}

/// Continuation of [`plan_delete`] once the remote probe returns. In
/// two-way mode a local delete only propagates when the remote is
/// still exactly what we last synced (op-id or content hash matches
/// the sync index). If another writer changed the remote since our
/// last sync, or the index has no row to compare against, the delete
/// is refused and a Download is enqueued to bring the remote content
/// back locally, mirroring the upload guard so a version this device
/// never saw is never destroyed. Push-only is a strict mirror: the
/// local side is the source of truth, so the delete goes through
/// whatever the remote holds.
fn continue_plan_delete(
    state_db: &mut DurableStateDb,
    sync_mode: vapor_shared::SyncMode,
    intent: &DurableIntentRecord,
    plan: TransferPlan,
    index: Option<crate::state_db::SyncIndexEntry>,
    probe: &ProbeResult,
    now: SystemTime,
) -> PlanOutcome {
    let Some(stat) = probe.stat.as_ref() else {
        return PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: "internal error: delete probe carried no remote stat".to_string(),
        };
    };
    if let Ok(Some(remote_entry)) = stat {
        if remote_entry.kind == vapor_providers::RemoteEntryKind::Directory {
            return continue_plan_directory_delete(state_db, intent, plan, probe, now);
        }
        if let Some(outcome) = keep_cloud_file_of_present_path(state_db, &plan.local_path, now) {
            return outcome;
        }
    }
    match stat {
        Ok(None) => {
            // Remote already gone: the deletion converged. Drop the
            // index row and complete as a no-op.
            forget_deleted_path(state_db, &plan.local_path);
            PlanOutcome::Noop("remote already absent; deletion converged")
        }
        Ok(Some(_)) if sync_mode != vapor_shared::SyncMode::TwoWay => {
            PlanOutcome::RemoteDelete(plan)
        }
        Ok(Some(remote_entry)) => {
            let unchanged = match &index {
                Some(index) => {
                    // The op-id tag alone is not proof (an in-place write
                    // keeps it); the size-and-mtime quick check has to
                    // agree, else the hash decides.
                    if remote_entry.op_id.as_deref() == Some(index.last_op_id.as_str())
                        && remote_quick_check_passes(index, remote_entry)
                    {
                        true
                    } else {
                        let remote_hash = remote_entry
                            .content_hash
                            .clone()
                            .or_else(|| probe.content_hash_ok());
                        remote_hash.as_deref() == Some(index.content_hash.as_str())
                    }
                }
                // No row: this device never compared the cloud object
                // with anything, so it cannot know that the deletion
                // was of this content. The object stays, unless the
                // user's answer named this path.
                None => intent.names_path,
            };
            if unchanged {
                return PlanOutcome::RemoteDelete(plan);
            }
            // The cloud holds something this device did not delete:
            // another writer changed it since the last sync, or it was
            // never synced here. Keeping data wins over the deletion.
            // Refuse the delete and pull the cloud content back.
            if let Err(error) = state_db.enqueue_intents_coalesced(
                &[(plan.local_path.clone(), PendingIntentKind::Download, now)],
                crate::safeguards::IntentSource::Fresh,
            ) {
                return PlanOutcome::Fail {
                    failure: RetryFailureKind::Transient,
                    message: format!("cannot enqueue delete-preservation download: {error}"),
                };
            }
            if let Err(error) = state_db.remove_sync_index(&plan.local_path) {
                crate::logging::warning(
                    "Could not clear stale sync index after refusing a remote delete",
                    &[("error", error.to_string())],
                );
            }
            if index.is_some() {
                crate::logging::warning(
                    "Remote changed since last sync; preserving it over a local deletion",
                    &[("path", plan.local_path.display().to_string())],
                );
                PlanOutcome::Noop("remote changed since last sync; modification wins over deletion")
            } else {
                crate::logging::info(
                    "Kept a cloud file that this device never compared; downloading it again",
                    &[("path", plan.local_path.display().to_string())],
                );
                PlanOutcome::Noop("the cloud file was never compared on this device; kept")
            }
        }
        Err(error) => PlanOutcome::Fail {
            failure: error.kind.retry_classification(),
            message: format!("cannot stat remote before delete: {}", error.message),
        },
    }
}

/// A local directory was deleted and the remote still holds a
/// directory at that path. The provider never deletes a directory
/// recursively: that would destroy children the engine never compared
/// against the index. Instead the deletion expands into one guarded
/// Delete intent per remote entry, deepest first, so every file goes
/// through the same "refused when the remote changed since the last
/// sync" rule, and the directory itself is re-enqueued last. A later
/// pass that still finds children waits while any of them has queued
/// work, and keeps the directory once the children that remain are the
/// ones the rules preserved.
fn continue_plan_directory_delete(
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    plan: TransferPlan,
    probe: &ProbeResult,
    now: SystemTime,
) -> PlanOutcome {
    // A folder that is on this device again was not deleted: its
    // children that are gone still expand below, the folder stays.
    let local_folder_present = fs::symlink_metadata(&plan.local_path)
        .is_ok_and(|metadata| metadata.is_dir())
        && crate::name_collision::exactly_present(&plan.local_path);
    let subtree = match probe.subtree.as_ref() {
        Some(Ok(entries)) => entries,
        Some(Err(error)) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
            // The directory went between the stat and the listing (a
            // sibling delete or move emptied and removed it): the
            // deletion converged.
            forget_deleted_path(state_db, &plan.local_path);
            return PlanOutcome::Noop("remote directory already absent; deletion converged");
        }
        Some(Err(error)) => {
            return PlanOutcome::Fail {
                failure: error.kind.retry_classification(),
                message: format!(
                    "cannot list remote directory before delete: {}",
                    error.message
                ),
            };
        }
        None => {
            return PlanOutcome::Fail {
                failure: RetryFailureKind::Transient,
                message: "internal error: directory delete probe carried no listing".to_string(),
            };
        }
    };
    if subtree.is_empty() {
        if local_folder_present {
            return PlanOutcome::Noop("the folder is on this device again; nothing to delete");
        }
        // Nothing underneath: the provider removes the empty directory.
        return PlanOutcome::RemoteDelete(plan);
    }
    let queued_below = match state_db.intents_under(&plan.local_path, intent.id) {
        Ok(count) => count,
        Err(error) => {
            return PlanOutcome::Fail {
                failure: RetryFailureKind::Transient,
                message: format!("cannot inspect queued work under a deleted directory: {error}"),
            };
        }
    };
    if queued_below > 0 {
        // Children are still being deleted (or preserved through a
        // download); come back once they have settled. This is a wait
        // and not a failure: counted as tries, a large folder would
        // read as a file that cannot sync while its removal is under
        // way. Each look lists the folder in the cloud, so the wait
        // grows with what is left.
        let delay = match queued_below {
            0..=99 => constants::engine::FOLDER_DELETE_WAIT_SECONDS,
            100..=1_999 => 5 * constants::engine::FOLDER_DELETE_WAIT_SECONDS,
            _ => 15 * constants::engine::FOLDER_DELETE_WAIT_SECONDS,
        };
        return PlanOutcome::Defer {
            delay: Duration::from_secs(delay),
            reason: "waiting for the entries under a deleted folder to leave",
        };
    }
    // Only entries that are gone locally are deletions. An entry whose
    // local counterpart exists (restored by a refused delete, or
    // recreated by the user) is content to keep, never to expand into
    // another delete; that is also what stops an expansion loop.
    // Deepest entries first so the FIFO deletes files before their
    // directories and inner directories before outer ones.
    let mut entries: Vec<&vapor_providers::RemoteEntry> = subtree.iter().collect();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.path.as_str().matches('/').count()));
    let mut batch: Vec<(PathBuf, PendingIntentKind, SystemTime)> =
        Vec::with_capacity(entries.len() + 1);
    let mut kept = 0usize;
    for entry in entries {
        let Some(relative) = entry
            .path
            .as_str()
            .strip_prefix(plan.remote_path.as_str())
            .map(|rest| rest.trim_start_matches('/'))
        else {
            continue;
        };
        if relative.is_empty() {
            continue;
        }
        let mut local = plan.local_path.clone();
        for segment in relative.split('/') {
            local.push(segment);
        }
        if crate::name_collision::exactly_present(&local) {
            kept += 1;
            continue;
        }
        batch.push((local, PendingIntentKind::Delete, now));
    }
    if batch.is_empty() {
        crate::logging::info(
            "Keeping a remote directory: every remaining child exists locally",
            &[
                ("path", plan.local_path.display().to_string()),
                ("children", kept.to_string()),
            ],
        );
        return PlanOutcome::Noop("remote directory kept: its remaining children exist locally");
    }
    if !local_folder_present {
        batch.push((plan.local_path.clone(), PendingIntentKind::Delete, now));
    }
    // The children of a deletion the user answered a decision with
    // carry that answer.
    let approval = if intent.names_path {
        crate::state_db::Approval::NamedPath
    } else if intent.approved {
        crate::state_db::Approval::Guards
    } else {
        crate::state_db::Approval::None
    };
    let enqueued = match state_db.enqueue_intents_coalesced_with(
        &batch,
        crate::safeguards::IntentSource::Fresh,
        approval,
    ) {
        Ok(count) => count,
        Err(error) => {
            return PlanOutcome::Fail {
                failure: RetryFailureKind::Transient,
                message: format!("cannot expand a directory delete: {error}"),
            };
        }
    };
    crate::logging::info(
        "Expanded a directory delete into per-entry deletes",
        &[
            ("path", plan.local_path.display().to_string()),
            ("entries", subtree.len().to_string()),
            ("kept", kept.to_string()),
            ("enqueued", enqueued.to_string()),
        ],
    );
    PlanOutcome::Noop("directory delete expanded into per-entry deletes")
}

/// Plans an upload with the two-way conflict guard: two-way mode probes
/// the remote (continuing in [`continue_plan_upload`]); one-way modes
/// skip the guard entirely: strict mirror overwrites by design.
fn plan_upload(
    env: &ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    remote_path: RemotePath,
    op_id: String,
) -> PlanOutcome {
    let plan = TransferPlan {
        remote_path,
        op_id,
        local_path: intent.path.clone(),
        staging_path: None,
        content_hash: None,
        precondition: RemotePrecondition::None,
        verify_remote_before_upload: false,
        last_synced_hash: None,
        remote_gone_since_sync: false,
        remote_unchanged_since_sync: false,
        hashed_local_state: None,
        remote_op_id: None,
        remote_size: None,
    };
    if env.sync_mode != vapor_shared::SyncMode::TwoWay {
        return PlanOutcome::Upload(plan);
    }

    let index = match state_db.sync_index(&intent.path) {
        Ok(index) => index,
        Err(error) => {
            return PlanOutcome::Fail {
                failure: RetryFailureKind::Transient,
                message: format!("cannot read sync index: {error}"),
            };
        }
    };
    let hash = match &index {
        Some(index) => ProbeHash::IfDivergedFrom {
            index_op_id: index.last_op_id.clone(),
        },
        None => ProbeHash::Never,
    };
    PlanOutcome::Probe {
        request: Box::new(ProbeRequest {
            remote_path: plan.remote_path.clone(),
            want_stat: true,
            hash,
            want_subtree_listing: false,
            stat_also: None,
            local_hashes_if_absent: None,
            cloud_root_identity_of: None,
        }),
        pending: PendingPlan::Upload { plan, index },
    }
}

/// Continuation of [`plan_upload`] once the remote probe returns. The
/// sync index distinguishes "remote unchanged since our last sync"
/// (safe overwrite, hash-guarded) from "remote changed by another
/// writer" (keep both).
fn continue_plan_upload(
    state_db: &mut DurableStateDb,
    mut plan: TransferPlan,
    index: Option<crate::state_db::SyncIndexEntry>,
    probe: &ProbeResult,
    now: SystemTime,
) -> PlanOutcome {
    let Some(stat) = probe.stat.as_ref() else {
        return PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: "internal error: upload probe carried no remote stat".to_string(),
        };
    };
    if let Ok(Some(remote_entry)) = stat
        && remote_entry.kind == vapor_providers::RemoteEntryKind::Directory
    {
        // A file here, a folder in the cloud. An upload cannot settle
        // that, and a retry would fail the same way for good. The walk
        // of the folder has both listings: it removes the cloud folder
        // when this device replaced it with the file, or asks.
        return compare_folder_of_a_type_clash(
            state_db,
            &plan.local_path,
            now,
            "the cloud holds a folder under this name; its folder is compared",
        );
    }
    match stat {
        Ok(None) => {
            // Remote absent. With an index this is either a cloud
            // deletion of a file this device has not touched, which the
            // upload gate recognises once the local hash is known and
            // applies here, or a delete/modify race, where the
            // modification wins (data preservation). Either way the
            // upload, if it happens, is a guarded fresh create.
            plan.precondition = RemotePrecondition::Absent;
            if let Some(index) = index {
                plan.last_synced_hash = Some(index.content_hash);
                plan.remote_gone_since_sync = true;
            }
            PlanOutcome::Upload(plan)
        }
        Ok(Some(remote_entry)) => match index {
            Some(index)
                if remote_entry.op_id.as_deref() == Some(index.last_op_id.as_str())
                    && remote_quick_check_passes(&index, remote_entry) =>
            {
                // Remote unchanged since our last sync: overwrite,
                // guarded against the tiny window between the probe and
                // the upload landing. The op-id alone is not proof: an
                // in-place edit on a filesystem keeps the tag, so the
                // size-and-mtime quick check has to agree.
                //
                // The guard needs a hash to compare. An index row with
                // none recorded (an upload whose provider reported no
                // hash) says nothing: the object's own hash stands in,
                // which is the hash of what this device wrote. With no
                // hash from either, the upload gate decides.
                match known_hash(&index.content_hash)
                    .map(str::to_string)
                    .or_else(|| remote_entry.content_hash.clone())
                    .or_else(|| probe.content_hash_ok())
                    .filter(|hash| !hash.is_empty())
                {
                    Some(hash) => plan.precondition = RemotePrecondition::HashEquals(hash),
                    None => {
                        plan.verify_remote_before_upload = true;
                        plan.remote_unchanged_since_sync = true;
                    }
                }
                PlanOutcome::Upload(plan)
            }
            Some(index) => {
                // An op-id mismatch is not yet divergence: the tag is
                // absent on any externally-written remote file (which we
                // may have already synced *from*), and provider-side
                // copies can strip tags. The content hash is the ground
                // truth: an equal hash means the remote is
                // byte-identical to our last sync, so the upload is a
                // safe guarded overwrite. Without this check, editing a
                // file whose last change arrived from an untagged
                // external write manufactured a keep-both conflict on
                // every upload (and could revert a just-resolved one).
                let remote_hash = remote_entry
                    .content_hash
                    .clone()
                    .or_else(|| probe.content_hash_ok());
                if known_hash(&index.content_hash).is_some()
                    && remote_hash.as_deref() == Some(index.content_hash.as_str())
                {
                    plan.precondition = RemotePrecondition::HashEquals(index.content_hash);
                    PlanOutcome::Upload(plan)
                } else {
                    // The remote no longer matches our last sync, or
                    // cannot be shown to. This is usually a concurrent
                    // writer, but it is also the crash-replay case: an
                    // upload that committed at the provider but crashed
                    // before the durable index write leaves the remote
                    // holding *our own* new content under a fresh op-id,
                    // with the index still on the old hash. And it is
                    // the offline cloud edit found by the reconcile
                    // walk, where the local copy has not changed at all.
                    // Defer to the upload gate, which knows the local
                    // hash and the last synced one.
                    plan.verify_remote_before_upload = true;
                    plan.last_synced_hash = known_hash(&index.content_hash).map(str::to_string);
                    PlanOutcome::Upload(plan)
                }
            }
            None => {
                // Remote exists but this path has never synced (first
                // sync overlap or index loss). Defer the decision to the
                // upload gate, where the local hash is known: identical
                // content converges, divergent content conflicts.
                plan.verify_remote_before_upload = true;
                PlanOutcome::Upload(plan)
            }
        },
        Err(error) => PlanOutcome::Fail {
            failure: error.kind.retry_classification(),
            message: format!("cannot stat remote before upload: {}", error.message),
        },
    }
}

/// A name that is a file on one side and a folder on the other stops
/// the intent that met it. The walk over the parent folder decides: it
/// carries out a swap that one side made, and opens the question when
/// both sides made the name. Asking for the walk now, in place of a
/// retry that fails for good, is what makes the answer come in seconds
/// and not at the next daily scan.
fn compare_folder_of_a_type_clash(
    state_db: &mut DurableStateDb,
    local_path: &Path,
    now: SystemTime,
    reason: &'static str,
) -> PlanOutcome {
    let folder = local_path.parent().unwrap_or(local_path).to_path_buf();
    match state_db.enqueue_intents_coalesced(
        &[(folder, PendingIntentKind::ReconcileSubtree, now)],
        crate::safeguards::IntentSource::Fresh,
    ) {
        Ok(_) => PlanOutcome::Noop(reason),
        Err(error) => PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: format!("cannot schedule the walk for a file and folder clash: {error}"),
        },
    }
}

/// The remote object still has the size and mtime the index recorded,
/// or the index never recorded a remote mtime (a row written before the
/// provider reported one), in which case the op-id has to carry the
/// decision on its own as it always did.
/// A content hash that the sync index recorded, or `None` for an empty
/// one. An empty hash is "not known": it is never a value to compare,
/// since no object's hash equals it and every guard built on it fails.
fn known_hash(hash: &str) -> Option<&str> {
    (!hash.is_empty()).then_some(hash)
}

fn remote_quick_check_passes(
    index: &crate::state_db::SyncIndexEntry,
    remote_entry: &vapor_providers::RemoteEntry,
) -> bool {
    index.remote_modified_at.is_none()
        || index.matches_remote(remote_entry.size_bytes, remote_entry.modified_at)
}

/// Continuation of the Download planner probe: record the remote
/// writer's op-id (best-effort: a failed stat only disables the op-id
/// correlator for this path).
fn continue_plan_download(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    mut plan: TransferPlan,
    probe: &ProbeResult,
    now: SystemTime,
) -> PlanOutcome {
    let remote_entry = probe
        .stat
        .as_ref()
        .and_then(|stat| stat.as_ref().ok())
        .and_then(|entry| entry.as_ref());
    if remote_entry.is_some_and(|entry| entry.kind == vapor_providers::RemoteEntryKind::Directory) {
        // A directory has no payload to download. The path reached the
        // queue as a file (a pull-only restore of a folder this device
        // removed, a watcher event on a folder): the walk over its
        // parent materializes the children, or raises the type
        // mismatch when a local file stands at the name.
        let scope = plan
            .local_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| plan.local_path.clone());
        if let Err(error) = state_db.enqueue_intents_coalesced(
            &[(scope, PendingIntentKind::ReconcileSubtree, now)],
            crate::safeguards::IntentSource::Fresh,
        ) {
            return PlanOutcome::Fail {
                failure: RetryFailureKind::Transient,
                message: format!("cannot schedule the walk for a remote directory: {error}"),
            };
        }
        return PlanOutcome::Noop(
            "remote object is a directory; its subtree is reconciled instead",
        );
    }
    if remote_entry.is_some()
        && env.sync_mode == vapor_shared::SyncMode::TwoWay
        && fs::symlink_metadata(&plan.local_path).is_ok_and(|metadata| metadata.is_dir())
        && !is_empty_folder(&plan.local_path)
    {
        // A file in the cloud, a folder with content here: the payload
        // has no place to land. When the folder was synced, the walk
        // already read the swap and queued the removal of its files,
        // and this download waits for them to leave. Asking for the
        // walk again would make it queue this download again, a loop
        // with no pause for as long as one file stays.
        if state_db
            .sync_index_has_rows_under(&plan.local_path)
            .unwrap_or(false)
        {
            return PlanOutcome::Defer {
                delay: Duration::from_secs(constants::engine::TYPE_CLASH_WAIT_SECONDS),
                reason: "waiting for the files of the folder that the cloud replaced to leave",
            };
        }
        // A folder that never synced: only the walk can say what it is.
        return compare_folder_of_a_type_clash(
            state_db,
            &plan.local_path,
            now,
            "this device holds a folder under this name; its folder is compared",
        );
    }
    plan.remote_op_id = remote_entry.and_then(|entry| entry.op_id.clone());
    plan.remote_size = remote_entry.map(|entry| entry.size_bytes);
    // An object with the size of the content last synced here, while
    // the local file is here: the object may have been touched and not
    // changed (a `touch`, a rewrite with the same bytes). Its content
    // says so, and then there is nothing to download. A local edit
    // that waits for its upload must not become a conflict over it.
    if let Some(remote) = remote_entry
        && env.sync_mode == vapor_shared::SyncMode::TwoWay
        && let Some(index) = last_sync_the_remote_may_still_hold(state_db, &plan.local_path, remote)
    {
        return match remote.content_hash.as_deref() {
            Some(hash) if hash == index.content_hash => {
                keep_local_file_of_unchanged_remote(state_db, &plan, &index, remote, now)
            }
            Some(_) => PlanOutcome::Download(plan),
            None => PlanOutcome::Probe {
                request: Box::new(ProbeRequest {
                    remote_path: plan.remote_path.clone(),
                    want_stat: true,
                    hash: ProbeHash::Always,
                    want_subtree_listing: false,
                    stat_also: None,
                    local_hashes_if_absent: None,
                    cloud_root_identity_of: None,
                }),
                pending: PendingPlan::DownloadUnchangedCheck { plan, index },
            },
        };
    }
    // A remote object with the size and mtime of a file this device
    // synced, while that file is still here untouched and the download
    // target is not: a cloud-side rename, most likely. The content is
    // confirmed (by the hash the backend reports, or a hash probe)
    // before anything is renamed.
    if let Some(remote) = remote_entry
        && env.sync_mode == vapor_shared::SyncMode::TwoWay
        && !crate::name_collision::exactly_present(&plan.local_path)
        && let Some(candidate) = local_move_candidate(state_db, &plan.local_path, remote)
        && let Some(old_name) = remote_path_of(state_db, env.local_root, &candidate.path)
    {
        // The object is a moved one only when the candidate's own
        // object is gone. A copy that kept the time (`cp -p`, a Finder
        // duplicate) looks the same from here, and both names must end
        // up on this device then.
        return PlanOutcome::Probe {
            request: Box::new(ProbeRequest {
                remote_path: plan.remote_path.clone(),
                want_stat: true,
                hash: if remote.content_hash.is_some() {
                    ProbeHash::Never
                } else {
                    ProbeHash::Always
                },
                want_subtree_listing: false,
                stat_also: Some(old_name),
                local_hashes_if_absent: None,
                cloud_root_identity_of: None,
            }),
            pending: PendingPlan::DownloadMoveCheck { plan, candidate },
        };
    }
    PlanOutcome::Download(plan)
}

/// The index row of a download target whose cloud object may still
/// hold the content of the last sync: the row has a hash to compare,
/// the object has the size of that content, and a regular file is at
/// the target. With no local file the download restores it, as before.
fn last_sync_the_remote_may_still_hold(
    state_db: &DurableStateDb,
    target: &Path,
    remote: &vapor_providers::RemoteEntry,
) -> Option<crate::state_db::SyncIndexEntry> {
    let index = state_db.sync_index(target).ok().flatten()?;
    (known_hash(&index.content_hash).is_some()
        && index.size_bytes == remote.size_bytes
        && fs::symlink_metadata(target).is_ok_and(|metadata| metadata.is_file())
        && crate::name_collision::exactly_present(target))
    .then_some(index)
}

/// Continuation of the unchanged check: the remote hash says whether
/// the object still holds the content last synced.
fn continue_download_unchanged_check(
    state_db: &mut DurableStateDb,
    mut plan: TransferPlan,
    index: crate::state_db::SyncIndexEntry,
    probe: &ProbeResult,
    now: SystemTime,
) -> PlanOutcome {
    let remote = probe
        .stat
        .as_ref()
        .and_then(|stat| stat.as_ref().ok())
        .and_then(|entry| entry.as_ref());
    let Some(remote) = remote else {
        return PlanOutcome::Download(plan);
    };
    plan.remote_op_id = remote.op_id.clone();
    plan.remote_size = Some(remote.size_bytes);
    if remote.size_bytes == index.size_bytes
        && probe.content_hash_ok().as_deref() == Some(index.content_hash.as_str())
    {
        keep_local_file_of_unchanged_remote(state_db, &plan, &index, remote, now)
    } else {
        PlanOutcome::Download(plan)
    }
}

/// The cloud object of a download holds the content last synced: it
/// was touched, not changed. Nothing is downloaded and the local file
/// is not rewritten. The index takes the object's new time, so the
/// quick check passes again, and a local file that changed since the
/// last sync is the one change there is: its upload is queued.
fn keep_local_file_of_unchanged_remote(
    state_db: &mut DurableStateDb,
    plan: &TransferPlan,
    index: &crate::state_db::SyncIndexEntry,
    remote: &vapor_providers::RemoteEntry,
    now: SystemTime,
) -> PlanOutcome {
    match record_unchanged_remote(
        state_db,
        &plan.local_path,
        index,
        remote.modified_at,
        remote.op_id.as_deref(),
        now,
    ) {
        Ok(()) => PlanOutcome::Noop("the cloud copy holds the content last synced"),
        Err(message) => PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message,
        },
    }
}

fn record_unchanged_remote(
    state_db: &mut DurableStateDb,
    local_path: &Path,
    index: &crate::state_db::SyncIndexEntry,
    remote_modified_at: SystemTime,
    remote_op_id: Option<&str>,
    now: SystemTime,
) -> Result<(), String> {
    state_db
        .set_sync_index_remote_state(local_path, remote_modified_at, remote_op_id.unwrap_or(""))
        .map_err(|error| format!("cannot record the state of an unchanged cloud copy: {error}"))?;
    let local_changed = fs::symlink_metadata(local_path).is_ok_and(|metadata| {
        metadata.is_file() && !index.matches_local(metadata.len(), metadata.modified().ok())
    });
    if local_changed {
        state_db
            .enqueue_intents_coalesced(
                &[(local_path.to_path_buf(), PendingIntentKind::Upload, now)],
                crate::safeguards::IntentSource::Fresh,
            )
            .map_err(|error| format!("cannot queue the upload of the local edit: {error}"))?;
    }
    crate::logging::info(
        "The cloud copy holds the content last synced; nothing to download",
        &[
            ("path", local_path.display().to_string()),
            ("local_edit_queued", local_changed.to_string()),
        ],
    );
    Ok(())
}

/// A synced local file the remote object could be the moved copy of:
/// same size, the remote mtime the index recorded, the local copy still
/// exactly what was last synced.
fn local_move_candidate(
    state_db: &DurableStateDb,
    target: &Path,
    remote: &vapor_providers::RemoteEntry,
) -> Option<crate::state_db::SyncIndexEntry> {
    let candidates = state_db
        .sync_index_by_remote_state(remote.size_bytes, remote.modified_at)
        .ok()?;
    candidates.into_iter().find(|entry| {
        entry.path != target
            && crate::name_collision::exactly_present(&entry.path)
            && fs::symlink_metadata(&entry.path).is_ok_and(|metadata| {
                metadata.is_file() && entry.matches_local(metadata.len(), metadata.modified().ok())
            })
    })
}

/// Continuation of the move check: the remote hash confirms (or not)
/// that the object is the candidate's content.
fn continue_download_move_check(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    mut plan: TransferPlan,
    candidate: crate::state_db::SyncIndexEntry,
    probe: &ProbeResult,
    now: SystemTime,
) -> PlanOutcome {
    let Some(remote) = probe
        .stat
        .as_ref()
        .and_then(|stat| stat.as_ref().ok())
        .and_then(|entry| entry.as_ref())
    else {
        return PlanOutcome::Download(plan);
    };
    plan.remote_op_id = remote.op_id.clone();
    plan.remote_size = Some(remote.size_bytes);
    // Only "no object" under the old name makes this a move. An object
    // there, or no answer, leaves a copy: the download brings it.
    let old_name_is_gone = matches!(probe.also_stat, Some(Ok(None)));
    let remote_hash = remote
        .content_hash
        .clone()
        .or_else(|| probe.content_hash_ok());
    if old_name_is_gone && remote_hash.as_deref() == Some(candidate.content_hash.as_str()) {
        apply_local_move(env, state_db, plan, &candidate, remote.modified_at, now)
    } else {
        PlanOutcome::Download(plan)
    }
}

/// The cloud object of a local file: the mirror of its path, or the
/// colliding name it is the alias of.
fn remote_path_of(
    state_db: &DurableStateDb,
    local_root: Option<&Path>,
    local_path: &Path,
) -> Option<RemotePath> {
    match state_db.alias_remote_for_local(local_path).ok()? {
        Some(text) => RemotePath::new(text).ok(),
        None => RemotePath::from_local(local_root?, local_path),
    }
}

/// Renames the synced local file into the download's target path and
/// re-keys its index row, so the cloud-side rename costs no transfer.
/// A rename that fails falls back to the download; the source's own
/// stale `ApplyRemoteDelete` finds nothing to remove when it runs.
fn apply_local_move(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    plan: TransferPlan,
    source: &crate::state_db::SyncIndexEntry,
    remote_modified_at: SystemTime,
    now: SystemTime,
) -> PlanOutcome {
    if let Some(parent) = plan.local_path.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return PlanOutcome::Download(plan);
    }
    // A hash probe may have run since the candidate was chosen. A
    // source that changed in that time is not the cloud object any
    // more, and a file that appeared at the target must not be
    // replaced: the download takes over, and it keeps both.
    let source_untouched = fs::symlink_metadata(&source.path).is_ok_and(|metadata| {
        metadata.is_file() && source.matches_local(metadata.len(), metadata.modified().ok())
    });
    if !source_untouched
        || vapor_platform::fs_ops::atomic_noclobber_rename(&source.path, &plan.local_path).is_err()
    {
        return PlanOutcome::Download(plan);
    }
    let _ = env.tags.remove(&source.path);
    let _ = vapor_platform::fs_ops::with_owner_write(&plan.local_path, || {
        env.tags.write_op_id(
            &plan.local_path,
            plan.remote_op_id.as_deref().unwrap_or(&plan.op_id),
        )
    });
    // The watcher reports the rename as a delete and a create; both
    // are this daemon's own writes.
    env.local_echoes.record_delete(path_key(&source.path), now);
    let local_modified_at = fs::symlink_metadata(&plan.local_path)
        .and_then(|metadata| metadata.modified())
        .ok();
    env.local_echoes.record_write(
        path_key(&plan.local_path),
        plan.remote_op_id.clone(),
        Some(source.content_hash.clone()),
        Some(source.size_bytes),
        local_modified_at,
        now,
    );
    write_sync_index_entry(
        state_db,
        &plan,
        &source.content_hash,
        source.size_bytes,
        local_modified_at,
        Some(remote_modified_at),
        plan.remote_op_id.as_deref().unwrap_or(""),
        now,
    );
    forget_deleted_path(state_db, &source.path);
    record_remote_id(state_db, &plan.local_path, source.remote_id.as_deref());
    ask_about_emptied_folder(env, state_db, &source.path, now);
    crate::logging::info(
        "Renamed the local file instead of downloading a moved cloud object",
        &[
            ("from", source.path.display().to_string()),
            ("to", plan.local_path.display().to_string()),
            ("bytes", source.size_bytes.to_string()),
        ],
    );
    PlanOutcome::MovedLocally
}

/// Continuation of the two-way ApplyRemoteDelete probe: a remote that
/// exists again makes the deletion stale; otherwise the local
/// preservation guard decides and the delete applies locally.
fn continue_apply_remote_delete(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    probe: &ProbeResult,
    now: SystemTime,
) -> PlanOutcome {
    // Only a stat that answers "no object" confirms the deletion. An
    // error is not an answer: a vanished cloud root makes every probe
    // fail, and finishing deletions on that evidence would mirror a
    // missing root as the loss of every file under it.
    match probe.stat.as_ref() {
        Some(Ok(None)) => {}
        // A folder in the cloud where this device holds a file: the
        // file is gone there all the same, and a folder took its name.
        Some(Ok(Some(entry)))
            if entry.kind == vapor_providers::RemoteEntryKind::Directory
                && fs::symlink_metadata(&intent.path).is_ok_and(|metadata| metadata.is_file()) => {}
        Some(Ok(Some(_))) => {
            // The remote object was recreated after the deletion was
            // observed; completing the delete would remove a file that
            // is present remotely.
            return PlanOutcome::Noop("remote object exists again; deletion is stale");
        }
        Some(Err(error)) => {
            return PlanOutcome::Fail {
                failure: error.kind.retry_classification(),
                message: format!(
                    "cannot confirm the cloud deletion before removing the local copy: {}",
                    error.message
                ),
            };
        }
        None => {
            return PlanOutcome::Fail {
                failure: RetryFailureKind::Transient,
                message: "internal error: remote-delete probe carried no remote stat".to_string(),
            };
        }
    }
    if let Some(outcome) = removal_must_not_go_on(state_db, intent, probe) {
        return outcome;
    }
    if probe.local_hashes.cut_short() {
        // Not every touched file was read. Deciding now would keep a
        // file only because nobody looked at it.
        return PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: "the check of the local files stopped while hashing is on hold".to_string(),
        };
    }
    match deletion_loses_to_local_state(state_db, intent, &probe.local_hashes) {
        Ok(Some(reason)) => return PlanOutcome::Noop(reason),
        Ok(None) => {}
        Err(message) => {
            return PlanOutcome::Fail {
                failure: RetryFailureKind::Transient,
                message,
            };
        }
    }
    let cloud_holds_a_folder = matches!(probe.stat.as_ref(), Some(Ok(Some(_))));
    let outcome =
        match apply_remote_delete_locally(env, state_db, &intent.path, &probe.local_hashes, now) {
            PlanOutcome::DiscardOnWorker { .. } => PlanOutcome::DiscardOnWorker {
                cloud_folder_waits: cloud_holds_a_folder,
            },
            other => other,
        };
    if matches!(outcome, PlanOutcome::AppliedLocally) {
        forget_deleted_path(state_db, &intent.path);
        ask_about_emptied_folder(env, state_db, &intent.path, now);
        if cloud_holds_a_folder {
            // The cloud holds a folder under the name the file had: its
            // files come down now that the name is free here.
            if let Err(error) = state_db.enqueue_intents_coalesced(
                &[(
                    intent.path.clone(),
                    PendingIntentKind::ReconcileSubtree,
                    now,
                )],
                crate::safeguards::IntentSource::Fresh,
            ) {
                crate::logging::warning(
                    "Could not queue the walk of a cloud folder that replaced a file",
                    &[
                        ("path", intent.path.display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
            }
        }
    }
    outcome
}

fn is_empty_folder(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
        && fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

/// Whether `path` is a file with one of the names that an OS writes
/// into a folder by itself (`.DS_Store` and the like).
fn is_os_folder_file(path: &Path, metadata: &fs::Metadata) -> bool {
    metadata.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(constants::filtering::is_os_folder_file)
}

/// Whether the folder at `path` holds nothing that a user put there:
/// it is empty, or every entry in it is a file that an OS writes into
/// a folder by itself. A folder with any other entry (an ignored
/// build tree, a file that is not synced yet, a link) holds something.
fn holds_nothing_of_the_user(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
        && fs::read_dir(path).is_ok_and(|entries| {
            entries.into_iter().all(|entry| {
                entry.is_ok_and(|entry| {
                    fs::symlink_metadata(entry.path())
                        .is_ok_and(|metadata| is_os_folder_file(&entry.path(), &metadata))
                })
            })
        })
}

/// A cloud-side move or removal just took `gone` out of its folder. If
/// that left nothing of the user in the folder, the folder may be gone
/// from the cloud too (it was renamed there, or removed with what it
/// held), and the file-only engine has no event of its own for that.
/// The folder gets the question every cloud deletion gets: the planner
/// asks the cloud, and removes the folder only when the cloud has none
/// and nothing in it is unsynced. A folder the cloud still has stays,
/// and so does one the user put a file into in the meantime.
///
/// A folder that holds only files which an OS writes into a folder by
/// itself is asked about like an empty one, and those files go to the
/// trash with it. A folder with any other entry is not asked about:
/// what is left in it is the user's, and it stays as it is.
fn ask_about_emptied_folder(
    env: &ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    gone: &Path,
    now: SystemTime,
) {
    ask_about_folder_that_lost(state_db, env.local_root, env.sync_mode, gone, now);
}

/// [`ask_about_emptied_folder`] for a caller with no execution
/// environment: the rename of an entry that got another place in the
/// cloud takes it out of its folder too.
pub(crate) fn ask_about_folder_that_lost(
    state_db: &mut DurableStateDb,
    local_root: Option<&Path>,
    sync_mode: vapor_shared::SyncMode,
    gone: &Path,
    now: SystemTime,
) {
    if sync_mode != vapor_shared::SyncMode::TwoWay {
        return;
    }
    let (Some(folder), Some(root)) = (gone.parent(), local_root) else {
        return;
    };
    if folder == root || !folder.starts_with(root) || !holds_nothing_of_the_user(folder) {
        return;
    }
    if let Err(error) = state_db.enqueue_intents_coalesced(
        &[(
            folder.to_path_buf(),
            PendingIntentKind::ApplyRemoteDelete,
            now,
        )],
        crate::safeguards::IntentSource::Fresh,
    ) {
        crate::logging::warning(
            "Could not queue the check of a folder that a cloud change emptied",
            &[
                ("path", folder.display().to_string()),
                ("error", error.to_string()),
            ],
        );
    }
}

/// Continuation of the one-way ApplyRemoteDelete probe: the mirror
/// removes the local entry, unconditionally, once the cloud root is
/// known to be the folder the profile adopted.
fn continue_mirror_remote_delete(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    probe: &ProbeResult,
    now: SystemTime,
) -> PlanOutcome {
    if let Some(outcome) = removal_must_not_go_on(state_db, intent, probe) {
        return outcome;
    }
    let outcome =
        apply_remote_delete_locally(env, state_db, &intent.path, &LocalHashes::default(), now);
    if matches!(outcome, PlanOutcome::AppliedLocally) {
        forget_deleted_path(state_db, &intent.path);
    }
    outcome
}

/// What stops the local removal that a cloud deletion asked for, once
/// its probe is back: the deletion was withdrawn, the provider refused
/// the read of the cloud root for a reason of its own, or the cloud
/// root is not known to be the folder the profile adopted.
fn removal_must_not_go_on(
    state_db: &DurableStateDb,
    intent: &DurableIntentRecord,
    probe: &ProbeResult,
) -> Option<PlanOutcome> {
    // A root recovery drops queued deletions while this one may be in
    // flight; the dropped row is the answer.
    match state_db.intent_is_queued(intent.id) {
        Ok(true) => {}
        Ok(false) => return Some(PlanOutcome::Noop("deletion withdrawn while its probe ran")),
        Err(error) => {
            return Some(PlanOutcome::Fail {
                failure: RetryFailureKind::Transient,
                message: format!("cannot re-read the deletion intent: {error}"),
            });
        }
    }
    // A rate limit or a refused sign-in says nothing about the root.
    // It takes the path of every other provider call, with the
    // slowdown or the sign-in hold that belongs to it.
    if let Some(Err(error)) = probe.cloud_root_identity.as_ref()
        && matches!(
            error.kind,
            vapor_shared::ProviderErrorKind::RateLimited { .. }
                | vapor_shared::ProviderErrorKind::Authentication
        )
    {
        return Some(PlanOutcome::Fail {
            failure: error.kind.retry_classification(),
            message: format!(
                "cannot confirm the cloud folder before removing the local copy: {}",
                error.message
            ),
        });
    }
    // "No object" came from whatever folder is at the cloud root now.
    // In a root that was swapped, every file is "gone".
    if !cloud_root_is_adopted(state_db, probe.cloud_root_identity.as_ref()) {
        return Some(PlanOutcome::CloudRootSuspect);
    }
    None
}

/// Whether the cloud root is the folder the profile adopted, from the
/// identity a worker read after the call that found an object gone.
/// `None` means nothing was read because no cloud root is configured
/// (a fixture). A profile that adopted no root has nothing to compare.
/// Anything else that is not the recorded identity (another folder, no
/// folder, no answer) vouches for nothing: the removal waits.
fn cloud_root_is_adopted(
    state_db: &DurableStateDb,
    identity: Option<&crate::provider_jobs::CloudRootIdentity>,
) -> bool {
    use crate::root_identity::{RootSide, RootStatus, classify_cloud_probe, recorded_identity};
    let Some(identity) = identity else {
        return true;
    };
    match recorded_identity(state_db, RootSide::Cloud) {
        Ok(None) => true,
        Ok(Some(recorded)) => matches!(
            classify_cloud_probe(&recorded, identity.clone()),
            RootStatus::Ready
        ),
        Err(_) => false,
    }
}

/// Keep-both resolution when the local side lost an upload race: move
/// the local loser to its conflict-copy path (suppressing the rename's
/// delete echo), enqueue an upload for the copy, and, when the caller
/// does not itself hold the canonical remote payload: a download for the
/// remote canonical, letting the caller complete the original intent.
/// Deterministic: the conflict path derives from the device id and the
/// intent's durable event time.
///
/// Ordering is crash-safe: the Download(original) restore is enqueued
/// *before* the rename, so a crash between rename and the copy's
/// Upload-enqueue cannot strand the canonical path with nothing pending
/// to restore it.
fn resolve_upload_conflict(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    now: SystemTime,
) -> PlanOutcome {
    let timestamp_ms = intent
        .enqueued_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let conflict_local = crate::conflict::conflict_copy_path(
        &intent.path,
        env.device_id,
        timestamp_ms,
        |candidate: &Path| candidate.exists(),
    );
    if let Err(error) = state_db.enqueue_intents_coalesced(
        &[(intent.path.clone(), PendingIntentKind::Download, now)],
        crate::safeguards::IntentSource::Fresh,
    ) {
        return PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: format!("cannot enqueue conflict canonical-restore download: {error}"),
        };
    }
    if let Err(error) = fs::rename(&intent.path, &conflict_local) {
        if error.kind() == std::io::ErrorKind::NotFound {
            // The local loser vanished mid-conflict: nothing to keep.
            return PlanOutcome::Noop("local file vanished during conflict resolution");
        }
        return PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: format!("cannot stage conflict copy: {error}"),
        };
    }
    // The rename emits Removed(original): an echo of our own write.
    env.local_echoes.record_delete(path_key(&intent.path), now);
    let _ = env.tags.relocate_side_file(&intent.path, &conflict_local);
    // The index entry described the pre-conflict canonical; it no longer
    // holds for either path.
    if let Err(error) = state_db.remove_sync_index(&intent.path) {
        crate::logging::warning(
            "Conflict resolution could not clear the sync index entry",
            &[("error", error.to_string())],
        );
    }
    if let Err(error) = state_db.enqueue_intents_coalesced(
        &[(conflict_local.clone(), PendingIntentKind::Upload, now)],
        crate::safeguards::IntentSource::Fresh,
    ) {
        return PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: format!("cannot enqueue conflict-copy upload: {error}"),
        };
    }
    crate::logging::warning(
        "Resolved concurrent divergence by keeping both versions",
        &[
            ("canonical", intent.path.display().to_string()),
            ("conflict_copy", conflict_local.display().to_string()),
        ],
    );
    PlanOutcome::ConflictResolved
}

/// Deletion guard: returns the preservation reason when a remote
/// deletion must NOT apply to the local file, `None` when the deletion
/// may proceed.
fn deletion_loses_to_local_state(
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    local_hashes: &LocalHashes,
) -> Result<Option<&'static str>, String> {
    let metadata = match fs::symlink_metadata(&intent.path) {
        Ok(metadata) if metadata.is_file() => metadata,
        // Directories and absent paths have no unsynced content to
        // preserve; the apply path handles them.
        _ => return Ok(None),
    };
    let Some(index) = state_db
        .sync_index(&intent.path)
        .map_err(|error| format!("cannot read sync index: {error}"))?
    else {
        return Ok(Some(
            "preserving local file of unknown provenance over a remote deletion",
        ));
    };
    if index.updated_at > intent.enqueued_at {
        return Ok(Some(
            "remote deletion is older than the last sync of this path",
        ));
    }
    if differs_from_indexed_content(&intent.path, &metadata, &index, local_hashes) {
        return Ok(Some(
            "local file was modified after the last sync; modification wins over deletion",
        ));
    }
    Ok(None)
}

/// Whether a local file holds other content than the index recorded.
/// The size and the quick check answer for most files. A file with the
/// indexed size and another modification time is decided by its
/// content, which only the probe's worker reads. With no hash from it
/// for the file as it is now (a large file, a file that changed after
/// the worker read it, a file it could not read) the file counts as
/// changed, which is the side that keeps data: the file survives and an
/// upload compares it off the thread.
fn differs_from_indexed_content(
    path: &Path,
    metadata: &fs::Metadata,
    index: &crate::state_db::SyncIndexEntry,
    local_hashes: &LocalHashes,
) -> bool {
    let (size_bytes, modified_at) = (metadata.len(), metadata.modified().ok());
    if size_bytes != index.size_bytes {
        return true;
    }
    if index.matches_local(size_bytes, modified_at) {
        return false;
    }
    local_hashes.hash_of(path, size_bytes, modified_at) != Some(index.content_hash.as_str())
}

/// The index rows a cloud deletion of `path` is checked against: the
/// row of a file, every row under a folder. The probe's worker reads
/// the files among them that only their content can clear.
fn touched_file_check(
    state_db: &DurableStateDb,
    path: &Path,
    algorithm: vapor_providers::HashAlgorithm,
) -> TouchedFileCheck {
    // A row that cannot be read leaves its file unread, and an unread
    // file is kept.
    let rows = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            state_db.sync_index_rows_under(path).unwrap_or_default()
        }
        Ok(metadata) if metadata.is_file() => state_db
            .sync_index(path)
            .ok()
            .flatten()
            .into_iter()
            .collect(),
        _ => Vec::new(),
    };
    TouchedFileCheck { rows, algorithm }
}

/// What a download worker needs to compare the local file its payload
/// displaces. Only a two-way apply keeps both; a mirror overwrites.
fn displaced_file_check(
    env: &ExecutionEnv<'_>,
    state_db: &DurableStateDb,
    plan: &TransferPlan,
) -> Option<DisplacedFileCheck> {
    (env.sync_mode == vapor_shared::SyncMode::TwoWay).then(|| DisplacedFileCheck {
        local_path: plan.local_path.clone(),
        index: state_db.sync_index(&plan.local_path).ok().flatten(),
        algorithm: env.hash_algorithm,
    })
}

/// Tells the provider about a write of this device whose outcome the
/// index never got. The cloud object at the path of `plan` holds
/// `content_hash` and carries `cloud_op_id`, a tag that this device
/// gave out for this profile, and the index row of the path records
/// another tag and another hash: the upload landed in a daemon that
/// stopped before it stored the outcome. A provider that owes work
/// after a write owes it for that one
/// (`Provider::note_unrecorded_write`).
///
/// The tag alone decides, so the call comes whatever the local file
/// holds now. Bytes that another device put in the cloud carry the
/// tag of that device, and bytes from a program that sets no tag carry
/// none: no call (the same checkout on two devices is such a case). A
/// row whose object was left as it was holds the tag of that object,
/// so a later write of a program that keeps the tag gives no call
/// either.
///
/// With no row, or a row with no hash, there is no base that a write
/// could have replaced: no call. Until the row records the object, a
/// plan that runs again makes the call again; the provider starts no
/// second piece of work for one write.
fn note_own_write_the_index_does_not_know(
    app: &DaemonApp,
    state_db: &DurableStateDb,
    env: &ExecutionEnv<'_>,
    plan: &TransferPlan,
    content_hash: &str,
    cloud_op_id: Option<&str>,
) {
    let Some(cloud_op_id) = cloud_op_id
        .filter(|op_id| op_id.starts_with(&own_op_id_prefix(env.device_id, env.profile_id)))
    else {
        return;
    };
    let Ok(Some(index)) = state_db.sync_index(&plan.local_path) else {
        return;
    };
    if index.last_op_id == cloud_op_id {
        return;
    }
    let Some(base_hash) = known_hash(&index.content_hash) else {
        return;
    };
    if base_hash == content_hash {
        return;
    }
    crate::logging::debug(
        "The cloud holds a write of this device that the sync index did not record; telling the provider",
        &[("path", plan.local_path.display().to_string())],
    );
    app.provider()
        .note_unrecorded_write(&plan.remote_path, base_hash, content_hash, cloud_op_id);
}

/// Records the post-upload sync-index entry; failures are logged, not
/// fatal (the next transfer overwrites the entry, and a stale index
/// resolves through the conflict-verification path).
fn record_upload_index(
    state_db: &mut DurableStateDb,
    plan: &TransferPlan,
    content_hash: &str,
    size_bytes: u64,
    remote_modified_at: Option<SystemTime>,
    last_op_id: &str,
    now: SystemTime,
) {
    // The recorded content_hash is the bytes we hashed. If the local file
    // changed since (a mid-transfer edit), recording the current mtime
    // would pair a post-edit mtime with the pre-edit hash, letting the
    // mtime fast-path in the divergence checks skip a needed re-hash and
    // silently overwrite the edit. Detect that and record no mtime, which
    // forces the hash path next time.
    let current = fs::symlink_metadata(&plan.local_path).ok();
    let current_mtime = current
        .as_ref()
        .and_then(|metadata| metadata.modified().ok());
    let current_size = current.as_ref().map(fs::Metadata::len);
    let local_modified_at = match plan.hashed_local_state {
        Some((hashed_size, hashed_mtime))
            if current_size == Some(hashed_size) && current_mtime == hashed_mtime =>
        {
            current_mtime
        }
        Some(_) => None,
        None => current_mtime,
    };
    write_sync_index_entry(
        state_db,
        plan,
        content_hash,
        size_bytes,
        local_modified_at,
        remote_modified_at,
        last_op_id,
        now,
    );
}

/// The op-id for the index row of a file whose cloud object this
/// intent did not write: the content was there already. `seen` is the
/// tag that a lookup of this intent found on the object (`Some(None)`
/// for an object with no tag). With no lookup the row keeps the tag it
/// has. The op-id of the intent itself is on no object, and a row that
/// held it would never match the object again.
fn op_id_of_an_object_left_as_it_is(
    state_db: &DurableStateDb,
    plan: &TransferPlan,
    seen: Option<Option<&str>>,
) -> String {
    match seen {
        Some(tag) => tag.unwrap_or("").to_string(),
        None => state_db
            .sync_index(&plan.local_path)
            .ok()
            .flatten()
            .map(|row| row.last_op_id)
            .unwrap_or_default(),
    }
}

fn record_download_index(
    state_db: &mut DurableStateDb,
    plan: &TransferPlan,
    content_hash: &str,
    size_bytes: u64,
    remote_modified_at: Option<SystemTime>,
    now: SystemTime,
) {
    // We just wrote this file; its current mtime describes exactly the
    // content we applied, so the mtime fast-path is safe to record.
    let local_modified_at = fs::symlink_metadata(&plan.local_path)
        .and_then(|metadata| metadata.modified())
        .ok();
    // Record the REMOTE object's op-id (whoever wrote it), not this
    // device's download id, so a later upload of this path correlates by
    // op-id. An untagged remote records an empty id (never matches), which
    // correctly falls through to the hash comparison.
    let last_op_id = plan.remote_op_id.as_deref().unwrap_or("");
    write_sync_index_entry(
        state_db,
        plan,
        content_hash,
        size_bytes,
        local_modified_at,
        remote_modified_at,
        last_op_id,
        now,
    );
}

#[allow(clippy::too_many_arguments)]
fn write_sync_index_entry(
    state_db: &mut DurableStateDb,
    plan: &TransferPlan,
    content_hash: &str,
    size_bytes: u64,
    local_modified_at: Option<SystemTime>,
    remote_modified_at: Option<SystemTime>,
    last_op_id: &str,
    now: SystemTime,
) {
    if let Err(error) = state_db.set_sync_index(
        &plan.local_path,
        content_hash,
        size_bytes,
        local_modified_at,
        remote_modified_at,
        last_op_id,
        now,
    ) {
        crate::logging::warning(
            "Could not record post-transfer sync index entry",
            &[("error", error.to_string())],
        );
    }
}

/// Records the id that the provider has for the cloud object of the
/// synced file `local_path`, when it has one. A failure is logged and
/// not fatal: the next scan that lists the file writes the id.
fn record_remote_id(state_db: &mut DurableStateDb, local_path: &Path, remote_id: Option<&str>) {
    let Some(remote_id) = remote_id else {
        return;
    };
    if let Err(error) = state_db.set_sync_index_remote_id(local_path, remote_id) {
        crate::logging::warning(
            "Could not record the id of a cloud object in the sync index",
            &[("error", error.to_string())],
        );
    }
}

/// The synced file this upload is a rename of, when there is one: an
/// index row with the same content whose local path is gone, on a
/// backend that can move objects. Only for a path with no index row
/// of its own (an edit is never a move) and a fresh create (the remote
/// destination is absent). Never for a conflict copy: the daemon made
/// that file itself while the canonical path is being restored, so
/// moving the cloud canonical under the copy's name would destroy the
/// other side's version. The same holds for any candidate whose path
/// has a restore queued.
fn move_source_for(
    state_db: &DurableStateDb,
    env: &ExecutionEnv<'_>,
    app: &DaemonApp,
    intent: &DurableIntentRecord,
    plan: &TransferPlan,
) -> Option<crate::state_db::SyncIndexEntry> {
    // The watcher reports a rename's destination as a `Rename` intent
    // and a copy's as an `Upload`; both travel the upload route.
    if !matches!(
        intent.kind,
        PendingIntentKind::Upload | PendingIntentKind::Rename
    ) || plan.precondition != RemotePrecondition::Absent
        || !app.provider().capabilities().supports_server_side_move
    {
        return None;
    }
    if intent
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| crate::conflict::parse_conflict_copy_name(name).is_some())
    {
        return None;
    }
    let content_hash = plan.content_hash.as_deref()?;
    let (size_bytes, _) = plan.hashed_local_state?;
    if state_db.sync_index(&intent.path).ok().flatten().is_some() {
        return None;
    }
    let _ = env;
    state_db
        .sync_index_by_content(content_hash, size_bytes)
        .ok()?
        .into_iter()
        .find(|entry| {
            entry.path != intent.path
                && !crate::name_collision::exactly_present(&entry.path)
                && !state_db
                    .queued_kinds_at(&entry.path)
                    .unwrap_or_default()
                    .iter()
                    .any(|kind| {
                        matches!(
                            kind,
                            PendingIntentKind::Download | PendingIntentKind::ApplyRemoteDelete
                        )
                    })
        })
}

/// A remote delete that completed while a directory stands at the
/// local path (a type-mismatch answered in favour of the local folder)
/// leaves that folder's content to upload; a subtree reconcile picks
/// it up now that the remote name is free.
fn reconcile_local_directory_left_behind(
    state_db: &mut DurableStateDb,
    local_path: &Path,
    now: SystemTime,
) {
    let Ok(metadata) = fs::symlink_metadata(local_path) else {
        return;
    };
    if metadata.is_file() {
        // The mirror case: the cloud folder is gone and a local file
        // has its name. The name is free for the file now.
        if let Err(error) = state_db.enqueue_intents_coalesced(
            &[(local_path.to_path_buf(), PendingIntentKind::Upload, now)],
            crate::safeguards::IntentSource::Fresh,
        ) {
            crate::logging::warning(
                "Could not queue the upload of a file after a remote delete",
                &[
                    ("path", local_path.display().to_string()),
                    ("error", error.to_string()),
                ],
            );
        }
        return;
    }
    if !metadata.is_dir() {
        return;
    }
    if let Err(error) = state_db.enqueue_intents_coalesced(
        &[(
            local_path.to_path_buf(),
            PendingIntentKind::ReconcileSubtree,
            now,
        )],
        crate::safeguards::IntentSource::Fresh,
    ) {
        crate::logging::warning(
            "Could not schedule the subtree reconcile after a remote delete",
            &[
                ("path", local_path.display().to_string()),
                ("error", error.to_string()),
            ],
        );
    }
}

fn forget_deleted_path(state_db: &mut DurableStateDb, local_path: &Path) {
    if let Err(error) = state_db.remove_sync_index(local_path) {
        crate::logging::warning(
            "Could not clear sync index entry after delete",
            &[("error", error.to_string())],
        );
    }
    // A deleted file that stood in for a colliding cloud name releases
    // the alias with it, whichever side deleted.
    if let Err(error) = state_db.remove_name_alias_for_local(local_path) {
        crate::logging::warning(
            "Could not release the name alias after a delete",
            &[("error", error.to_string())],
        );
    }
}

/// The size and the modification time of the file at `path`; `None`
/// for a folder and for a path with nothing at it.
fn file_size_and_time(path: &Path) -> Option<(u64, Option<SystemTime>)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    metadata
        .is_file()
        .then(|| (metadata.len(), metadata.modified().ok()))
}

fn local_size(path: &Path) -> u64 {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

/// Verifies that applying a remote-sourced change at `target` cannot
/// escape the canonical local sync root through a symlinked parent
/// component. `RemotePath` validation is purely lexical, so a symlink in
/// an intermediate directory (`root/link -> /elsewhere`) would otherwise
/// let a remote `link/x` write into (or delete under) `/elsewhere`.
/// Canonicalizes the deepest existing ancestor of the target's parent and
/// confirms it is still inside the root.
pub(crate) fn verify_within_local_root(local_root: &Path, target: &Path) -> Result<(), String> {
    // Canonicalize the root itself so the containment comparison holds even
    // when the caller passed a not-yet-canonical root (tests, or a root
    // reached through a symlinked parent of its own).
    let root =
        vapor_shared::paths::canonicalize(local_root).unwrap_or_else(|_| local_root.to_path_buf());
    let mut ancestor = target.parent().unwrap_or(local_root).to_path_buf();
    loop {
        match vapor_shared::paths::canonicalize(&ancestor) {
            Ok(real) => {
                if real == root || real.starts_with(&root) {
                    return Ok(());
                }
                return Err(format!(
                    "refusing remote apply of {}: parent resolves outside the sync root ({})",
                    target.display(),
                    real.display()
                ));
            }
            // The parent does not exist yet; create_dir_all will make
            // fresh real directories under the deepest existing ancestor.
            // Keep walking up to find it.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match ancestor.parent() {
                    Some(parent) if parent.starts_with(local_root) || parent == local_root => {
                        ancestor = parent.to_path_buf();
                    }
                    // Walked above the root without finding an existing
                    // ancestor inside it: lexically the target claimed to be
                    // under the root, so this is a malformed path.
                    _ => {
                        return Err(format!(
                            "refusing remote apply of {}: no existing ancestor inside the sync root",
                            target.display()
                        ));
                    }
                }
            }
            Err(error) => {
                return Err(format!(
                    "cannot verify sync-root containment of {}: {error}",
                    ancestor.display()
                ));
            }
        }
    }
}

/// Method-form conflict finisher shared by the upload gate and the
/// precondition-failure arm.
impl StagedExecutor {
    fn finish_as_conflict(
        &mut self,
        app: &mut DaemonApp,
        state_db: &mut DurableStateDb,
        env: &mut ExecutionEnv<'_>,
        intent: &DurableIntentRecord,
        now: SystemTime,
        report: &mut StagedExecutorReport,
    ) -> Result<(), StateDbError> {
        match resolve_upload_conflict(env, state_db, intent, now) {
            PlanOutcome::ConflictResolved => {
                report.conflict_paths.push(intent.path.clone());
                self.complete(state_db, intent, report)
            }
            PlanOutcome::Noop(_) => self.complete(state_db, intent, report),
            PlanOutcome::Fail { failure, message } => {
                self.resolve_failure(app, state_db, intent, failure, &message, now, report)
            }
            _ => unreachable!("conflict resolution has no other outcomes"),
        }
    }
}

/// ApplyRemoteDelete happens inline in the planner: local deletion is a
/// metadata operation, and splitting it into more stages would only add
/// latency to the remote→local pipeline.
///
/// Directory deletions are the delicate case. In two-way mode a remote
/// folder deletion arrives as one `Removed(dir)` change, but the local
/// subtree may hold unsynced or locally-modified files (data preservation
/// wins over deletion). Rather than `remove_dir_all` the whole tree, the
/// two-way path walks it and removes only synced-and-unchanged files,
/// preserving (and re-uploading) the rest. One-way pull is a strict
/// mirror and removes the tree unconditionally.
fn apply_remote_delete_locally(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    path: &Path,
    local_hashes: &LocalHashes,
    now: SystemTime,
) -> PlanOutcome {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            PlanOutcome::Noop("local file already gone")
        }
        Err(error) => PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message: format!("cannot stat local file for remote-delete apply: {error}"),
        },
        Ok(metadata) => {
            if metadata.is_dir() {
                if env.sync_mode == vapor_shared::SyncMode::TwoWay {
                    return guarded_remove_dir(env, state_db, path, local_hashes, now);
                }
                return match discard_local(env, path, now) {
                    Ok(()) => {
                        let _ = env.tags.remove(path);
                        env.local_echoes.record_delete(path_key(path), now);
                        PlanOutcome::AppliedLocally
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        PlanOutcome::Noop("local file already gone")
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
                        PlanOutcome::DiscardOnWorker {
                            cloud_folder_waits: false,
                        }
                    }
                    Err(error) => PlanOutcome::Fail {
                        failure: RetryFailureKind::Transient,
                        message: format!(
                            "cannot delete local directory for remote-delete apply: {error}"
                        ),
                    },
                };
            }
            match discard_local(env, path, now) {
                Ok(()) => {
                    let _ = env.tags.remove(path);
                    env.local_echoes.record_delete(path_key(path), now);
                    PlanOutcome::AppliedLocally
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    PlanOutcome::Noop("local file already gone")
                }
                Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
                    PlanOutcome::DiscardOnWorker {
                        cloud_folder_waits: false,
                    }
                }
                Err(error) => PlanOutcome::Fail {
                    failure: RetryFailureKind::Transient,
                    message: format!("cannot delete local file for remote-delete apply: {error}"),
                },
            }
        }
    }
}

/// Removes a local file or directory the way the configuration asks:
/// into the trash when there is one, else by unlinking. A `pull-only`
/// mirror removal and a cloud deletion applied in `two-way` are the
/// two reasons, and the trash entry records which.
fn discard_local(env: &ExecutionEnv<'_>, path: &Path, now: SystemTime) -> std::io::Result<()> {
    let Some(trash) = env.trash else {
        let metadata = fs::symlink_metadata(path)?;
        return if metadata.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
    };
    let reason = if env.sync_mode == vapor_shared::SyncMode::TwoWay {
        constants::trash::REASON_CLOUD_DELETE
    } else {
        constants::trash::REASON_MIRROR_DELETE
    };
    // Never a copy here: this is the tick thread, and a copy reads
    // the file. `CrossesDevices` tells the caller to use a worker.
    match trash.discard_without_copy(path, reason, now)? {
        crate::trash::Disposition::Managed(id) => crate::logging::info(
            "Moved a file Vapor removed on this device to the trash",
            &[
                ("path", path.display().to_string()),
                ("trash_entry", id),
                ("reason", reason.to_string()),
            ],
        ),
        crate::trash::Disposition::System(landed) => crate::logging::info(
            "Moved a file Vapor removed on this device to the user's trash",
            &[
                ("path", path.display().to_string()),
                ("landed", landed.display().to_string()),
                ("reason", reason.to_string()),
            ],
        ),
        crate::trash::Disposition::Removed => {}
    }
    Ok(())
}

/// Two-way guarded recursive delete of a remotely-removed directory.
/// Returns `AppliedLocally` when the whole subtree (including the root
/// directory) was removed, or a `Noop` describing the preservation when
/// at least one entry was kept.
fn guarded_remove_dir(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    dir: &Path,
    local_hashes: &LocalHashes,
    now: SystemTime,
) -> PlanOutcome {
    match remove_dir_preserving_unsynced(env, state_db, dir, local_hashes, now) {
        Ok(true) => match fs::remove_dir(dir) {
            Ok(()) => {
                let _ = env.tags.remove(dir);
                env.local_echoes.record_delete(path_key(dir), now);
                PlanOutcome::AppliedLocally
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                PlanOutcome::Noop("local directory already gone")
            }
            // Something reappeared under the directory between the walk and
            // this rmdir: leave it, the next reconcile converges.
            Err(_) => PlanOutcome::Noop(
                "preserved local content that reappeared under a deleted directory",
            ),
        },
        Ok(false) => {
            PlanOutcome::Noop("preserved unsynced local content under a remotely-deleted directory")
        }
        Err(message) => PlanOutcome::Fail {
            failure: RetryFailureKind::Transient,
            message,
        },
    }
}

/// Depth-first guarded delete. Returns `true` when everything inside the
/// directory was removed, `false` when at least one entry was preserved.
/// A synced-and-unchanged file is removed; an unsynced, locally-modified,
/// or unknown-provenance file (or any symlink/special node) is preserved
/// and its Upload re-enqueued so the remote is restored.
fn remove_dir_preserving_unsynced(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    dir: &Path,
    local_hashes: &LocalHashes,
    now: SystemTime,
) -> Result<bool, String> {
    let entries = fs::read_dir(dir)
        .map_err(|error| format!("cannot read directory for remote-delete apply: {error}"))?;
    let mut all_removed = true;
    // The files that an OS wrote into this folder by itself. They are
    // not synced, so no index row vouches for them, and they are no
    // content of the user either: they go with the folder, to the
    // trash, when nothing else stays in it.
    let mut os_files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read directory entry: {error}"))?;
        let child = entry.path();
        let metadata = match fs::symlink_metadata(&child) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("cannot stat {}: {error}", child.display())),
        };
        if is_os_folder_file(&child, &metadata) && matches!(state_db.sync_index(&child), Ok(None)) {
            os_files.push(child);
            continue;
        }
        if metadata.is_dir() {
            if remove_dir_preserving_unsynced(env, state_db, &child, local_hashes, now)? {
                match fs::remove_dir(&child) {
                    Ok(()) => {
                        let _ = env.tags.remove(&child);
                        env.local_echoes.record_delete(path_key(&child), now);
                        forget_deleted_path(state_db, &child);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => all_removed = false,
                }
            } else {
                all_removed = false;
            }
        } else if metadata.is_file() {
            if child_delete_is_safe(state_db, &child, &metadata, local_hashes)? {
                match discard_local(env, &child, now) {
                    Ok(()) => {
                        let _ = env.tags.remove(&child);
                        env.local_echoes.record_delete(path_key(&child), now);
                        forget_deleted_path(state_db, &child);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
                        // This file needs a copy to reach the trash. It
                        // gets a removal of its own, which a worker
                        // carries out; the folder goes when it is empty.
                        all_removed = false;
                        if let Err(error) = state_db.enqueue_intents_coalesced(
                            &[(child.clone(), PendingIntentKind::ApplyRemoteDelete, now)],
                            crate::safeguards::IntentSource::Fresh,
                        ) {
                            return Err(format!(
                                "cannot queue the removal of {}: {error}",
                                child.display()
                            ));
                        }
                    }
                    Err(error) => {
                        return Err(format!("cannot delete {}: {error}", child.display()));
                    }
                }
            } else if env
                .path_filter
                .is_some_and(|filter| filter.should_ignore_kind(&child, false))
            {
                // A file that an ignore rule hides is kept, and it is
                // not sent: an ignored name never syncs. An upload of
                // it would put the folder back in the cloud.
                all_removed = false;
            } else {
                all_removed = false;
                if let Err(error) = state_db.enqueue_intents_coalesced(
                    &[(child.clone(), PendingIntentKind::Upload, now)],
                    crate::safeguards::IntentSource::Fresh,
                ) {
                    crate::logging::warning(
                        "Could not enqueue re-upload for preserved file under deleted directory",
                        &[
                            ("path", child.display().to_string()),
                            ("error", error.to_string()),
                        ],
                    );
                }
            }
        } else {
            // Symlinks and special files are outside the sync contract;
            // never follow or delete them.
            all_removed = false;
        }
    }
    if !all_removed {
        // Something else stays, so the folder stays as it is.
        return Ok(false);
    }
    for os_file in os_files {
        match discard_local(env, &os_file, now) {
            Ok(()) => env.local_echoes.record_delete(path_key(&os_file), now),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            // The trash is on another volume and takes a copy, which a
            // worker makes: the file stays for now, and the folder
            // with it.
            Err(_) => all_removed = false,
        }
    }
    Ok(all_removed)
}

/// Whether a file under a remotely-deleted directory is safe to remove:
/// only when the sync index proves it is exactly what was last synced.
/// Unknown provenance or any divergence fails safe to `false`
/// (preserve).
fn child_delete_is_safe(
    state_db: &mut DurableStateDb,
    path: &Path,
    metadata: &fs::Metadata,
    local_hashes: &LocalHashes,
) -> Result<bool, String> {
    let Some(index) = state_db
        .sync_index(path)
        .map_err(|error| format!("cannot read sync index for {}: {error}", path.display()))?
    else {
        return Ok(false);
    };
    Ok(!differs_from_indexed_content(
        path,
        metadata,
        &index,
        local_hashes,
    ))
}

/// Removes what a download left in its staging file when the attempt
/// ends without an apply. The payload is a full copy of the file, and
/// it sits in the user's folder.
fn discard_staged_payload(plan: &TransferPlan) {
    if let Some(staging) = &plan.staging_path {
        let _ = fs::remove_file(staging);
    }
}

/// What the apply of a downloaded payload did.
#[derive(Debug, PartialEq)]
enum DownloadApply {
    /// The payload is at the path now.
    Applied,
    /// The payload is at the path, and the local file it displaced is
    /// next to it as a conflict copy.
    KeptBoth,
    /// Nothing was applied: the payload is the content last synced, as
    /// this index row has it, and a local file is at the path.
    RemoteUnchanged(crate::state_db::SyncIndexEntry),
}

/// Moves a fully-downloaded staging payload into place atomically and
/// tags it with the operation id so the watcher echo correlates.
fn apply_downloaded_payload(
    env: &mut ExecutionEnv<'_>,
    plan: &TransferPlan,
) -> std::io::Result<()> {
    let staging = plan
        .staging_path
        .as_ref()
        .expect("download plans always carry a staging path");
    if let Some(parent) = plan.local_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if is_empty_folder(&plan.local_path) {
        // The cloud put a file where a folder was, and the folder's
        // files are gone already. An empty folder holds nothing to
        // lose; `remove_dir` refuses one that holds anything.
        let _ = fs::remove_dir(&plan.local_path);
    }
    fs::rename(staging, &plan.local_path)?;
    // Tag after the rename: the side-file fallback must name the final
    // path, and the echo cache carries hash + size for the window
    // between rename and tag landing. A payload that came down
    // read-only refuses the tag as it is, and the fallback would put a
    // side file next to it in the user's folder (inside a `.git`, that
    // is a file git calls garbage), so its owner may write it for the
    // moment of the tag.
    let tagged = vapor_platform::fs_ops::with_owner_write(&plan.local_path, || {
        env.tags.write_op_id(&plan.local_path, &plan.op_id)
    });
    if let Err(error) = tagged {
        crate::logging::warning(
            "Applied downloaded payload but could not record its op-id tag",
            &[
                ("path", plan.local_path.display().to_string()),
                ("error", error.to_string()),
            ],
        );
    }
    Ok(())
}

/// Two-way download apply that closes the check-then-act window. The
/// existing local file (if any) is renamed aside first: atomically
/// capturing whatever bytes are present, then the canonical payload is
/// renamed in, then the displaced bytes are examined and either dropped
/// (they matched the sync index or the incoming payload) or promoted to a
/// keep-both conflict copy (they diverged). The divergence decision runs
/// on the displaced file as it is after the rename: its size and
/// modification time against the sync index, and against the file
/// `displaced_hash` was read from by the download worker. A local edit
/// present at apply time changes one of the two, so it is never
/// silently overwritten.
///
/// A payload that is the content last synced displaces nothing: the
/// cloud object was touched and not changed, so a local file that
/// differs holds the one change there is, and replacing it with the
/// payload would turn that edit into a conflict copy under old bytes.
fn apply_downloaded_payload_keep_both(
    env: &mut ExecutionEnv<'_>,
    state_db: &mut DurableStateDb,
    intent: &DurableIntentRecord,
    plan: &TransferPlan,
    incoming_hash: &str,
    displaced_hash: Option<&LocalFileHash>,
    now: SystemTime,
) -> Result<DownloadApply, String> {
    // A crash between the two renames below leaves the user's bytes
    // under this name; the next walk gives them back as a conflict copy.
    let aside = crate::conflict::aside_path(&plan.local_path, &plan.op_id);

    // Read the pre-apply index before renaming; it still keys off the
    // canonical path and describes the bytes we are about to displace.
    let index = state_db
        .sync_index(&plan.local_path)
        .map_err(|error| format!("cannot read sync index: {error}"))?;

    if let Some(index) = &index
        && known_hash(&index.content_hash) == Some(incoming_hash)
        && fs::symlink_metadata(&plan.local_path).is_ok_and(|metadata| metadata.is_file())
    {
        if let Some(staging) = &plan.staging_path {
            let _ = fs::remove_file(staging);
        }
        return Ok(DownloadApply::RemoteUnchanged(index.clone()));
    }

    // The size and mtime of the file once it is set aside describe the
    // displaced bytes: an edit that landed before the rename shows in
    // them. With the index they answer "unchanged since the last sync"
    // without a hash for the common case.
    let displaced = match fs::symlink_metadata(&plan.local_path) {
        Ok(metadata) if metadata.is_file() => match fs::rename(&plan.local_path, &aside) {
            Ok(()) => Some(match fs::symlink_metadata(&aside) {
                Ok(aside_metadata) => (aside_metadata.len(), aside_metadata.modified().ok()),
                // No time to compare: the file counts as diverged.
                Err(_) => (metadata.len(), None),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(format!("cannot set aside local file before apply: {error}"));
            }
        },
        // Directory or special node at the path: apply_downloaded_payload's
        // rename will surface an appropriate error. Nothing to preserve.
        _ => None,
    };

    if let Err(error) = apply_downloaded_payload(env, plan) {
        // Restore the displaced file so a failed apply loses nothing.
        if displaced.is_some() {
            let _ = fs::rename(&aside, &plan.local_path);
        }
        return Err(format!("local apply of downloaded payload failed: {error}"));
    }

    let Some((aside_size, aside_modified_at)) = displaced else {
        return Ok(DownloadApply::Applied);
    };

    // The displaced bytes are unchanged when the quick check says they
    // were not touched since the last sync, or when their content is
    // the incoming payload or the indexed content. The tick thread
    // reads no file: the content is the hash the download worker read,
    // and it counts only for the file that has the size and the time
    // the worker saw. With no such hash (a large file, a file written
    // after the worker read it) the file counts as diverged and is kept
    // as a copy.
    let quick_unchanged = index
        .as_ref()
        .is_some_and(|index| index.matches_local(aside_size, aside_modified_at));
    let unchanged = quick_unchanged
        || displaced_hash
            .and_then(|hash| hash.of_file_with(aside_size, aside_modified_at))
            .is_some_and(|aside_hash| {
                aside_hash == incoming_hash
                    || index
                        .as_ref()
                        .is_some_and(|index| aside_hash == index.content_hash)
            });
    if unchanged {
        let _ = fs::remove_file(&aside);
        let _ = env.tags.remove(&aside);
        return Ok(DownloadApply::Applied);
    }

    // The displaced bytes diverged from both the last sync and the
    // incoming payload: keep both.
    let timestamp_ms = intent
        .enqueued_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let conflict_local = crate::conflict::conflict_copy_path(
        &plan.local_path,
        env.device_id,
        timestamp_ms,
        |candidate: &Path| candidate.exists(),
    );
    if let Err(error) = fs::rename(&aside, &conflict_local) {
        return Err(format!(
            "cannot stage conflict copy from displaced file: {error}"
        ));
    }
    let _ = env.tags.relocate_side_file(&aside, &conflict_local);
    if let Err(error) = state_db.enqueue_intents_coalesced(
        &[(conflict_local.clone(), PendingIntentKind::Upload, now)],
        crate::safeguards::IntentSource::Fresh,
    ) {
        return Err(format!("cannot enqueue conflict-copy upload: {error}"));
    }
    crate::logging::warning(
        "Kept both versions on download-apply (local edit diverged from the incoming payload)",
        &[
            ("canonical", plan.local_path.display().to_string()),
            ("conflict_copy", conflict_local.display().to_string()),
        ],
    );
    Ok(DownloadApply::KeptBoth)
}

fn allocate_op_id(env: &ExecutionEnv<'_>, intent: &DurableIntentRecord, now: SystemTime) -> String {
    let now_ms = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!(
        "{}{}-a{}-t{now_ms}",
        own_op_id_prefix(env.device_id, env.profile_id),
        intent.id,
        intent.attempt_count
    )
}

/// What every op-id that this device gives out for this profile starts
/// with, and no op-id of another device or another profile does.
pub(crate) fn own_op_id_prefix(device_id: &str, profile_id: &str) -> String {
    format!("{device_id}-{}-op", profile_tag(profile_id))
}

/// A profile id as it goes into an op-id: short and of a fixed length,
/// because a provider limits how long a tag may be (Google Drive: 124
/// bytes for the name and the value together).
fn profile_tag(profile_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(profile_id.as_bytes());
    format!(
        "p{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
}

fn sanitize_for_file_name(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Assembles the by-value context a provider job carries to its worker.
fn job_context(
    app: &DaemonApp,
    env: &ExecutionEnv<'_>,
    clock: &Arc<dyn Clock>,
    root_identity: &Arc<crate::provider_jobs::RootIdentityReads>,
) -> JobContext {
    JobContext {
        provider: app.provider_arc(),
        bandwidth: env.bandwidth.clone(),
        clock: clock.clone(),
        root_identity: root_identity.clone(),
    }
}

fn probe_saw_cloud_root_unavailable(probe: &ProbeResult) -> bool {
    let unavailable = |result: &Result<_, ProviderError>| {
        result.as_ref().err().is_some_and(|error| {
            error.kind == vapor_shared::ProviderErrorKind::CloudRootUnavailable
        })
    };
    probe.stat.as_ref().is_some_and(|stat| {
        stat.as_ref().err().is_some_and(|error| {
            error.kind == vapor_shared::ProviderErrorKind::CloudRootUnavailable
        })
    }) || probe.content_hash.as_ref().is_some_and(unavailable)
}

/// Aborts the session inside an outcome that will not be applied
/// (mismatched stage, aborted execution). Other outcomes carry no
/// live resources.
fn abort_outcome_session(outcome: ProviderJobOutcome) {
    if let ProviderJobOutcome::TransferHeld { mut session, .. } = outcome {
        session.abort();
    }
}

pub(crate) fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn max_in_flight_items(workgate: WorkgateSnapshot) -> usize {
    workgate.caps.planner_workers
        + workgate.caps.hash_workers
        + workgate.caps.upload_concurrency
        + workgate.caps.download_concurrency
}

/// Chunked content hash of a local file in the provider's algorithm:
/// at most `max_bytes` read per step, so a worker that hashes a large
/// file checks the throttle between two steps.
pub(crate) struct StreamingFileHash {
    file: fs::File,
    hasher: HashState,
    /// Reused across steps; hashing a large file takes many and must
    /// not allocate 64 KiB on each.
    buffer: Vec<u8>,
}

enum HashState {
    Sha256(Sha256),
    Md5(md5::Md5),
}

impl HashState {
    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(hasher) => hasher.update(bytes),
            Self::Md5(hasher) => hasher.update(bytes),
        }
    }

    fn finalize_hex(&mut self) -> String {
        let digest: Vec<u8> = match self {
            Self::Sha256(hasher) => std::mem::take(hasher).finalize().to_vec(),
            Self::Md5(hasher) => std::mem::take(hasher).finalize().to_vec(),
        };
        let mut hex = String::with_capacity(digest.len() * 2);
        for byte in digest {
            hex.push_str(&format!("{byte:02x}"));
        }
        hex
    }
}

impl StreamingFileHash {
    fn open(path: &Path, algorithm: vapor_providers::HashAlgorithm) -> std::io::Result<Self> {
        Ok(Self {
            file: fs::File::open(path)?,
            hasher: match algorithm {
                vapor_providers::HashAlgorithm::Sha256 => HashState::Sha256(Sha256::new()),
                vapor_providers::HashAlgorithm::Md5 => HashState::Md5(md5::Md5::new()),
            },
            buffer: vec![0_u8; 64 * 1024],
        })
    }

    /// Returns `Some(hex)` once the file is fully hashed.
    pub(crate) fn step(&mut self, max_bytes: u64) -> std::io::Result<Option<String>> {
        crate::local_hash::note_content_read();
        let mut remaining = max_bytes;
        while remaining > 0 {
            let chunk = self.buffer.len().min(remaining as usize);
            let read = self.file.read(&mut self.buffer[..chunk])?;
            if read == 0 {
                return Ok(Some(self.hasher.finalize_hex()));
            }
            self.hasher.update(&self.buffer[..read]);
            remaining -= read as u64;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::event_intents::PendingIntentKind;
    use crate::throttle::ThrottleInputs;
    use std::time::Duration;
    use vapor_platform::fs_caps::NativeFilesystemCapabilities;
    use vapor_providers::FilesystemProvider;
    use vapor_providers::filesystem::hash_hex_of_bytes;

    struct Fixture {
        _temp: tempfile::TempDir,
        local_root: PathBuf,
        cloud_root: PathBuf,
        /// The cloud root as a profile configures it; set by the tests
        /// that adopt the root.
        cloud_root_text: Option<String>,
        sync_mode: vapor_shared::SyncMode,
        bandwidth: Arc<crate::bandwidth::TransferBandwidth>,
        app: DaemonApp,
        state_db: DurableStateDb,
        executor: StagedExecutor,
        tags: OpIdTagStore,
        local_echoes: SelfWriteCache,
        remote_echoes: SelfWriteCache,
        clock: Arc<ManualClock>,
        /// What the runtime would say about local events that are not
        /// in the queue yet.
        local_changes_pending: bool,
        /// The trash of the profile, for the tests that have one.
        trash: Option<Arc<crate::trash::LocalTrash>>,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_provider(|cloud_root| {
                Box::new(FilesystemProvider::with_root(cloud_root).expect("provider"))
            })
        }

        /// A fixture whose provider is built by `make` over the cloud
        /// root, for tests that wrap the filesystem provider.
        fn with_provider(make: impl FnOnce(&Path) -> Box<dyn vapor_providers::Provider>) -> Self {
            let temp = tempfile::TempDir::new().expect("temp dir");
            let local_root = temp.path().join("local");
            let cloud_root = temp.path().join("cloud");
            std::fs::create_dir_all(&local_root).expect("local root");
            std::fs::create_dir_all(&cloud_root).expect("cloud root");
            let clock = Arc::new(ManualClock::at_now());
            let provider = make(&cloud_root);
            let app = DaemonApp::new_with_clock(provider, clock.clone());
            let state_db = DurableStateDb::open(temp.path().join("state/vapor.sqlite"))
                .expect("open state db");
            Self {
                local_root,
                cloud_root,
                cloud_root_text: None,
                sync_mode: vapor_shared::SyncMode::TwoWay,
                bandwidth: Arc::default(),
                _temp: temp,
                app,
                state_db,
                executor: StagedExecutor::with_clock(clock.clone()),
                tags: OpIdTagStore::new(Arc::new(NativeFilesystemCapabilities::for_current_host())),
                local_echoes: SelfWriteCache::new(),
                remote_echoes: SelfWriteCache::new(),
                clock,
                local_changes_pending: false,
                trash: None,
            }
        }

        /// Enqueues and leases one intent. A deletion comes back as
        /// one that has already waited out its move-settle window (its
        /// first planning would otherwise only defer), so the tests
        /// exercise the deletion itself.
        fn enqueue_and_lease(
            &mut self,
            path: &Path,
            kind: PendingIntentKind,
        ) -> DurableIntentRecord {
            self.state_db
                .enqueue_intent(path, kind, timestamp_ms(0))
                .expect("enqueue intent");
            let leased = self
                .state_db
                .lease_next_ready(fixture_now())
                .expect("lease next ready")
                .expect("leased intent");
            if !matches!(
                kind,
                PendingIntentKind::Delete | PendingIntentKind::ApplyRemoteDelete
            ) {
                return leased;
            }
            self.state_db
                .defer_leased(leased.id, timestamp_ms(0), "settled")
                .expect("defer");
            self.state_db
                .lease_next_ready(fixture_now())
                .expect("lease next ready")
                .expect("leased intent")
        }

        /// Drives the executor until the given intent completes or the
        /// tick budget runs out. Returns the accumulated report.
        fn run_to_quiescence(&mut self, max_ticks: usize) -> StagedExecutorReport {
            let mut total = StagedExecutorReport::default();
            for _ in 0..max_ticks {
                self.clock.advance(Duration::from_millis(250));
                let hash_algorithm = self.app.provider().content_hash_algorithm();
                let mut env = ExecutionEnv {
                    local_root: Some(&self.local_root),
                    cloud_root: self.cloud_root_text.as_deref(),
                    sync_mode: self.sync_mode,
                    device_id: "testdev",
                    profile_id: "default",
                    hash_algorithm,
                    bandwidth: &self.bandwidth,
                    move_hints: &mut MoveHints::default(),
                    tags: &self.tags,
                    local_echoes: &mut self.local_echoes,
                    remote_echoes: &mut self.remote_echoes,
                    trash: self.trash.as_ref(),
                    local_changes_pending: self.local_changes_pending,
                    remote_changes_pending: false,
                    path_filter: None,
                };
                // Wall time sits past the move-settle window, so a
                // delete under test is never mistaken for a rename in
                // flight; move tests seed their own timing.
                let report = self
                    .executor
                    .advance(&mut self.app, &mut self.state_db, &mut env, fixture_now())
                    .expect("advance");
                total.completed += report.completed;
                total.retried += report.retried;
                total.failed += report.failed;
                total.mirror_deletes += report.mirror_deletes;
                total.cloud_root_suspect |= report.cloud_root_suspect;
                if self.executor.snapshot().active_total == 0 {
                    break;
                }
            }
            total
        }

        /// Adopts the cloud root as a profile does at its first contact:
        /// the provider writes its marker and the profile records the
        /// identity.
        fn adopt_cloud_root(&mut self) {
            let text = self.cloud_root.to_string_lossy().into_owned();
            let identity = self
                .app
                .provider()
                .adopt_root(&text, "testdev")
                .expect("adopt")
                .expect("the filesystem provider has an identity");
            crate::root_identity::record_identity(
                &mut self.state_db,
                crate::root_identity::RootSide::Cloud,
                &identity,
                timestamp_ms(0),
            )
            .expect("record identity");
            self.cloud_root_text = Some(text);
        }

        /// Puts an empty folder where the cloud root was: a volume that
        /// went away with a folder left at its mount point.
        fn swap_cloud_root(&self) {
            let parked = self.cloud_root.with_file_name("cloud-that-went-away");
            std::fs::rename(&self.cloud_root, parked).expect("park the root");
            std::fs::create_dir(&self.cloud_root).expect("empty folder in its place");
        }
    }

    impl Fixture {
        /// Drives the executor over its worker threads until nothing is
        /// active (the test turned them on). Bounded by iteration
        /// count, not wall time: the loop ends as soon as the workers
        /// deliver, and only a genuine deadlock could exhaust it.
        fn run_on_worker_threads(&mut self) -> StagedExecutorReport {
            let mut total = StagedExecutorReport::default();
            for _ in 0..1_000_000 {
                let hash_algorithm = self.app.provider().content_hash_algorithm();
                let mut env = ExecutionEnv {
                    local_root: Some(&self.local_root),
                    cloud_root: self.cloud_root_text.as_deref(),
                    sync_mode: self.sync_mode,
                    device_id: "testdev",
                    profile_id: "default",
                    hash_algorithm,
                    bandwidth: &self.bandwidth,
                    move_hints: &mut MoveHints::default(),
                    tags: &self.tags,
                    local_echoes: &mut self.local_echoes,
                    remote_echoes: &mut self.remote_echoes,
                    trash: None,
                    local_changes_pending: false,
                    remote_changes_pending: false,
                    path_filter: None,
                };
                let report = self
                    .executor
                    .advance(&mut self.app, &mut self.state_db, &mut env, fixture_now())
                    .expect("advance");
                total.completed += report.completed;
                total.failed += report.failed;
                total.conflict_paths.extend(report.conflict_paths);
                if self.executor.snapshot().active_total == 0 {
                    break;
                }
                std::thread::yield_now();
            }
            total
        }

        /// The conflict copies of `name` in the local root.
        fn conflict_copies_of(&self, stem: &str) -> Vec<PathBuf> {
            std::fs::read_dir(&self.local_root)
                .expect("read local root")
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with(&format!("{stem}~conflict-")))
                })
                .collect()
        }
    }

    /// The fixture's wall clock: past the move-settle window, so a
    /// delete under test is never mistaken for a rename in flight.
    #[test]
    fn a_small_transfer_takes_the_lane_only_where_commits_are_shared() {
        // The filesystem provider flushes one drive for the transfers
        // of a moment: its small transfers have a lane.
        let mut shared = DaemonApp::new(Box::new(FilesystemProvider::new()));
        shared.apply_throttle_inputs(ThrottleInputs::default());
        let slots = shared.workgate_snapshot().caps.upload_concurrency;
        let lane = slots * constants::engine::SMALL_TRANSFERS_PER_SLOT;
        let small: Vec<_> = (0..lane + slots)
            .map(|_| acquire_transfer(&mut shared, WorkClass::Upload, true).expect("a permit"))
            .collect();
        assert_eq!(
            small
                .iter()
                .filter(|permit| permit.class == WorkClass::SmallUpload)
                .count(),
            lane,
            "the lane fills first, and the slots take what is left"
        );
        assert!(acquire_transfer(&mut shared, WorkClass::Upload, true).is_err());
        assert_eq!(
            acquire_transfer(&mut shared, WorkClass::Download, true)
                .expect("the other direction")
                .class,
            WorkClass::SmallDownload
        );
        // A transfer that is not small never takes a place in the lane.
        assert_eq!(
            acquire_transfer(&mut shared, WorkClass::Download, false)
                .expect("a slot")
                .class,
            WorkClass::Download
        );

        // A provider that pays for each transfer on its own has none.
        let mut alone = DaemonApp::default();
        alone.apply_throttle_inputs(ThrottleInputs::default());
        assert_eq!(
            acquire_transfer(&mut alone, WorkClass::Upload, true)
                .expect("a slot")
                .class,
            WorkClass::Upload
        );
    }

    fn fixture_now() -> SystemTime {
        timestamp_ms(constants::engine::MOVE_SETTLE_DELAY_SECONDS * 1_000 + 1_000)
    }

    fn timestamp_ms(milliseconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(milliseconds)
    }

    #[test]
    fn upload_intent_copies_real_bytes_to_the_provider() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("docs/report.md");
        std::fs::create_dir_all(local_file.parent().unwrap()).expect("dirs");
        std::fs::write(&local_file, b"real payload").expect("seed local");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(16);

        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        let uploaded = fixture.cloud_root.join("docs/report.md");
        assert_eq!(std::fs::read(&uploaded).expect("uploaded"), b"real payload");
        // The remote echo cache carries the op-id + hash of our write.
        assert!(
            fixture
                .remote_echoes
                .has_write_record("docs/report.md", timestamp_ms(1))
        );
        assert_eq!(
            fixture.state_db.queue_depth().expect("queue depth"),
            0,
            "intent must complete durably"
        );
    }

    /// Records `body` as the content last synced at `local_file`.
    fn seed_synced_index(fixture: &mut Fixture, local_file: &Path, body: &[u8]) {
        fixture
            .state_db
            .set_sync_index(
                local_file,
                &hash_hex_of_bytes(body),
                body.len() as u64,
                None,
                None,
                "op-mine",
                timestamp_ms(0),
            )
            .expect("seed index");
    }

    #[test]
    fn delete_intent_removes_the_remote_object() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("a.txt");
        std::fs::write(fixture.cloud_root.join("a.txt"), b"remote copy").expect("seed remote");
        seed_synced_index(&mut fixture, &local_file, b"remote copy");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Delete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        assert!(!fixture.cloud_root.join("a.txt").exists());
    }

    /// Leases a fresh deletion of a synced file, as its first planning
    /// meets it.
    fn start_first_planning_of_a_deletion(fixture: &mut Fixture) -> PathBuf {
        let local = fixture.local_root.join("gone.txt");
        std::fs::write(fixture.cloud_root.join("gone.txt"), b"synced").expect("remote");
        seed_synced_index(fixture, &local, b"synced");
        fixture
            .state_db
            .enqueue_intent(&local, PendingIntentKind::Delete, timestamp_ms(0))
            .expect("enqueue");
        let intent = fixture
            .state_db
            .lease_next_ready(fixture_now())
            .expect("lease")
            .expect("intent");
        assert_eq!(intent.attempt_count, 0);
        fixture
            .executor
            .try_start(&mut fixture.app, intent, timestamp_ms(0));
        fixture.cloud_root.join("gone.txt")
    }

    #[test]
    fn a_deletion_does_not_wait_when_no_create_can_follow() {
        let mut fixture = Fixture::new();
        let cloud = start_first_planning_of_a_deletion(&mut fixture);

        let report = fixture.run_to_quiescence(8);

        assert_eq!((report.completed, report.retried), (1, 0));
        assert!(!cloud.exists(), "the deletion reached the cloud at once");
    }

    #[test]
    fn a_deletion_waits_while_local_events_are_still_in_their_quiet_window() {
        let mut fixture = Fixture::new();
        fixture.local_changes_pending = true;
        let cloud = start_first_planning_of_a_deletion(&mut fixture);

        let report = fixture.run_to_quiescence(8);

        assert_eq!((report.completed, report.retried), (0, 1));
        assert!(cloud.exists(), "the create half of a rename may still come");
    }

    #[test]
    fn a_delete_with_no_index_row_keeps_the_cloud_file_and_downloads_it() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("a.txt");
        std::fs::write(fixture.cloud_root.join("a.txt"), b"never compared here")
            .expect("seed remote");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Delete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("a.txt")).expect("cloud file kept"),
            b"never compared here"
        );
        let restored = fixture
            .state_db
            .lease_next_ready(fixture_now())
            .expect("lease")
            .expect("a download brings the file back");
        assert_eq!(restored.kind, PendingIntentKind::Download);
        assert_eq!(restored.path, local_file);
    }

    #[test]
    fn a_delete_of_a_file_that_is_back_keeps_the_cloud_file_and_queues_an_upload() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("a.txt");
        std::fs::write(fixture.cloud_root.join("a.txt"), b"synced body").expect("seed remote");
        seed_synced_index(&mut fixture, &local_file, b"synced body");
        // The removal was undone before the delete ran.
        std::fs::write(&local_file, b"synced body").expect("file is back");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Delete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("a.txt")).expect("cloud file kept"),
            b"synced body"
        );
        let queued = fixture
            .state_db
            .lease_next_ready(fixture_now())
            .expect("lease")
            .expect("an upload compares the file that came back");
        assert_eq!(queued.kind, PendingIntentKind::Upload);
        assert_eq!(queued.path, local_file);
    }

    #[test]
    fn a_folder_that_is_back_is_not_removed_from_the_cloud() {
        let mut fixture = Fixture::new();
        let dir = fixture.local_root.join("album");
        std::fs::create_dir_all(&dir).expect("local folder is back");
        std::fs::create_dir_all(fixture.cloud_root.join("album")).expect("empty cloud folder");

        let intent = fixture.enqueue_and_lease(&dir, PendingIntentKind::Delete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        assert!(fixture.cloud_root.join("album").is_dir());
        assert_eq!(fixture.state_db.queue_depth().expect("queue depth"), 0);
    }

    #[test]
    fn delete_of_already_missing_remote_completes_as_convergence() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("never-uploaded.txt");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Delete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
    }

    #[test]
    fn local_delete_refused_when_remote_diverged_and_enqueues_download() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("shared.txt");
        // We last synced this path (op-id "op-mine", content "v1").
        fixture
            .state_db
            .set_sync_index(
                &local_file,
                &hash_hex_of_bytes(b"v1"),
                2,
                None,
                None,
                "op-mine",
                timestamp_ms(0),
            )
            .expect("seed index");
        // Another device modified the remote since our last sync.
        std::fs::write(
            fixture.cloud_root.join("shared.txt"),
            b"v2-from-other-device",
        )
        .expect("seed remote");
        // The user deleted the file locally (hence the Delete intent).
        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Delete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        // The newer remote content is preserved, not destroyed.
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("shared.txt")).expect("remote intact"),
            b"v2-from-other-device"
        );
        // A Download was enqueued to restore the newer remote locally.
        let restored = fixture
            .state_db
            .lease_next_ready(fixture_now())
            .expect("lease")
            .expect("download intent enqueued");
        assert_eq!(restored.kind, PendingIntentKind::Download);
        assert_eq!(restored.path, local_file);
    }

    #[test]
    fn two_way_remote_dir_delete_preserves_unsynced_child() {
        let mut fixture = Fixture::new();
        let dir = fixture.local_root.join("project");
        std::fs::create_dir_all(&dir).expect("dir");
        let synced = dir.join("synced.txt");
        let unsynced = dir.join("unsynced.txt");
        std::fs::write(&synced, b"synced body").expect("synced");
        std::fs::write(&unsynced, b"brand new local file").expect("unsynced");
        // Only the synced child has an index entry matching its content.
        let mtime = std::fs::symlink_metadata(&synced)
            .and_then(|m| m.modified())
            .ok();
        fixture
            .state_db
            .set_sync_index(
                &synced,
                &hash_hex_of_bytes(b"synced body"),
                11,
                mtime,
                None,
                "op-synced",
                timestamp_ms(0),
            )
            .expect("seed index");

        let intent = fixture.enqueue_and_lease(&dir, PendingIntentKind::ApplyRemoteDelete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        // The synced-and-unchanged child was removed; the unsynced child
        // (and thus the directory) survived.
        assert!(!synced.exists());
        assert!(unsynced.exists());
        assert!(dir.exists());
        // The preserved child has an Upload re-enqueued to restore it.
        let restore = fixture
            .state_db
            .lease_next_ready(fixture_now())
            .expect("lease")
            .expect("upload intent enqueued");
        assert_eq!(restore.kind, PendingIntentKind::Upload);
        assert_eq!(restore.path, unsynced);
    }

    #[test]
    fn local_directory_delete_expands_into_per_entry_deletes_deepest_first() {
        // A deleted local directory still holds files remotely. The
        // provider never deletes recursively; the planner expands the
        // delete into one guarded Delete per remote entry (files and
        // inner directories before outer ones) and re-enqueues the
        // directory itself last.
        let mut fixture = Fixture::new();
        let dir = fixture.local_root.join("folder");
        let remote_dir = fixture.cloud_root.join("folder");
        std::fs::create_dir_all(remote_dir.join("sub")).expect("remote dirs");
        std::fs::write(remote_dir.join("a.txt"), b"a").expect("a");
        std::fs::write(remote_dir.join("sub/b.txt"), b"b").expect("b");
        // The local directory is gone (the user renamed or removed it).
        assert!(!dir.exists());

        let intent = fixture.enqueue_and_lease(&dir, PendingIntentKind::Delete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);
        assert_eq!(
            report.completed, 1,
            "the directory delete completes as an expansion"
        );

        let mut queued = Vec::new();
        while let Some(next) = fixture
            .state_db
            .lease_next_ready(fixture_now())
            .expect("lease")
        {
            queued.push((next.path.clone(), next.kind));
        }
        let expected: Vec<(PathBuf, PendingIntentKind)> = vec![
            (dir.join("sub/b.txt"), PendingIntentKind::Delete),
            (dir.join("a.txt"), PendingIntentKind::Delete),
            (dir.join("sub"), PendingIntentKind::Delete),
            (dir.clone(), PendingIntentKind::Delete),
        ];
        // Order within one depth is the listing order; assert the depth
        // discipline and the full set rather than the exact interleave.
        assert_eq!(queued.len(), expected.len(), "queued: {queued:?}");
        for entry in &expected {
            assert!(queued.contains(entry), "missing {entry:?} in {queued:?}");
        }
        let position = |path: &PathBuf| queued.iter().position(|(p, _)| p == path).expect("queued");
        assert!(position(&dir.join("sub/b.txt")) < position(&dir.join("sub")));
        assert!(position(&dir.join("sub")) < position(&dir));
        assert!(position(&dir.join("a.txt")) < position(&dir));
        // Nothing was removed remotely by the expansion itself.
        assert!(remote_dir.join("a.txt").exists());
        assert!(remote_dir.join("sub/b.txt").exists());
    }

    #[test]
    fn push_only_delete_of_a_cloud_only_directory_expands_and_removes_it() {
        // The local side is the source of truth. A folder that exists
        // only in the cloud (another device made it there) is a mirror
        // delete of a directory: no provider removes a tree in one
        // call, so the planner expands it the way two-way does and
        // removes the emptied folder last. Sent straight to the
        // provider, a folder that is not empty is refused for ever.
        let mut fixture = Fixture::new();
        fixture.sync_mode = vapor_shared::SyncMode::PushOnly;
        let dir = fixture.local_root.join("folder");
        let remote_dir = fixture.cloud_root.join("folder");
        std::fs::create_dir_all(remote_dir.join("sub")).expect("remote dirs");
        std::fs::write(remote_dir.join("a.txt"), b"a").expect("a");
        std::fs::write(remote_dir.join("sub/b.txt"), b"b").expect("b");
        assert!(!dir.exists());

        let intent = fixture.enqueue_and_lease(&dir, PendingIntentKind::Delete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);
        assert_eq!(
            report.completed, 1,
            "the directory delete completes as an expansion"
        );
        assert!(
            remote_dir.join("a.txt").exists(),
            "the expansion itself removes nothing"
        );

        // Work the expansion in queue order until nothing is left.
        let mut worked = 0u64;
        while let Some(next) = fixture
            .state_db
            .lease_next_ready(fixture_now())
            .expect("lease")
        {
            worked += 1;
            assert!(worked <= 8, "the expansion must converge, not loop");
            fixture
                .executor
                .try_start(&mut fixture.app, next, timestamp_ms(worked));
            let report = fixture.run_to_quiescence(8);
            assert_eq!(report.failed, 0, "no delete may fail on the way down");
        }
        assert!(!remote_dir.exists(), "the cloud-only folder is gone");
        assert_eq!(fixture.state_db.queue_depth().expect("depth"), 0);
    }

    #[test]
    fn conflict_copy_upload_never_moves_the_cloud_canonical() {
        // Keep-both renamed the local loser to a conflict copy and
        // queued a Download to restore the canonical. The copy holds
        // the bytes the index last synced under the canonical name,
        // and that name is gone locally: the exact shape of a rename.
        // It is not one. Moving the cloud canonical under the copy's
        // name would destroy the other side's version before the
        // restore could fetch it.
        let mut fixture = Fixture::new();
        let canonical = fixture.local_root.join("doc.txt");
        fixture
            .state_db
            .set_sync_index(
                &canonical,
                &hash_hex_of_bytes(b"ours"),
                4,
                None,
                None,
                "op-ours",
                timestamp_ms(0),
            )
            .expect("seed index");
        std::fs::write(fixture.cloud_root.join("doc.txt"), b"theirs").expect("cloud edit");
        let conflict =
            crate::conflict::conflict_copy_path(&canonical, "testdev", 1_700_000_000_000, |_| {
                false
            });
        std::fs::write(&conflict, b"ours").expect("conflict copy");
        assert!(!canonical.exists());

        let intent = fixture.enqueue_and_lease(&conflict, PendingIntentKind::Upload);
        fixture
            .state_db
            .enqueue_intent(&canonical, PendingIntentKind::Download, timestamp_ms(0))
            .expect("queue the restore");
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(16);
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);

        assert_eq!(
            std::fs::read(fixture.cloud_root.join("doc.txt")).expect("cloud canonical"),
            b"theirs",
            "the other side's version must stay at the canonical name"
        );
        let remote_copy = fixture
            .cloud_root
            .join(conflict.file_name().expect("conflict name"));
        assert_eq!(
            std::fs::read(&remote_copy).expect("uploaded conflict copy"),
            b"ours",
            "the conflict copy is uploaded, not moved into place"
        );
    }

    #[test]
    fn directory_delete_removes_an_empty_remote_directory_and_keeps_a_preserved_child() {
        let mut fixture = Fixture::new();
        // Empty remote directory: deleted outright.
        let empty = fixture.local_root.join("empty");
        std::fs::create_dir_all(fixture.cloud_root.join("empty")).expect("remote empty dir");
        let intent = fixture.enqueue_and_lease(&empty, PendingIntentKind::Delete);
        fixture
            .executor
            .try_start(&mut fixture.app, intent, timestamp_ms(0));
        let report = fixture.run_to_quiescence(8);
        assert_eq!(report.completed, 1);
        assert!(
            !fixture.cloud_root.join("empty").exists(),
            "empty remote directory removed"
        );

        // A directory whose only child has no queued work left is kept:
        // the child is content the rules preserved.
        let kept = fixture.local_root.join("kept");
        std::fs::create_dir_all(fixture.cloud_root.join("kept")).expect("remote kept dir");
        std::fs::write(fixture.cloud_root.join("kept/child.txt"), b"preserved").expect("child");
        // First pass expands (child delete + directory re-enqueued).
        let intent = fixture.enqueue_and_lease(&kept, PendingIntentKind::Delete);
        fixture
            .executor
            .try_start(&mut fixture.app, intent, timestamp_ms(1));
        fixture.run_to_quiescence(8);
        // The child delete was refused and the child restored locally
        // (what a preservation download does): drop the child delete
        // from the queue and put the file back.
        let child = fixture
            .state_db
            .lease_next_ready(fixture_now())
            .expect("lease")
            .expect("child delete");
        assert_eq!(child.path, kept.join("child.txt"));
        fixture
            .state_db
            .complete_leased(child.id)
            .expect("complete child");
        std::fs::create_dir_all(&kept).expect("local dir");
        std::fs::write(kept.join("child.txt"), b"preserved").expect("restored child");
        // Second pass: the directory delete finds a child that exists
        // locally, has nothing to expand, and keeps the directory.
        let again = fixture
            .state_db
            .lease_next_ready(fixture_now())
            .expect("lease")
            .expect("directory delete re-enqueued");
        assert_eq!(again.path, kept);
        fixture
            .executor
            .try_start(&mut fixture.app, again, timestamp_ms(2));
        let report = fixture.run_to_quiescence(8);
        assert_eq!(report.completed, 1);
        assert!(fixture.cloud_root.join("kept/child.txt").exists());
        assert_eq!(fixture.state_db.queue_depth().expect("depth"), 0);
    }

    #[test]
    fn download_apply_keeps_both_when_local_diverged() {
        let mut fixture = Fixture::new();
        std::fs::write(fixture.cloud_root.join("doc.txt"), b"remote version").expect("remote");
        let local = fixture.local_root.join("doc.txt");
        // A locally-edited file that never matched the incoming payload and
        // has no index (unknown provenance -> keep both).
        std::fs::write(&local, b"local edit").expect("local");

        let intent = fixture.enqueue_and_lease(&local, PendingIntentKind::Download);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(16);

        assert_eq!(report.completed, 1);
        // Canonical payload applied.
        assert_eq!(std::fs::read(&local).expect("applied"), b"remote version");
        // The diverged local edit was kept as a conflict copy, not lost.
        // Skip internal side-files: on a filesystem without xattr support
        // (Windows), the applied payload's op-id tag lands as a
        // `doc.txt.vapor-meta.json` side-file, which also starts with
        // "doc": the conflict copy is the non-internal `doc~conflict-…`.
        let conflict = std::fs::read_dir(&fixture.local_root)
            .expect("read local root")
            .filter_map(|e| e.ok())
            .find(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with("doc")
                    && name != "doc.txt"
                    && !vapor_providers::filesystem::is_internal_file_name(&name)
            });
        let conflict = conflict.expect("a conflict copy was created");
        assert_eq!(
            std::fs::read(conflict.path()).expect("conflict body"),
            b"local edit"
        );
    }

    /// A file synced from an object that no Vapor wrote (no op-id), the
    /// local copy then edited and not uploaded yet.
    fn fixture_with_a_waiting_local_edit(remote_body: &[u8]) -> (Fixture, PathBuf, PathBuf) {
        let mut fixture = Fixture::new();
        let remote = fixture.cloud_root.join("doc.txt");
        let local = fixture.local_root.join("doc.txt");
        std::fs::write(&remote, remote_body).expect("remote");
        std::fs::write(&local, remote_body).expect("local");
        let remote_modified_at = std::fs::metadata(&remote)
            .and_then(|metadata| metadata.modified())
            .expect("remote mtime");
        fixture
            .state_db
            .set_sync_index(
                &local,
                &hash_hex_of_bytes(remote_body),
                remote_body.len() as u64,
                std::fs::metadata(&local)
                    .and_then(|metadata| metadata.modified())
                    .ok(),
                Some(remote_modified_at),
                "",
                timestamp_ms(0),
            )
            .expect("seed index");
        std::fs::write(&local, b"the local edit that waits for its upload").expect("edit");
        (fixture, local, remote)
    }

    fn touch(path: &Path, modified_at: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|file| file.set_modified(modified_at))
            .expect("touch");
    }

    #[test]
    fn a_touch_in_the_cloud_never_turns_a_waiting_local_edit_into_a_conflict() {
        let (mut fixture, local, remote) = fixture_with_a_waiting_local_edit(b"synced bytes");
        // The object is touched: a new time, the same bytes.
        let touched_at = SystemTime::now() + Duration::from_secs(120);
        touch(&remote, touched_at);

        let intent = fixture.enqueue_and_lease(&local, PendingIntentKind::Download);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(16);

        assert_eq!(report.completed, 1);
        assert_eq!(
            std::fs::read(&local).expect("local"),
            b"the local edit that waits for its upload",
            "the edit keeps the name"
        );
        assert!(
            fixture.conflict_copies_of("doc").is_empty(),
            "no conflict copy: only one side changed"
        );
        // The edit goes up, and the index knows the object's new time.
        assert_eq!(
            fixture.state_db.queued_kinds_at(&local).expect("queued"),
            vec![PendingIntentKind::Upload]
        );
        let index = fixture
            .state_db
            .sync_index(&local)
            .expect("index")
            .expect("row");
        assert!(index.matches_remote(b"synced bytes".len() as u64, touched_at));
        assert_eq!(index.content_hash, hash_hex_of_bytes(b"synced bytes"));
    }

    #[test]
    fn a_touch_in_the_cloud_does_not_rewrite_an_untouched_local_file() {
        let mut fixture = Fixture::new();
        let remote = fixture.cloud_root.join("doc.txt");
        let local = fixture.local_root.join("doc.txt");
        std::fs::write(&remote, b"synced bytes").expect("remote");
        std::fs::write(&local, b"synced bytes").expect("local");
        let local_modified_before = std::fs::metadata(&local)
            .and_then(|metadata| metadata.modified())
            .expect("mtime");
        fixture
            .state_db
            .set_sync_index(
                &local,
                &hash_hex_of_bytes(b"synced bytes"),
                b"synced bytes".len() as u64,
                Some(local_modified_before),
                std::fs::metadata(&remote)
                    .and_then(|metadata| metadata.modified())
                    .ok(),
                "op-mine",
                timestamp_ms(0),
            )
            .expect("seed index");
        touch(&remote, SystemTime::now() + Duration::from_secs(120));

        let intent = fixture.enqueue_and_lease(&local, PendingIntentKind::Download);
        fixture
            .executor
            .try_start(&mut fixture.app, intent, timestamp_ms(0));
        let report = fixture.run_to_quiescence(16);

        assert_eq!(report.completed, 1);
        assert_eq!(
            std::fs::metadata(&local)
                .and_then(|metadata| metadata.modified())
                .expect("mtime"),
            local_modified_before,
            "the local file was not written again"
        );
        assert!(
            fixture
                .state_db
                .queued_kinds_at(&local)
                .expect("queued")
                .is_empty(),
            "nothing changed on either side: nothing to upload"
        );
    }

    #[test]
    fn a_same_size_edit_in_the_cloud_still_downloads_and_keeps_both() {
        let (mut fixture, local, remote) = fixture_with_a_waiting_local_edit(b"synced bytes");
        // Another writer changed the object and kept its size.
        std::fs::write(&remote, b"SYNCED BYTES").expect("remote edit");

        let intent = fixture.enqueue_and_lease(&local, PendingIntentKind::Download);
        fixture
            .executor
            .try_start(&mut fixture.app, intent, timestamp_ms(0));
        let report = fixture.run_to_quiescence(16);

        assert_eq!(report.completed, 1);
        assert_eq!(std::fs::read(&local).expect("local"), b"SYNCED BYTES");
        let copies = fixture.conflict_copies_of("doc");
        assert_eq!(copies.len(), 1, "both sides changed: keep both");
        assert_eq!(
            std::fs::read(&copies[0]).expect("copy"),
            b"the local edit that waits for its upload"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_file_from_the_cloud_gets_no_side_file_next_to_it() {
        use std::os::unix::fs::PermissionsExt;
        let mut fixture = Fixture::new();
        // Where the volume takes no extended attribute, the side file
        // is the tag, for every file.
        let probe = fixture.local_root.join("probe");
        std::fs::write(&probe, b"x").expect("probe");
        let has_xattrs = matches!(
            fixture.tags.write_op_id(&probe, "op-probe"),
            Ok(vapor_providers::tags::OpIdTagLocation::Xattr)
        );
        std::fs::remove_file(&probe).expect("probe");
        if !has_xattrs {
            return;
        }
        // Git writes its objects read-only.
        let remote = fixture.cloud_root.join("object");
        std::fs::write(&remote, b"packed").expect("remote");
        std::fs::set_permissions(&remote, std::fs::Permissions::from_mode(0o444)).expect("mode");
        let local = fixture.local_root.join("object");

        let intent = fixture.enqueue_and_lease(&local, PendingIntentKind::Download);
        fixture
            .executor
            .try_start(&mut fixture.app, intent, timestamp_ms(0));
        let report = fixture.run_to_quiescence(16);

        assert_eq!(report.completed, 1);
        assert_eq!(std::fs::read(&local).expect("local"), b"packed");
        assert_eq!(
            std::fs::metadata(&local)
                .expect("stat")
                .permissions()
                .mode()
                & 0o777,
            0o444,
            "the file keeps the mode it came with"
        );
        assert!(
            !OpIdTagStore::side_file_path(&local).exists(),
            "no side file next to a read-only file"
        );
        assert!(
            fixture.tags.read_op_id(&local).is_some(),
            "the file is tagged"
        );
    }

    #[test]
    fn conflict_copy_lookup_excludes_op_id_side_files() {
        // The keep-both path on a filesystem with no xattr. Without xattr
        // support the applied payload's op-id tag lands as a
        // `doc.txt.vapor-meta.json` side-file next to the canonical file,
        // which also starts with "doc" and is not "doc.txt". A conflict-copy
        // search must exclude internal side-files (order-independently) and
        // resolve to the real `doc~conflict-…` copy, else it can read the
        // side-file's JSON instead of the preserved local bytes.
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dir.path();
        std::fs::write(root.join("doc.txt"), b"remote version").expect("canonical");
        std::fs::write(root.join("doc.txt.vapor-meta.json"), br#"{"opId":"x"}"#)
            .expect("side-file");
        std::fs::write(
            root.join("doc~conflict-devA-1750000000000.txt"),
            b"local edit",
        )
        .expect("conflict copy");

        let matches: Vec<_> = std::fs::read_dir(root)
            .expect("read root")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with("doc")
                    && name != "doc.txt"
                    && !vapor_providers::filesystem::is_internal_file_name(&name)
            })
            .collect();

        assert_eq!(
            matches.len(),
            1,
            "exactly the conflict copy must match; the side-file must be excluded: {:?}",
            matches.iter().map(|e| e.file_name()).collect::<Vec<_>>()
        );
        assert_eq!(
            std::fs::read(matches[0].path()).expect("conflict body"),
            b"local edit"
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_download_through_symlinked_parent_is_refused() {
        use std::os::unix::fs::symlink;
        let mut fixture = Fixture::new();
        // A directory outside the sync root, reachable via a symlink inside
        // it: an attacker-controlled cloud path `link/x` must not write
        // there.
        let outside = fixture._temp.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside");
        symlink(&outside, fixture.local_root.join("link")).expect("symlink");

        let target = fixture.local_root.join("link/payload.txt");
        std::fs::write(fixture.cloud_root.join("link/payload.txt"), b"x").ok();
        let intent = fixture.enqueue_and_lease(&target, PendingIntentKind::Download);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.failed, 1);
        // Nothing was written outside the sync root.
        assert!(!outside.join("payload.txt").exists());
    }

    #[test]
    fn download_records_the_remote_objects_op_id_in_the_sync_index() {
        let mut fixture = Fixture::new();
        std::fs::create_dir_all(fixture.cloud_root.join("docs")).expect("dirs");
        let cloud_path = fixture.cloud_root.join("docs/new.txt");
        std::fs::write(&cloud_path, b"from the cloud").expect("seed remote");
        // The remote object was written by another device; tag it so.
        fixture
            .tags
            .write_op_id(&cloud_path, "remote-writer-op")
            .expect("tag remote object");

        let local_target = fixture.local_root.join("docs/new.txt");
        let intent = fixture.enqueue_and_lease(&local_target, PendingIntentKind::Download);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(16);
        assert_eq!(report.completed, 1);

        let index = fixture
            .state_db
            .sync_index(&local_target)
            .expect("read index")
            .expect("index present after download");
        assert_eq!(
            index.last_op_id, "remote-writer-op",
            "the sync index must record the remote writer's op-id, not this device's download id"
        );
    }

    #[test]
    fn download_intent_applies_remote_content_atomically_with_op_id_tag() {
        let mut fixture = Fixture::new();
        std::fs::create_dir_all(fixture.cloud_root.join("docs")).expect("dirs");
        std::fs::write(fixture.cloud_root.join("docs/new.txt"), b"from the cloud")
            .expect("seed remote");
        let local_target = fixture.local_root.join("docs/new.txt");

        let intent = fixture.enqueue_and_lease(&local_target, PendingIntentKind::Download);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(16);

        assert_eq!(report.completed, 1);
        assert_eq!(
            std::fs::read(&local_target).expect("applied payload"),
            b"from the cloud"
        );
        // Op-id tagged for watcher-echo correlation.
        assert!(fixture.tags.read_op_id(&local_target).is_some());
        // Local echo cache carries the write (hash matches content),
        // keyed by the spelling the durable queue handed the executor.
        let stored: PathBuf = local_target.components().collect();
        assert!(fixture.local_echoes.matches_write(
            &stored.to_string_lossy(),
            fixture.tags.read_op_id(&local_target).as_deref(),
            None,
            timestamp_ms(1),
        ));
        assert!(fixture.local_echoes.matches_write(
            &stored.to_string_lossy(),
            None,
            Some(&hash_hex_of_bytes(b"from the cloud")),
            timestamp_ms(1),
        ));
        // No staging temp file survives.
        let staging_leftovers: Vec<_> = std::fs::read_dir(fixture.local_root.join("docs"))
            .expect("read docs")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(constants::provider::TEMP_FILE_PREFIX)
            })
            .collect();
        assert!(staging_leftovers.is_empty());
    }

    #[test]
    fn apply_remote_delete_removes_local_file_and_records_echo() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("gone.txt");
        std::fs::write(&local_file, b"stale").expect("seed local");
        // The file was previously synced: the index matches its current
        // content, so the preservation guard lets the deletion
        // proceed.
        let mtime = std::fs::symlink_metadata(&local_file)
            .and_then(|m| m.modified())
            .ok();
        fixture
            .state_db
            .set_sync_index(
                &local_file,
                &hash_hex_of_bytes(b"stale"),
                5,
                mtime,
                None,
                "op-past",
                timestamp_ms(0),
            )
            .expect("seed sync index");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::ApplyRemoteDelete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        assert!(!local_file.exists());
        assert!(
            fixture
                .local_echoes
                .matches_delete(&local_file.to_string_lossy(), timestamp_ms(1))
        );
    }

    #[test]
    fn a_cloud_deletion_whose_discard_needs_a_copy_is_carried_out_by_a_worker() {
        let mut fixture = Fixture::new();
        let trash_root = fixture.local_root.with_file_name("trash");
        let mut trash = crate::trash::LocalTrash::new(
            "default",
            trash_root,
            crate::trash::TrashSettings::default(),
            Arc::new(vapor_platform::InMemoryTrashBin::unsupported()),
        );
        // The trash is on another volume than the file.
        trash.rename_crosses_devices = true;
        fixture.trash = Some(Arc::new(trash));
        let local = fixture.local_root.join("gone-in-the-cloud.txt");
        std::fs::write(&local, b"synced").expect("local");
        fixture
            .state_db
            .set_sync_index(
                &local,
                &hash_hex_of_bytes(b"synced"),
                6,
                std::fs::metadata(&local).and_then(|m| m.modified()).ok(),
                None,
                "op-past",
                timestamp_ms(0),
            )
            .expect("seed index");

        let intent = fixture.enqueue_and_lease(&local, PendingIntentKind::ApplyRemoteDelete);
        fixture
            .executor
            .try_start(&mut fixture.app, intent, timestamp_ms(0));
        let report = fixture.run_to_quiescence(8);

        assert_eq!((report.completed, report.failed, report.retried), (1, 0, 0));
        assert!(!local.exists(), "the file left its folder");
        let entries = fixture.trash.as_ref().expect("trash").list();
        assert_eq!(entries.len(), 1, "and is whole in the trash");
        assert!(
            fixture
                .state_db
                .sync_index(&local)
                .expect("index")
                .is_none(),
            "its index row goes with it"
        );
    }

    #[test]
    fn a_remote_delete_is_not_applied_when_the_cloud_root_is_gone() {
        // The cloud root vanishes; the feed reports every file under
        // it removed. The probe cannot answer "no object", it fails
        // with the root missing, and the local copy has to stay.
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("kept.txt");
        std::fs::write(&local_file, b"kept").expect("seed local");
        let mtime = std::fs::symlink_metadata(&local_file)
            .and_then(|m| m.modified())
            .ok();
        fixture
            .state_db
            .set_sync_index(
                &local_file,
                &hash_hex_of_bytes(b"kept"),
                4,
                mtime,
                None,
                "op-past",
                timestamp_ms(0),
            )
            .expect("seed sync index");
        std::fs::remove_dir_all(&fixture.cloud_root).expect("cloud root gone");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::ApplyRemoteDelete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 0, "{report:?}");
        assert!(
            local_file.exists(),
            "a missing root is never mirrored as deletions"
        );
        let last_error = fixture
            .state_db
            .list_queue_intents(8)
            .expect("queue")
            .into_iter()
            .find(|record| record.path == local_file)
            .and_then(|record| record.last_error)
            .unwrap_or_default();
        assert!(
            last_error.contains("cannot confirm the cloud deletion"),
            "{last_error}"
        );
    }

    #[test]
    fn upload_over_untagged_remote_matching_the_index_overwrites_without_conflict() {
        // The remote was last written externally (no op-id tag) and we
        // synced *from* it, so the index holds its exact hash. A local
        // edit must upload as a guarded overwrite: the op-id mismatch
        // alone is not divergence. The old behavior manufactured a
        // keep-both copy here on every such upload (and could revert a
        // freshly resolved conflict, the S15 e2e flake).
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("doc.txt");
        std::fs::write(&local_file, b"local edit v2").expect("seed local");
        std::fs::write(fixture.cloud_root.join("doc.txt"), b"external content")
            .expect("seed remote externally");
        let mtime = std::fs::symlink_metadata(&local_file)
            .and_then(|m| m.modified())
            .ok();
        fixture
            .state_db
            .set_sync_index(
                &local_file,
                &hash_hex_of_bytes(b"external content"),
                16,
                mtime,
                None,
                "op-of-the-download",
                timestamp_ms(0),
            )
            .expect("seed sync index");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(32);

        assert_eq!(report.completed, 1);
        assert!(
            report.conflict_paths.is_empty(),
            "no keep-both copy may be created"
        );
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("doc.txt")).expect("remote bytes"),
            b"local edit v2"
        );
        for root in [&fixture.local_root, &fixture.cloud_root] {
            let conflicts: Vec<_> = std::fs::read_dir(root)
                .expect("read root")
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().contains("~conflict-"))
                .collect();
            assert!(
                conflicts.is_empty(),
                "spurious conflict copies: {conflicts:?}"
            );
        }
    }

    #[test]
    fn upload_of_an_untouched_local_over_a_changed_remote_becomes_a_download() {
        // The reconcile walk found the pair diverged, but the local copy
        // still hashes to what the index recorded: the change is
        // remote-only (an offline cloud edit). Uploading would overwrite
        // it; a conflict copy would duplicate an unchanged file. The
        // planner turns the intent into a download.
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("doc.txt");
        std::fs::write(&local_file, b"last synced").expect("seed local");
        std::fs::write(fixture.cloud_root.join("doc.txt"), b"edited in the cloud")
            .expect("seed remote");
        fixture
            .state_db
            .set_sync_index(
                &local_file,
                &hash_hex_of_bytes(b"last synced"),
                11,
                None,
                None,
                "op-old",
                timestamp_ms(0),
            )
            .expect("seed sync index");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(32);
        assert_eq!(report.completed, 1);
        assert!(report.conflict_paths.is_empty());
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("doc.txt")).expect("remote bytes"),
            b"edited in the cloud",
            "the remote edit must survive"
        );
        let queued = fixture.state_db.list_queue_intents(4).expect("queue");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].kind, PendingIntentKind::Download);
        assert_eq!(queued[0].path, local_file);
    }

    #[test]
    fn local_delete_is_refused_when_the_remote_was_edited_in_place_keeping_its_tag() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("doc.txt");
        let cloud_file = fixture.cloud_root.join("doc.txt");
        std::fs::write(&cloud_file, b"last synced").expect("seed remote");
        fixture
            .state_db
            .set_sync_index(
                &local_file,
                &hash_hex_of_bytes(b"last synced"),
                11,
                None,
                Some(timestamp_ms(1_000)),
                "op-ours",
                timestamp_ms(0),
            )
            .expect("seed sync index");
        std::fs::write(&cloud_file, b"cloud  edit").expect("edit remote in place");
        let caps: Arc<dyn vapor_platform::fs_caps::FilesystemCapabilities> =
            Arc::new(vapor_platform::fs_caps::NativeFilesystemCapabilities::for_current_host());
        vapor_providers::tags::OpIdTagStore::new(caps)
            .write_op_id(&cloud_file, "op-ours")
            .expect("tag");

        // The local file is gone; its Delete must not take the tag's word.
        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Delete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(32);
        assert_eq!(report.completed, 1);
        assert_eq!(
            std::fs::read(&cloud_file).expect("remote bytes"),
            b"cloud  edit",
            "the edited remote survives the stale delete"
        );
        let queued = fixture.state_db.list_queue_intents(4).expect("queue");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].kind, PendingIntentKind::Download);
    }

    #[test]
    fn upload_with_a_tag_preserving_in_place_remote_edit_is_not_a_blind_overwrite() {
        // The remote still carries our op-id tag (an in-place write on
        // a filesystem keeps xattrs) but its size-and-mtime moved from
        // what the index recorded. The tag alone would select a
        // guarded overwrite that fails its precondition and falls into
        // keep-both; with the local copy untouched, this is a download.
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("doc.txt");
        std::fs::write(&local_file, b"last synced").expect("seed local");
        let cloud_file = fixture.cloud_root.join("doc.txt");
        std::fs::write(&cloud_file, b"last synced").expect("seed remote");
        let stale_remote_mtime = Some(timestamp_ms(1_000));
        fixture
            .state_db
            .set_sync_index(
                &local_file,
                &hash_hex_of_bytes(b"last synced"),
                11,
                None,
                stale_remote_mtime,
                "op-ours",
                timestamp_ms(0),
            )
            .expect("seed sync index");
        // The remote is rewritten with the same length and keeps the
        // tag the index expects.
        std::fs::write(&cloud_file, b"cloud  edit").expect("edit remote in place");
        let caps: Arc<dyn vapor_platform::fs_caps::FilesystemCapabilities> =
            Arc::new(vapor_platform::fs_caps::NativeFilesystemCapabilities::for_current_host());
        vapor_providers::tags::OpIdTagStore::new(caps)
            .write_op_id(&cloud_file, "op-ours")
            .expect("tag the remote like our own upload would");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(32);
        assert_eq!(report.completed, 1);
        assert!(report.conflict_paths.is_empty());
        assert_eq!(
            std::fs::read(&cloud_file).expect("remote bytes"),
            b"cloud  edit",
            "the remote edit must survive"
        );
        let queued = fixture.state_db.list_queue_intents(4).expect("queue");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].kind, PendingIntentKind::Download);
    }

    #[test]
    fn an_edit_uploads_when_the_index_recorded_no_hash_for_the_last_sync() {
        // What an upload leaves when its provider reported no hash: a
        // row with an empty one. The next edit of the file must still
        // go up. A guard that compares the object's hash with "" fails
        // on every try.
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("big.bin");
        let cloud_file = fixture.cloud_root.join("big.bin");
        std::fs::write(&cloud_file, b"first version").expect("seed remote");
        let caps: Arc<dyn vapor_platform::fs_caps::FilesystemCapabilities> =
            Arc::new(vapor_platform::fs_caps::NativeFilesystemCapabilities::for_current_host());
        vapor_providers::tags::OpIdTagStore::new(caps)
            .write_op_id(&cloud_file, "op-ours")
            .expect("tag the remote like our own upload did");
        fixture
            .state_db
            .set_sync_index(&local_file, "", 13, None, None, "op-ours", timestamp_ms(0))
            .expect("seed a sync index row with no hash");
        std::fs::write(&local_file, b"the second version").expect("local edit");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(32);
        assert_eq!(report.completed, 1, "{report:?}");
        assert_eq!(
            std::fs::read(&cloud_file).expect("remote bytes"),
            b"the second version"
        );
        assert_eq!(
            std::fs::read(&local_file).expect("local bytes"),
            b"the second version",
            "a local edit is not a conflict: the file keeps its name"
        );
        let index = fixture
            .state_db
            .sync_index(&local_file)
            .expect("read")
            .expect("row");
        assert_eq!(index.content_hash, hash_hex_of_bytes(b"the second version"));
    }

    #[cfg(unix)]
    #[test]
    fn upload_of_a_fifo_completes_as_noop_instead_of_wedging() {
        // Special files are outside the sync contract and must be
        // refused at planning: hashing a FIFO blocks until a writer
        // appears, and before this guard the intent sat in the queue
        // forever as a permanently-WaitingForHash row.
        let mut fixture = Fixture::new();
        let fifo = fixture.local_root.join("pipe.fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(status.success(), "mkfifo must succeed");

        let intent = fixture.enqueue_and_lease(&fifo, PendingIntentKind::Upload);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1, "the intent must complete, not wedge");
        assert_eq!(report.failed, 0);
        assert!(
            !fixture.cloud_root.join("pipe.fifo").exists(),
            "no remote object may be created for a special file"
        );
    }

    #[test]
    fn upload_of_vanished_local_file_completes_as_noop() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("ephemeral.txt");
        // Never created on disk: mimics a file deleted between event and
        // lease.

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);

        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        assert!(!fixture.cloud_root.join("ephemeral.txt").exists());
    }

    /// Wraps the filesystem provider and removes the upload source the
    /// moment the transfer opens it: the exact window between planning
    /// and the transfer that a save-then-rename hits.
    struct VanishingSourceProvider {
        inner: FilesystemProvider,
    }

    impl vapor_providers::Provider for VanishingSourceProvider {
        fn name(&self) -> &'static str {
            self.inner.name()
        }
        fn capabilities(&self) -> vapor_providers::ProviderCapabilities {
            self.inner.capabilities()
        }
        fn ensure_cloud_root(&self, dir: &str) -> Result<(), vapor_providers::ProviderError> {
            self.inner.ensure_cloud_root(dir)
        }
        fn enumerate(
            &self,
            directory: &RemotePath,
        ) -> Result<Vec<vapor_providers::RemoteEntry>, vapor_providers::ProviderError> {
            self.inner.enumerate(directory)
        }
        fn stat(
            &self,
            path: &RemotePath,
        ) -> Result<Option<vapor_providers::RemoteEntry>, vapor_providers::ProviderError> {
            self.inner.stat(path)
        }
        fn content_hash(
            &self,
            path: &RemotePath,
        ) -> Result<String, vapor_providers::ProviderError> {
            self.inner.content_hash(path)
        }
        fn begin_upload(
            &self,
            request: vapor_providers::UploadRequest,
        ) -> Result<Box<dyn vapor_providers::TransferSession>, vapor_providers::ProviderError>
        {
            let _ = std::fs::remove_file(&request.local_source);
            self.inner.begin_upload(request)
        }
        fn begin_download(
            &self,
            request: vapor_providers::DownloadRequest,
        ) -> Result<Box<dyn vapor_providers::TransferSession>, vapor_providers::ProviderError>
        {
            self.inner.begin_download(request)
        }
        fn delete(
            &self,
            path: &RemotePath,
            op_id: &str,
        ) -> Result<(), vapor_providers::ProviderError> {
            self.inner.delete(path, op_id)
        }
        fn poll_changes(
            &self,
            cursor: Option<&str>,
            max: usize,
        ) -> Result<vapor_providers::ChangesPoll, vapor_providers::ProviderError> {
            self.inner.poll_changes(cursor, max)
        }
    }

    #[test]
    fn upload_whose_source_vanishes_after_planning_completes_as_noop() {
        // A save followed by a rename: the file exists through planning
        // and hashing and is gone when the transfer opens it. That is a
        // race the watcher has already reported (the rename produced
        // its own intents), never a terminal failure.
        let mut fixture = Fixture::with_provider(|cloud_root| {
            Box::new(VanishingSourceProvider {
                inner: FilesystemProvider::with_root(cloud_root).expect("provider"),
            })
        });
        let local_file = fixture.local_root.join("moving.txt");
        std::fs::write(&local_file, b"about to be renamed").expect("seed");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);
        assert_eq!(
            report.failed, 0,
            "a vanished source must not fail the intent"
        );
        assert_eq!(report.completed, 1);
        assert_eq!(fixture.state_db.failed_depth().expect("failed"), 0);
        assert!(!fixture.cloud_root.join("moving.txt").exists());
    }

    #[test]
    fn download_failure_is_scheduled_for_retry_with_transient_classification() {
        let mut fixture = Fixture::new();
        // Remote object exists at plan time and disappears before the
        // session starts? NotFound completes as noop, so instead break
        // the transfer at its start: the staging file lands next to the
        // target, and a regular file standing where the parent folder
        // should be makes that create fail.
        std::fs::create_dir_all(fixture.cloud_root.join("hole")).expect("dirs");
        std::fs::write(fixture.cloud_root.join("hole/inner.txt"), b"payload").expect("remote");
        std::fs::write(fixture.local_root.join("hole"), b"not a folder").expect("local file");
        let local_target = fixture.local_root.join("hole/inner.txt");

        let intent = fixture.enqueue_and_lease(&local_target, PendingIntentKind::Download);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(8);

        // A destination that cannot be created yields a transient
        // error → retry, not terminal failure.
        assert_eq!(report.failed, 0);
        assert_eq!(report.retried, 1);
        assert_eq!(fixture.state_db.queue_depth().expect("depth"), 1);
    }

    #[test]
    fn a_download_that_cannot_be_applied_leaves_no_staging_file_behind() {
        let mut fixture = Fixture::new();
        // A mirror: the cloud holds a file where this device holds a
        // folder with content, so the payload comes down and cannot
        // take the name. (In two-way the planner sees the clash before
        // any transfer and asks for the walk of the folder.)
        fixture.sync_mode = vapor_shared::SyncMode::PullOnly;
        std::fs::write(fixture.cloud_root.join("thing"), b"a file named thing").expect("remote");
        std::fs::create_dir_all(fixture.local_root.join("thing")).expect("local folder");
        std::fs::write(fixture.local_root.join("thing/inner.txt"), b"x").expect("inner");
        let target = fixture.local_root.join("thing");

        fixture
            .state_db
            .enqueue_intent(&target, PendingIntentKind::Download, timestamp_ms(0))
            .expect("enqueue");
        // Three attempts of the one intent.
        for attempt in 0..3u64 {
            let due = fixture_now() + Duration::from_secs(3_600 * (attempt + 1));
            let intent = fixture
                .state_db
                .lease_next_ready(due)
                .expect("lease")
                .expect("the intent is due again");
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0));
            let report = fixture.run_to_quiescence(8);
            assert_eq!((report.failed, report.retried), (0, 1));
        }

        let left: Vec<String> = std::fs::read_dir(&fixture.local_root)
            .expect("read local root")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(constants::provider::TEMP_FILE_PREFIX))
            .collect();
        assert!(left.is_empty(), "staging files left behind: {left:?}");
    }

    #[test]
    fn same_path_intents_are_serialized() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("contended.txt");
        std::fs::write(&local_file, b"payload").expect("seed");

        fixture.app.apply_throttle_inputs(ThrottleInputs::default());
        let first = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        fixture
            .state_db
            .enqueue_intent(&local_file, PendingIntentKind::Delete, timestamp_ms(0))
            .expect("enqueue second");
        let second = fixture
            .state_db
            .lease_next_ready(fixture_now())
            .expect("lease")
            .expect("second intent");

        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, first, timestamp_ms(0)),
            StartDecision::Started
        );
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, second, timestamp_ms(0)),
            StartDecision::PathBusy,
            "second intent on the same path must not start concurrently"
        );
    }

    #[test]
    fn staged_executor_respects_workgate_caps() {
        let mut fixture = Fixture::new();
        // User activity drives the throttle to `Throttled`: exactly one
        // planner worker is available.
        fixture.app.apply_throttle_inputs(ThrottleInputs {
            user_active: true,
            ..ThrottleInputs::default()
        });

        for index in 0..2 {
            let path = fixture.local_root.join(format!("file-{index}.txt"));
            std::fs::write(&path, b"x").expect("seed");
            fixture
                .state_db
                .enqueue_intent(&path, PendingIntentKind::Upload, timestamp_ms(0))
                .expect("enqueue intent");
        }

        let first = fixture
            .state_db
            .lease_next_ready(fixture_now())
            .expect("lease")
            .expect("first");
        let second = fixture
            .state_db
            .lease_next_ready(fixture_now())
            .expect("lease")
            .expect("second");

        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, first, timestamp_ms(0)),
            StartDecision::Started
        );
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, second, timestamp_ms(0)),
            StartDecision::AtCapacity,
            "throttled state must cap planner admission at one"
        );
        assert_eq!(fixture.executor.snapshot().planner_running, 1);
    }

    #[test]
    fn suspended_throttle_holds_in_flight_transfers_at_slice_checkpoints() {
        let mut fixture = Fixture::new();
        // Cap bandwidth at one transfer step per second: with the manual
        // clock frozen inside an advance, the shaper grants at most one
        // step before the session holds: pinning the transfer mid-flight
        // so the suspension below catches it between checkpoints.
        *fixture.bandwidth.shaper(TransferDirection::Upload) =
            vapor_providers::BandwidthShaper::with_rate(constants::provider::TRANSFER_STEP_BYTES);
        let local_file = fixture.local_root.join("held.bin");
        let payload = vec![7_u8; (constants::provider::TRANSFER_STEP_BYTES * 3) as usize];
        std::fs::write(&local_file, &payload).expect("seed large");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );

        // Drive into the transfer, then suspend.
        let _ = fixture.run_to_quiescence(4);
        assert!(
            fixture.executor.snapshot().active_total == 1,
            "large upload must still be in flight"
        );
        fixture.app.apply_throttle_inputs(ThrottleInputs {
            thermal_pressure: crate::throttle::ThermalPressure::Critical,
            ..ThrottleInputs::default()
        });
        assert_eq!(
            fixture.app.snapshot().throttle_state,
            vapor_shared::ThrottleState::Suspended
        );

        // Suspended: many ticks pass, nothing completes, nothing fails;
        // the transfer holds at its slice checkpoint with progress kept.
        let held = fixture.run_to_quiescence(6);
        assert_eq!(held.completed, 0);
        assert_eq!(held.failed, 0);
        assert_eq!(fixture.executor.snapshot().active_total, 1);

        // Relax the throttle: the held transfer resumes and completes.
        // (Suspended has a 1s min dwell; advance the clock past it.)
        fixture.clock.advance(Duration::from_secs(2));
        fixture.app.apply_throttle_inputs(ThrottleInputs::default());
        let resumed = fixture.run_to_quiescence(16);
        assert_eq!(resumed.completed, 1);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("held.bin"))
                .expect("uploaded")
                .len(),
            payload.len()
        );
    }

    #[test]
    fn hashing_stops_while_sync_is_suspended_and_resumes_after() {
        let mut fixture = Fixture::new();
        let local_file = fixture.local_root.join("large.bin");
        // Larger than one hash step, so the worker checks the throttle
        // at least once between two steps.
        let payload = vec![42_u8; (constants::engine::HASH_STAGE_STEP_BYTES + 1024) as usize];
        std::fs::write(&local_file, &payload).expect("seed large");

        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        // The device is about to overheat before the hash starts.
        fixture.app.apply_throttle_inputs(ThrottleInputs {
            thermal_pressure: crate::throttle::ThermalPressure::Critical,
            ..ThrottleInputs::default()
        });
        let held = fixture.run_to_quiescence(4);
        assert_eq!(held.completed, 0, "nothing is hashed or uploaded");
        assert_eq!(fixture.executor.snapshot().active_total, 1);
        assert!(!fixture.cloud_root.join("large.bin").exists());

        // The dwell of Suspended passes and the pressure is gone.
        fixture.clock.advance(Duration::from_secs(2));
        fixture.app.apply_throttle_inputs(ThrottleInputs::default());
        let report = fixture.run_to_quiescence(32);
        assert_eq!(report.completed, 1);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("large.bin"))
                .expect("uploaded")
                .len(),
            payload.len()
        );
    }

    #[test]
    fn worker_thread_pool_completes_uploads_off_the_tick_thread() {
        // Production mode: provider jobs run on worker threads and the
        // advance loop harvests their outcomes. Bounded by iteration
        // count, not wall time: the loop ends as soon as the worker
        // delivers, and only a genuine deadlock could exhaust it.
        let mut fixture = Fixture::new();
        fixture.executor.enable_worker_threads(None);
        let local_file = fixture.local_root.join("threaded.txt");
        std::fs::write(&local_file, b"threaded payload").expect("seed");

        let reads_before = crate::local_hash::content_reads_on_this_thread();
        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Upload);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );

        let total = fixture.run_on_worker_threads();

        assert_eq!(total.completed, 1);
        assert_eq!(total.failed, 0);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("threaded.txt")).expect("uploaded"),
            b"threaded payload"
        );
        assert_eq!(
            crate::local_hash::content_reads_on_this_thread(),
            reads_before,
            "the tick thread hashed nothing"
        );
    }

    /// A synced local file whose cloud copy is gone, with the deletion
    /// leased and started.
    /// A synced local file, and the cloud object that another device
    /// renamed it to: the download of the new name can be a local move.
    /// Returns the local file and the download target.
    fn start_download_of_a_file_renamed_in_the_cloud(fixture: &mut Fixture) -> (PathBuf, PathBuf) {
        let body = b"the synced body of the file";
        let source = fixture.local_root.join("old-name.txt");
        std::fs::write(&source, body).expect("seed local");
        let cloud_object = fixture.cloud_root.join("new-name.txt");
        std::fs::write(&cloud_object, body).expect("seed cloud");
        let modified_at = |path: &Path| {
            std::fs::symlink_metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
        };
        fixture
            .state_db
            .set_sync_index(
                &source,
                &hash_hex_of_bytes(body),
                body.len() as u64,
                modified_at(&source),
                modified_at(&cloud_object),
                "op-past",
                timestamp_ms(0),
            )
            .expect("seed sync index");
        let target = fixture.local_root.join("new-name.txt");
        let intent = fixture.enqueue_and_lease(&target, PendingIntentKind::Download);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        (source, target)
    }

    /// Whether some file under `root` holds exactly `bytes`.
    fn some_file_holds(root: &Path, bytes: &[u8]) -> bool {
        std::fs::read_dir(root)
            .expect("read root")
            .flatten()
            .any(|entry| std::fs::read(entry.path()).is_ok_and(|found| found == bytes))
    }

    #[test]
    fn a_file_renamed_in_the_cloud_is_moved_locally() {
        let mut fixture = Fixture::new();
        let (source, target) = start_download_of_a_file_renamed_in_the_cloud(&mut fixture);

        let report = fixture.run_to_quiescence(12);

        assert_eq!((report.completed, report.failed), (1, 0));
        assert!(!source.exists(), "the file moved, it was not downloaded");
        assert!(target.is_file());
    }

    #[test]
    fn a_copy_made_in_the_cloud_that_keeps_the_time_is_downloaded_and_the_original_stays() {
        let mut fixture = Fixture::new();
        let body = b"the synced body of the file";
        let original = fixture.local_root.join("original.bin");
        std::fs::write(&original, body).expect("seed local");
        let cloud_original = fixture.cloud_root.join("original.bin");
        std::fs::write(&cloud_original, body).expect("seed cloud");
        let modified_at = |path: &Path| {
            std::fs::symlink_metadata(path)
                .and_then(|metadata| metadata.modified())
                .expect("mtime")
        };
        fixture
            .state_db
            .set_sync_index(
                &original,
                &hash_hex_of_bytes(body),
                body.len() as u64,
                Some(modified_at(&original)),
                Some(modified_at(&cloud_original)),
                "op-past",
                timestamp_ms(0),
            )
            .expect("seed sync index");
        // `cp -p original.bin copy.bin` in the cloud folder: the same
        // bytes, the same time, and the original still there.
        let cloud_copy = fixture.cloud_root.join("copy.bin");
        std::fs::write(&cloud_copy, body).expect("copy");
        touch(&cloud_copy, modified_at(&cloud_original));

        let copy = fixture.local_root.join("copy.bin");
        let intent = fixture.enqueue_and_lease(&copy, PendingIntentKind::Download);
        fixture
            .executor
            .try_start(&mut fixture.app, intent, timestamp_ms(0));
        let report = fixture.run_to_quiescence(16);

        assert_eq!((report.completed, report.failed), (1, 0));
        assert_eq!(std::fs::read(&original).expect("original"), body);
        assert_eq!(std::fs::read(&copy).expect("copy"), body);
        assert!(
            fixture
                .state_db
                .sync_index(&original)
                .expect("index")
                .is_some(),
            "the original keeps its index row"
        );
    }

    /// Wraps the filesystem provider and runs `meddle` while the hash
    /// of a cloud object is read: the window between the choice of a
    /// local file to move and the move itself.
    struct MeddlingProvider {
        inner: FilesystemProvider,
        meddle: Box<dyn Fn() + Send + Sync>,
        /// When set, the read of the cloud root's identity fails so.
        identity_refusal: Arc<std::sync::Mutex<Option<ProviderError>>>,
    }

    impl vapor_providers::Provider for MeddlingProvider {
        fn name(&self) -> &'static str {
            self.inner.name()
        }
        fn capabilities(&self) -> vapor_providers::ProviderCapabilities {
            self.inner.capabilities()
        }
        fn ensure_cloud_root(&self, dir: &str) -> Result<(), vapor_providers::ProviderError> {
            self.inner.ensure_cloud_root(dir)
        }
        fn enumerate(
            &self,
            directory: &RemotePath,
        ) -> Result<Vec<vapor_providers::RemoteEntry>, vapor_providers::ProviderError> {
            self.inner.enumerate(directory)
        }
        fn stat(
            &self,
            path: &RemotePath,
        ) -> Result<Option<vapor_providers::RemoteEntry>, vapor_providers::ProviderError> {
            self.inner.stat(path)
        }
        fn content_hash(
            &self,
            path: &RemotePath,
        ) -> Result<String, vapor_providers::ProviderError> {
            (self.meddle)();
            self.inner.content_hash(path)
        }
        fn root_identity(
            &self,
            cloud_root: &str,
        ) -> Result<Option<String>, vapor_providers::ProviderError> {
            match self.identity_refusal.lock().expect("refusal").clone() {
                Some(error) => Err(error),
                None => self.inner.root_identity(cloud_root),
            }
        }
        fn adopt_root(
            &self,
            cloud_root: &str,
            device_id: &str,
        ) -> Result<Option<String>, vapor_providers::ProviderError> {
            self.inner.adopt_root(cloud_root, device_id)
        }
        fn begin_upload(
            &self,
            request: vapor_providers::UploadRequest,
        ) -> Result<Box<dyn vapor_providers::TransferSession>, vapor_providers::ProviderError>
        {
            self.inner.begin_upload(request)
        }
        fn begin_download(
            &self,
            request: vapor_providers::DownloadRequest,
        ) -> Result<Box<dyn vapor_providers::TransferSession>, vapor_providers::ProviderError>
        {
            self.inner.begin_download(request)
        }
        fn delete(
            &self,
            path: &RemotePath,
            op_id: &str,
        ) -> Result<(), vapor_providers::ProviderError> {
            self.inner.delete(path, op_id)
        }
        fn poll_changes(
            &self,
            cursor: Option<&str>,
            max: usize,
        ) -> Result<vapor_providers::ChangesPoll, vapor_providers::ProviderError> {
            self.inner.poll_changes(cursor, max)
        }
    }

    /// A fixture whose provider writes `bytes` to `name` in the local
    /// root while the hash of a cloud object is read.
    fn fixture_with_a_user_write_during_the_hash(
        name: &'static str,
        bytes: &'static [u8],
    ) -> Fixture {
        let local_root: Arc<std::sync::Mutex<Option<PathBuf>>> = Arc::default();
        let seen = local_root.clone();
        let fixture = Fixture::with_provider(move |cloud_root| {
            Box::new(MeddlingProvider {
                inner: FilesystemProvider::with_root(cloud_root).expect("provider"),
                meddle: Box::new(move || {
                    if let Some(root) = seen.lock().expect("root").as_ref() {
                        std::fs::write(root.join(name), bytes).expect("user write");
                    }
                }),
                identity_refusal: Arc::default(),
            })
        });
        *local_root.lock().expect("root") = Some(fixture.local_root.clone());
        fixture
    }

    #[test]
    fn a_file_that_appears_at_the_target_of_a_local_move_is_never_replaced() {
        let user_bytes = b"written by the user at the target";
        let mut fixture = fixture_with_a_user_write_during_the_hash("new-name.txt", user_bytes);
        let (source, _target) = start_download_of_a_file_renamed_in_the_cloud(&mut fixture);

        let report = fixture.run_to_quiescence(24);

        assert_eq!(report.failed, 0);
        assert!(
            some_file_holds(&fixture.local_root, user_bytes),
            "the move replaced the file the user wrote at its target"
        );
        assert!(source.is_file(), "nothing moved; the download took over");
    }

    #[test]
    fn a_source_that_changes_before_a_local_move_is_not_moved() {
        let edited = b"an edit that never reached the cloud, longer than the body";
        let mut fixture = fixture_with_a_user_write_during_the_hash("old-name.txt", edited);
        let (source, target) = start_download_of_a_file_renamed_in_the_cloud(&mut fixture);

        let report = fixture.run_to_quiescence(24);

        assert_eq!((report.completed, report.failed), (1, 0));
        assert_eq!(std::fs::read(&source).expect("source"), edited);
        // The cloud object came down by itself, and the index records
        // what it holds.
        let body = std::fs::read(&target).expect("target");
        assert_ne!(body, edited);
        let row = fixture
            .state_db
            .sync_index(&target)
            .expect("index")
            .expect("row");
        assert_eq!(row.content_hash, hash_hex_of_bytes(&body));
    }

    fn start_cloud_deletion_of_a_synced_file(fixture: &mut Fixture) -> PathBuf {
        let local_file = fixture.local_root.join("report.txt");
        std::fs::write(&local_file, b"synced body").expect("seed local");
        let modified_at = std::fs::symlink_metadata(&local_file)
            .and_then(|metadata| metadata.modified())
            .ok();
        fixture
            .state_db
            .set_sync_index(
                &local_file,
                &hash_hex_of_bytes(b"synced body"),
                11,
                modified_at,
                None,
                "op-past",
                timestamp_ms(0),
            )
            .expect("seed sync index");
        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::ApplyRemoteDelete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        local_file
    }

    #[test]
    fn a_cloud_deletion_removes_the_local_file_when_the_cloud_root_is_the_adopted_one() {
        for mode in [
            vapor_shared::SyncMode::TwoWay,
            vapor_shared::SyncMode::PullOnly,
        ] {
            let mut fixture = Fixture::new();
            fixture.sync_mode = mode;
            fixture.adopt_cloud_root();
            let local_file = start_cloud_deletion_of_a_synced_file(&mut fixture);

            let report = fixture.run_to_quiescence(8);

            assert_eq!(report.completed, 1, "{mode:?}");
            assert!(!report.cloud_root_suspect, "{mode:?}");
            assert!(!local_file.exists(), "{mode:?}");
        }
    }

    #[test]
    fn a_cloud_deletion_removes_nothing_under_a_cloud_root_that_was_swapped() {
        for mode in [
            vapor_shared::SyncMode::TwoWay,
            vapor_shared::SyncMode::PullOnly,
        ] {
            let mut fixture = Fixture::new();
            fixture.sync_mode = mode;
            fixture.adopt_cloud_root();
            // An empty folder is where the root was, so the cloud copy
            // of every file is "gone".
            fixture.swap_cloud_root();
            let local_file = start_cloud_deletion_of_a_synced_file(&mut fixture);

            let report = fixture.run_to_quiescence(8);

            assert!(local_file.exists(), "{mode:?}: the file stays");
            assert_eq!((report.completed, report.failed), (0, 0), "{mode:?}");
            assert!(report.cloud_root_suspect, "{mode:?}: the runtime is told");
            // The deletion waits in the queue; it did not fail.
            let queued = fixture.state_db.list_queue_intents(4).expect("queue");
            assert_eq!(queued.len(), 1, "{mode:?}: {queued:?}");
            assert_eq!(
                fixture.state_db.failed_depth().expect("failed"),
                0,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_cloud_deletion_removes_nothing_while_the_cloud_root_cannot_be_read() {
        use vapor_shared::ProviderErrorKind;
        let cases = [
            (ProviderError::transient("the network is down"), true),
            (
                ProviderError::new(ProviderErrorKind::NotFound, "no such folder"),
                true,
            ),
            // A rate limit says nothing about the root: the intent
            // takes the retry path of every other provider call.
            (
                ProviderError::new(
                    ProviderErrorKind::RateLimited { retry_after: None },
                    "slow down",
                ),
                false,
            ),
        ];
        for (error, root_is_in_doubt) in cases {
            for mode in [
                vapor_shared::SyncMode::TwoWay,
                vapor_shared::SyncMode::PullOnly,
            ] {
                let refusal: Arc<std::sync::Mutex<Option<ProviderError>>> = Arc::default();
                let handle = refusal.clone();
                let mut fixture = Fixture::with_provider(move |cloud_root| {
                    Box::new(MeddlingProvider {
                        inner: FilesystemProvider::with_root(cloud_root).expect("provider"),
                        meddle: Box::new(|| {}),
                        identity_refusal: refusal,
                    })
                });
                fixture.sync_mode = mode;
                fixture.adopt_cloud_root();
                let local_file = start_cloud_deletion_of_a_synced_file(&mut fixture);
                *handle.lock().expect("refusal") = Some(error.clone());

                let report = fixture.run_to_quiescence(8);

                let case = format!("{mode:?}, {}", error.message);
                assert!(local_file.exists(), "{case}: the file stays");
                assert_eq!((report.completed, report.failed), (0, 0), "{case}");
                assert_eq!(report.cloud_root_suspect, root_is_in_doubt, "{case}");
                assert_eq!(
                    fixture.state_db.list_queue_intents(4).expect("queue").len(),
                    1,
                    "{case}: the deletion waits in the queue"
                );
            }
        }
    }

    #[test]
    fn a_mirror_keeps_local_only_content_under_a_cloud_root_that_was_swapped() {
        for swapped in [false, true] {
            let mut fixture = Fixture::new();
            fixture.sync_mode = vapor_shared::SyncMode::PullOnly;
            fixture.adopt_cloud_root();
            if swapped {
                fixture.swap_cloud_root();
            }
            // A local file with no cloud copy: the restore finds nothing
            // to fetch.
            let local_file = fixture.local_root.join("only-here.txt");
            std::fs::write(&local_file, b"local only").expect("seed local");
            let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::Download);
            assert_eq!(
                fixture
                    .executor
                    .try_start(&mut fixture.app, intent, timestamp_ms(0)),
                StartDecision::Started
            );

            let report = fixture.run_to_quiescence(16);

            assert_eq!(local_file.exists(), swapped, "swapped: {swapped}");
            assert_eq!(report.cloud_root_suspect, swapped);
            assert_eq!(report.mirror_deletes, usize::from(!swapped));
        }
    }

    #[test]
    fn a_cloud_deletion_reads_a_touched_file_on_a_worker_and_removes_it() {
        let mut fixture = Fixture::new();
        fixture.executor.enable_worker_threads(None);
        let local_file = fixture.local_root.join("touched.txt");
        std::fs::write(&local_file, b"synced body").expect("seed local");
        // The index has the size and the content and no modification
        // time: only the content can say that the file is unchanged.
        seed_synced_index(&mut fixture, &local_file, b"synced body");

        let reads_before = crate::local_hash::content_reads_on_this_thread();
        let intent = fixture.enqueue_and_lease(&local_file, PendingIntentKind::ApplyRemoteDelete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_on_worker_threads();

        assert_eq!(report.completed, 1);
        assert!(
            !local_file.exists(),
            "unchanged content: the deletion applies"
        );
        assert_eq!(
            crate::local_hash::content_reads_on_this_thread(),
            reads_before,
            "the tick thread read no file"
        );
    }

    #[test]
    fn a_cloud_folder_deletion_decides_touched_children_by_hashes_read_on_a_worker() {
        let mut fixture = Fixture::new();
        fixture.executor.enable_worker_threads(None);
        let dir = fixture.local_root.join("project");
        std::fs::create_dir_all(dir.join("deep")).expect("dirs");
        let same = dir.join("deep/same.txt");
        let edited = dir.join("edited.txt");
        std::fs::write(&same, b"synced body").expect("same");
        // Other bytes of the size that was synced.
        std::fs::write(&edited, b"edited body").expect("edited");
        seed_synced_index(&mut fixture, &same, b"synced body");
        seed_synced_index(&mut fixture, &edited, b"synced body");

        let reads_before = crate::local_hash::content_reads_on_this_thread();
        let intent = fixture.enqueue_and_lease(&dir, PendingIntentKind::ApplyRemoteDelete);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_on_worker_threads();

        assert_eq!(report.completed, 1);
        assert!(!same.exists(), "the unchanged child went with the folder");
        assert!(!dir.join("deep").exists());
        assert_eq!(
            std::fs::read(&edited).expect("kept"),
            b"edited body",
            "the changed child stays"
        );
        let restore = fixture
            .state_db
            .lease_next_ready(fixture_now())
            .expect("lease")
            .expect("upload intent enqueued");
        assert_eq!(
            (restore.kind, restore.path),
            (PendingIntentKind::Upload, edited)
        );
        assert_eq!(
            crate::local_hash::content_reads_on_this_thread(),
            reads_before,
            "the tick thread read no file"
        );
    }

    #[test]
    fn a_download_over_identical_local_bytes_makes_no_copy_and_reads_them_on_a_worker() {
        let mut fixture = Fixture::new();
        fixture.executor.enable_worker_threads(None);
        std::fs::write(fixture.cloud_root.join("doc.txt"), b"same bytes").expect("remote");
        let local = fixture.local_root.join("doc.txt");
        // No index row: only the content can say that the two are equal.
        std::fs::write(&local, b"same bytes").expect("local");

        let reads_before = crate::local_hash::content_reads_on_this_thread();
        let intent = fixture.enqueue_and_lease(&local, PendingIntentKind::Download);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_on_worker_threads();

        assert_eq!(report.completed, 1);
        assert!(report.conflict_paths.is_empty());
        assert_eq!(std::fs::read(&local).expect("applied"), b"same bytes");
        assert!(fixture.conflict_copies_of("doc").is_empty());
        assert_eq!(
            crate::local_hash::content_reads_on_this_thread(),
            reads_before,
            "the tick thread read no file"
        );
    }

    #[test]
    fn a_download_over_other_local_bytes_of_the_same_size_keeps_both() {
        let mut fixture = Fixture::new();
        std::fs::write(fixture.cloud_root.join("doc.txt"), b"cloud bytes").expect("remote");
        let local = fixture.local_root.join("doc.txt");
        std::fs::write(&local, b"local bytes").expect("local");

        let intent = fixture.enqueue_and_lease(&local, PendingIntentKind::Download);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(16);

        assert_eq!(report.completed, 1);
        assert_eq!(std::fs::read(&local).expect("applied"), b"cloud bytes");
        let copies = fixture.conflict_copies_of("doc");
        assert_eq!(copies.len(), 1);
        assert_eq!(std::fs::read(&copies[0]).expect("copy"), b"local bytes");
    }

    #[test]
    fn an_edit_made_after_the_worker_read_the_displaced_file_is_kept_as_a_copy() {
        let mut fixture = Fixture::new();
        let local = fixture.local_root.join("doc.txt");
        std::fs::write(&local, b"same bytes").expect("local");
        // What the download worker reads once the payload is staged:
        // the local file equals the payload.
        let displaced = DisplacedFileCheck {
            local_path: local.clone(),
            index: None,
            algorithm: vapor_providers::HashAlgorithm::Sha256,
        }
        .run(10)
        .expect("the worker read the file");
        // Before the apply, the user saves other bytes of that size.
        std::fs::write(&local, b"user edit!").expect("edit");
        std::fs::File::options()
            .write(true)
            .open(&local)
            .and_then(|file| file.set_modified(SystemTime::now() + Duration::from_secs(5)))
            .expect("later mtime");
        let staging = fixture.local_root.join(".vapor-tmp-staged-doc");
        std::fs::write(&staging, b"same bytes").expect("staged payload");

        let intent = fixture.enqueue_and_lease(&local, PendingIntentKind::Download);
        let plan = TransferPlan {
            remote_path: RemotePath::new("doc.txt").expect("remote path"),
            op_id: "testdev-op1-a0-t0".to_string(),
            local_path: local.clone(),
            staging_path: Some(staging),
            content_hash: None,
            precondition: RemotePrecondition::None,
            verify_remote_before_upload: false,
            last_synced_hash: None,
            remote_gone_since_sync: false,
            remote_unchanged_since_sync: false,
            hashed_local_state: None,
            remote_op_id: None,
            remote_size: None,
        };
        let mut env = ExecutionEnv {
            local_root: Some(&fixture.local_root),
            cloud_root: None,
            sync_mode: fixture.sync_mode,
            device_id: "testdev",
            profile_id: "default",
            hash_algorithm: vapor_providers::HashAlgorithm::Sha256,
            bandwidth: &fixture.bandwidth,
            move_hints: &mut MoveHints::default(),
            tags: &fixture.tags,
            local_echoes: &mut fixture.local_echoes,
            remote_echoes: &mut fixture.remote_echoes,
            trash: None,
            local_changes_pending: false,
            remote_changes_pending: false,
            path_filter: None,
        };
        let made_copy = apply_downloaded_payload_keep_both(
            &mut env,
            &mut fixture.state_db,
            &intent,
            &plan,
            &hash_hex_of_bytes(b"same bytes"),
            Some(&displaced),
            fixture_now(),
        )
        .expect("apply");

        assert_eq!(
            made_copy,
            DownloadApply::KeptBoth,
            "the hash describes a file that is gone"
        );
        assert_eq!(std::fs::read(&local).expect("applied"), b"same bytes");
        let copies = fixture.conflict_copies_of("doc");
        assert_eq!(copies.len(), 1);
        assert_eq!(std::fs::read(&copies[0]).expect("copy"), b"user edit!");
    }

    /// What a test sees of a [`NotingProvider`]: each write with no
    /// recorded outcome that the engine told it about, as the path,
    /// the base, the content, and the tag; and how many uploads it was
    /// asked to start.
    #[derive(Clone, Default)]
    struct Noted {
        writes: Arc<std::sync::Mutex<Vec<[String; 4]>>>,
        uploads: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Noted {
        fn writes(&self) -> Vec<[String; 4]> {
            self.writes.lock().expect("noted").clone()
        }

        fn uploads(&self) -> usize {
            self.uploads.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// The filesystem provider, keeping what the engine tells it.
    struct NotingProvider {
        inner: FilesystemProvider,
        noted: Noted,
    }

    impl vapor_providers::Provider for NotingProvider {
        fn name(&self) -> &'static str {
            self.inner.name()
        }
        fn capabilities(&self) -> vapor_providers::ProviderCapabilities {
            self.inner.capabilities()
        }
        fn ensure_cloud_root(&self, dir: &str) -> Result<(), vapor_providers::ProviderError> {
            self.inner.ensure_cloud_root(dir)
        }
        fn enumerate(
            &self,
            directory: &RemotePath,
        ) -> Result<Vec<vapor_providers::RemoteEntry>, vapor_providers::ProviderError> {
            self.inner.enumerate(directory)
        }
        fn stat(
            &self,
            path: &RemotePath,
        ) -> Result<Option<vapor_providers::RemoteEntry>, vapor_providers::ProviderError> {
            self.inner.stat(path)
        }
        fn content_hash(
            &self,
            path: &RemotePath,
        ) -> Result<String, vapor_providers::ProviderError> {
            self.inner.content_hash(path)
        }
        fn begin_upload(
            &self,
            request: vapor_providers::UploadRequest,
        ) -> Result<Box<dyn vapor_providers::TransferSession>, vapor_providers::ProviderError>
        {
            self.noted
                .uploads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.begin_upload(request)
        }
        fn begin_download(
            &self,
            request: vapor_providers::DownloadRequest,
        ) -> Result<Box<dyn vapor_providers::TransferSession>, vapor_providers::ProviderError>
        {
            self.inner.begin_download(request)
        }
        fn delete(
            &self,
            path: &RemotePath,
            op_id: &str,
        ) -> Result<(), vapor_providers::ProviderError> {
            self.inner.delete(path, op_id)
        }
        fn poll_changes(
            &self,
            cursor: Option<&str>,
            max: usize,
        ) -> Result<vapor_providers::ChangesPoll, vapor_providers::ProviderError> {
            self.inner.poll_changes(cursor, max)
        }
        fn note_unrecorded_write(
            &self,
            path: &RemotePath,
            base_hash: &str,
            content_hash: &str,
            op_id: &str,
        ) {
            self.noted.writes.lock().expect("noted").push([
                path.as_str().to_string(),
                base_hash.to_string(),
                content_hash.to_string(),
                op_id.to_string(),
            ]);
        }
    }

    /// A tag as this device gives it out for the profile of the
    /// fixture: the one of an upload whose outcome was never stored.
    fn lost_op_id() -> String {
        format!("{}7-a0-t1", own_op_id_prefix("testdev", "default"))
    }

    /// A fixture whose two sides hold `doc.txt` with the same content,
    /// and whose cloud object carries `tag`. With [`lost_op_id`] that
    /// is the state after an upload that landed in a daemon that
    /// stopped before it stored the outcome.
    fn fixture_with_the_same_content_on_both_sides(tag: Option<&str>) -> (Fixture, Noted, PathBuf) {
        let noted = Noted::default();
        let handle = noted.clone();
        let fixture = Fixture::with_provider(move |cloud_root| {
            Box::new(NotingProvider {
                inner: FilesystemProvider::with_root(cloud_root).expect("provider"),
                noted,
            })
        });
        let local = fixture.local_root.join("doc.txt");
        std::fs::write(&local, b"our edit").expect("local");
        let object = fixture.cloud_root.join("doc.txt");
        std::fs::write(&object, b"our edit").expect("remote");
        if let Some(tag) = tag {
            fixture.tags.write_op_id(&object, tag).expect("tag");
        }
        (fixture, handle, local)
    }

    /// Runs one intent of `kind` for `path` to its end.
    fn run_intent(fixture: &mut Fixture, path: &Path, kind: PendingIntentKind) {
        let intent = fixture.enqueue_and_lease(path, kind);
        assert_eq!(
            fixture
                .executor
                .try_start(&mut fixture.app, intent, timestamp_ms(0)),
            StartDecision::Started
        );
        let report = fixture.run_to_quiescence(16);
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
    }

    fn indexed_hash(fixture: &Fixture, path: &Path) -> String {
        fixture
            .state_db
            .sync_index(path)
            .expect("index")
            .expect("an index row")
            .content_hash
    }

    fn indexed_op_id(fixture: &Fixture, path: &Path) -> String {
        fixture
            .state_db
            .sync_index(path)
            .expect("index")
            .expect("an index row")
            .last_op_id
    }

    /// The one call that a test of a lost write expects.
    fn the_lost_write() -> Vec<[String; 4]> {
        vec![[
            "doc.txt".to_string(),
            hash_hex_of_bytes(b"base"),
            hash_hex_of_bytes(b"our edit"),
            lost_op_id(),
        ]]
    }

    #[test]
    fn an_upload_that_finds_its_own_unrecorded_write_in_the_cloud_tells_the_provider() {
        let (mut fixture, noted, local) =
            fixture_with_the_same_content_on_both_sides(Some(&lost_op_id()));
        // The index still holds what was synced before the edit.
        seed_synced_index(&mut fixture, &local, b"base");

        run_intent(&mut fixture, &local, PendingIntentKind::Upload);

        assert_eq!(
            noted.writes(),
            the_lost_write(),
            "the provider gets the path, the base of the index, the content, and the tag"
        );
        assert_eq!(noted.uploads(), 0, "no bytes are sent");
        assert_eq!(
            indexed_hash(&fixture, &local),
            hash_hex_of_bytes(b"our edit"),
            "the file is recorded as in sync"
        );
        assert!(fixture.conflict_copies_of("doc").is_empty());
        assert_eq!(
            indexed_op_id(&fixture, &local),
            lost_op_id(),
            "the row takes the tag that the object carries"
        );

        // The index knows the write now: one more upload intent for
        // the same file tells the provider nothing.
        run_intent(&mut fixture, &local, PendingIntentKind::Upload);
        assert_eq!(noted.writes().len(), 1);
        assert_eq!(noted.uploads(), 0);
    }

    #[test]
    fn a_row_keeps_the_tag_of_its_object_when_no_bytes_are_sent() {
        // The outcome of the upload with this tag was recorded.
        let (mut fixture, noted, local) =
            fixture_with_the_same_content_on_both_sides(Some(&lost_op_id()));
        fixture
            .state_db
            .set_sync_index(
                &local,
                &hash_hex_of_bytes(b"our edit"),
                8,
                None,
                None,
                &lost_op_id(),
                timestamp_ms(0),
            )
            .expect("seed index");

        // The file is saved again with the same bytes: nothing is
        // sent, and the row must not take the op-id of an upload that
        // never ran.
        run_intent(&mut fixture, &local, PendingIntentKind::Upload);
        assert_eq!(noted.uploads(), 0);
        assert_eq!(indexed_op_id(&fixture, &local), lost_op_id());

        // A program that keeps the tag writes other content, and this
        // device takes it. With a row that held another op-id, this
        // would read as a write of this device with no recorded
        // outcome.
        std::fs::write(fixture.cloud_root.join("doc.txt"), b"their edit").expect("remote");
        run_intent(&mut fixture, &local, PendingIntentKind::Download);
        assert!(noted.writes().is_empty(), "{:?}", noted.writes());
        assert_eq!(std::fs::read(&local).expect("applied"), b"their edit");
    }

    #[test]
    fn its_own_unrecorded_write_is_told_when_the_local_file_changed_again() {
        // The user saved the file once more while no daemon ran. The
        // two sides differ, so both are kept, and the write that
        // landed still gets its check: what it replaced unseen is on
        // no device.
        for kind in [PendingIntentKind::Upload, PendingIntentKind::Download] {
            let (mut fixture, noted, local) =
                fixture_with_the_same_content_on_both_sides(Some(&lost_op_id()));
            seed_synced_index(&mut fixture, &local, b"base");
            std::fs::write(&local, b"a later edit").expect("local");

            run_intent(&mut fixture, &local, kind);

            let told = noted.writes();
            assert!(!told.is_empty(), "{kind:?}: the provider is told");
            assert!(
                told.iter().all(|write| the_lost_write().contains(write)),
                "{kind:?}: {told:?}"
            );
            // The later edit stays on the device, in the file or in a
            // copy next to it, and the write that landed is not
            // replaced in the cloud.
            let mut on_the_device: Vec<Vec<u8>> = fixture
                .conflict_copies_of("doc")
                .iter()
                .filter_map(|copy| std::fs::read(copy).ok())
                .collect();
            on_the_device.extend(std::fs::read(&local).ok());
            assert!(
                on_the_device.contains(&b"a later edit".to_vec()),
                "{kind:?}: the later edit is on the device"
            );
            assert_eq!(
                std::fs::read(fixture.cloud_root.join("doc.txt")).expect("the object"),
                b"our edit",
                "{kind:?}: the cloud keeps the write that landed"
            );
        }
    }

    #[test]
    fn content_from_another_writer_tells_the_provider_nothing() {
        // The same bytes came to both sides by another route: a
        // checkout of one branch on two devices. The object carries
        // the tag of the device that uploaded it, the tag of another
        // profile of this device, or none. Nothing of this profile
        // replaced anything, so no check is owed, for an upload and
        // for a download alike.
        let of_another_device = "otherdev-p00000000-op7-a0-t1".to_string();
        let of_another_profile = format!("{}7-a0-t1", own_op_id_prefix("testdev", "second"));
        for tag in [Some(of_another_device), Some(of_another_profile), None] {
            for kind in [PendingIntentKind::Upload, PendingIntentKind::Download] {
                let (mut fixture, noted, local) =
                    fixture_with_the_same_content_on_both_sides(tag.as_deref());
                seed_synced_index(&mut fixture, &local, b"base");
                run_intent(&mut fixture, &local, kind);
                assert!(noted.writes().is_empty(), "{tag:?}: {:?}", noted.writes());
                assert_eq!(noted.uploads(), 0);
                assert_eq!(
                    indexed_hash(&fixture, &local),
                    hash_hex_of_bytes(b"our edit"),
                    "{tag:?}: the file is recorded as in sync"
                );
                assert!(fixture.conflict_copies_of("doc").is_empty());
            }

            // An edit of the other writer that this device takes.
            let (mut fixture, noted, local) =
                fixture_with_the_same_content_on_both_sides(tag.as_deref());
            seed_synced_index(&mut fixture, &local, b"our edit");
            std::fs::write(fixture.cloud_root.join("doc.txt"), b"their edit").expect("remote");
            run_intent(&mut fixture, &local, PendingIntentKind::Download);
            assert!(noted.writes().is_empty(), "{tag:?}: {:?}", noted.writes());
            assert_eq!(std::fs::read(&local).expect("applied"), b"their edit");
        }
    }

    #[test]
    fn its_own_write_in_the_cloud_with_no_base_tells_the_provider_nothing() {
        for kind in [PendingIntentKind::Upload, PendingIntentKind::Download] {
            // No index row: there is no base, and nothing can have
            // been replaced.
            let (mut fixture, noted, local) =
                fixture_with_the_same_content_on_both_sides(Some(&lost_op_id()));
            run_intent(&mut fixture, &local, kind);
            assert!(noted.writes().is_empty(), "{kind:?}");
            assert_eq!(noted.uploads(), 0);
            assert_eq!(
                indexed_hash(&fixture, &local),
                hash_hex_of_bytes(b"our edit")
            );
            assert!(fixture.conflict_copies_of("doc").is_empty());

            // A row that holds no hash is no base either.
            let (mut fixture, noted, local) =
                fixture_with_the_same_content_on_both_sides(Some(&lost_op_id()));
            fixture
                .state_db
                .set_sync_index(&local, "", 4, None, None, "op-mine", timestamp_ms(0))
                .expect("seed index");
            run_intent(&mut fixture, &local, kind);
            assert!(noted.writes().is_empty(), "{kind:?}");
            assert_eq!(noted.uploads(), 0);

            // A row that already holds the content: the index knows it.
            let (mut fixture, noted, local) =
                fixture_with_the_same_content_on_both_sides(Some(&lost_op_id()));
            seed_synced_index(&mut fixture, &local, b"our edit");
            run_intent(&mut fixture, &local, kind);
            assert!(noted.writes().is_empty(), "{kind:?}");
            assert_eq!(noted.uploads(), 0);

            // A row that holds the tag of the object: the outcome of
            // the write with that tag was recorded. The content came
            // later, from a program that keeps the tag when it writes.
            let (mut fixture, noted, local) =
                fixture_with_the_same_content_on_both_sides(Some(&lost_op_id()));
            let base = hash_hex_of_bytes(b"base");
            fixture
                .state_db
                .set_sync_index(&local, &base, 4, None, None, &lost_op_id(), timestamp_ms(0))
                .expect("seed index");
            run_intent(&mut fixture, &local, kind);
            assert!(noted.writes().is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn a_download_of_its_own_unrecorded_write_tells_the_provider() {
        // The change of the cloud reaches the device before the upload
        // runs again: the download finds its payload at the path.
        let (mut fixture, noted, local) =
            fixture_with_the_same_content_on_both_sides(Some(&lost_op_id()));
        seed_synced_index(&mut fixture, &local, b"base");

        run_intent(&mut fixture, &local, PendingIntentKind::Download);

        assert_eq!(noted.writes(), the_lost_write());
        assert_eq!(std::fs::read(&local).expect("applied"), b"our edit");
        assert!(fixture.conflict_copies_of("doc").is_empty());
        assert_eq!(
            indexed_hash(&fixture, &local),
            hash_hex_of_bytes(b"our edit")
        );
    }
}
