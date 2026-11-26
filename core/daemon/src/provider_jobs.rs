//! Off-thread execution of blocking provider I/O.
//!
//! Every provider call the staged executor makes: remote stats and
//! content hashes during planning, `begin_upload`/`begin_download`,
//! transfer-session steps, and remote deletes: is a blocking network
//! round trip on a real cloud provider. Running those inline on the
//! runtime tick thread would time-slice "concurrent" transfers on one
//! thread and stall fs-event draining, debounce, and IPC status behind
//! provider RTT. This pool runs them on worker threads instead: the
//! executor dispatches a job, keeps ticking, and harvests the outcome
//! on a later tick. Workgate permits stay on the tick thread, so the
//! throttle ladder still bounds concurrency; the pool only bounds
//! parallelism of already-permitted work.
//!
//! Transfer jobs loop provider steps continuously on the worker,
//! consulting the shared [`TransferGates`] and the bandwidth shaper
//! between steps. When a gate closes (throttle Suspended) or the
//! shaper runs dry, the session is handed back to the executor at its
//! checkpoint ([`ProviderJobOutcome::TransferHeld`]) and re-dispatched
//! once the gate reopens: the same hold-at-checkpoint discipline the
//! tick-inline executor enforced, minus the tick-thread stalls.
//!
//! The pool has two modes:
//! - **Inline** (default): `dispatch` runs the job synchronously on the
//!   calling thread and the outcome is harvestable immediately. Used by
//!   tests, where deterministic single-threaded ticks are required.
//! - **Threaded** (production): jobs run on lazily-spawned worker
//!   threads, capped at [`constants::engine::PROVIDER_JOB_WORKERS_MAX`].

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vapor_providers::{
    DownloadRequest, Provider, ProviderError, RemoteEntry, RemotePath, TransferOutcome,
    TransferSession, TransferStep, UploadRequest,
};

use crate::clock::Clock;
use crate::runtime::TickWaker;

/// Shared flags the workers consult between transfer steps. Mirrors of
/// the tick-side workgate caps that must stop in-flight work mid-flight
/// (throttle discipline: under Suspended, transfers hold at their slice
/// checkpoint). Updated by the executor at the start of every advance.
pub(crate) struct TransferGates {
    allow_uploads: AtomicBool,
    allow_downloads: AtomicBool,
    allow_hashing: AtomicBool,
    /// Bumped on abort/shutdown; jobs dispatched under an older
    /// generation abort their session and report `Cancelled`.
    generation: AtomicU64,
}

impl TransferGates {
    fn new() -> Self {
        Self {
            allow_uploads: AtomicBool::new(true),
            allow_downloads: AtomicBool::new(true),
            allow_hashing: AtomicBool::new(true),
            generation: AtomicU64::new(0),
        }
    }
}

/// Everything a job needs from the dispatching tick, captured by value
/// so the worker never touches tick-thread state. The provider is the
/// app's `Arc` (swappable in tests); the bandwidth state is the
/// daemon-wide shared instance, so a rate change reaches an in-flight
/// transfer on its next step and every step feeds one estimate.
pub(crate) struct JobContext {
    pub provider: Arc<dyn Provider>,
    pub bandwidth: Arc<crate::bandwidth::TransferBandwidth>,
    pub clock: Arc<dyn Clock>,
    pub root_identity: Arc<RootIdentityReads>,
}

/// The reads of the cloud root's identity that the workers of one pool
/// make, shared. Each deletion from the cloud needs the identity read
/// after the call that found its object gone. A burst of a thousand
/// deletions does not need a thousand reads for that: one read that
/// started after a worker's own call returned says the same thing to
/// that worker. On Google Drive a read is one request for each part of
/// the cloud root's path.
#[derive(Default)]
pub(crate) struct RootIdentityReads {
    last: Mutex<Option<SharedIdentityRead>>,
}

struct SharedIdentityRead {
    cloud_root: String,
    started: std::time::Instant,
    answer: CloudRootIdentity,
}

impl RootIdentityReads {
    /// The identity of `cloud_root`, read no earlier than `not_before`
    /// (the moment the caller learned that its object is gone). A read
    /// that another worker started after that moment is reused; else
    /// this worker reads, while the others that need one wait for it.
    /// A read that failed is never reused.
    pub fn read_after(
        &self,
        provider: &dyn Provider,
        cloud_root: &str,
        not_before: std::time::Instant,
    ) -> CloudRootIdentity {
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(read) = last.as_ref()
            && read.cloud_root == cloud_root
            && read.started >= not_before
            && read.answer.is_ok()
        {
            return read.answer.clone();
        }
        let started = std::time::Instant::now();
        let answer = provider.root_identity(cloud_root);
        *last = Some(SharedIdentityRead {
            cloud_root: cloud_root.to_string(),
            started,
            answer: answer.clone(),
        });
        answer
    }
}

pub(crate) use crate::bandwidth::TransferDirection;

/// Which phase of a transfer job an error came from. The executor's
/// failure handling differs: a failed `begin_download` on an absent
/// remote triggers the pull-only strict-mirror path, while a mid-step
/// NotFound merely completes as convergence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransferPhase {
    Begin,
    Step,
}

/// When a probe should also fetch the remote content hash (a full
/// remote read on filesystem-backed providers, only fetched when the
/// planner's decision actually needs it).
pub(crate) enum ProbeHash {
    Never,
    /// Unconditional hash fetch (upload-gate verification).
    Always,
    /// Fetch only when the stat found an entry whose op-id differs from
    /// the sync index's and which carries no hash of its own: the
    /// divergence check would otherwise have nothing to compare.
    IfDivergedFrom {
        index_op_id: String,
    },
}

pub(crate) struct ProbeRequest {
    pub remote_path: RemotePath,
    pub want_stat: bool,
    pub hash: ProbeHash,
    /// When the stat reports a directory, enumerate its whole subtree
    /// (files and directories, depth first) so a directory delete can be
    /// expanded into per-entry intents without provider I/O on the tick
    /// thread.
    pub want_subtree_listing: bool,
    /// Local files to read when the stat finds no object: a cloud
    /// deletion applies to a local file only while the file holds the
    /// bytes last synced, and the tick thread reads no file.
    pub local_hashes_if_absent: Option<crate::local_hash::TouchedFileCheck>,
    /// A second object to stat: the name a cloud-side move would have
    /// left. A new object that has a synced twin is a move only when
    /// the twin's own object is gone; with both there, it is a copy.
    pub stat_also: Option<RemotePath>,
    /// The cloud root to read the identity of, after the stat and only
    /// when the stat found no object (or none was asked for): "no
    /// object" is a deletion only in the folder the profile adopted. A
    /// root that was swapped holds none of the files.
    pub cloud_root_identity_of: Option<String>,
}

/// What a provider answered for the identity of the cloud root.
pub(crate) type CloudRootIdentity = Result<Option<String>, ProviderError>;

pub(crate) struct ProbeResult {
    /// `None` when the probe did not request a stat.
    pub stat: Option<Result<Option<RemoteEntry>, ProviderError>>,
    /// `Some` only when a hash fetch was attempted.
    pub content_hash: Option<Result<String, ProviderError>>,
    /// The stat of `stat_also`, when the request named one.
    pub also_stat: Option<Result<Option<RemoteEntry>, ProviderError>>,
    /// `Some` only when a subtree listing was requested and the stat
    /// reported a directory. Entries are ordered parents before
    /// children; the listing is capped at [`SUBTREE_LISTING_CAP`].
    pub subtree: Option<Result<Vec<RemoteEntry>, ProviderError>>,
    /// What `local_hashes_if_absent` asked for; empty otherwise.
    pub local_hashes: crate::local_hash::LocalHashes,
    /// `Some` when `cloud_root_identity_of` asked and the identity was
    /// read.
    pub cloud_root_identity: Option<CloudRootIdentity>,
}

/// Upper bound on entries one subtree probe collects. A directory
/// delete larger than this is expanded in slices: each expansion
/// re-plans once the first slice has drained.
pub(crate) const SUBTREE_LISTING_CAP: usize = 10_000;

impl ProbeResult {
    /// The fetched hash, folded to `None` on fetch failure: matching
    /// the planner's best-effort `.ok()` semantics.
    pub fn content_hash_ok(&self) -> Option<String> {
        self.content_hash
            .as_ref()
            .and_then(|result| result.as_ref().ok().cloned())
    }
}

pub(crate) enum ProviderJobKind {
    Probe(ProbeRequest),
    RemoteDelete {
        remote_path: RemotePath,
        op_id: String,
    },
    /// A server-side move: the detected rename of a synced file.
    Move {
        from: RemotePath,
        to: RemotePath,
        op_id: String,
        /// `Some` when the caller acts on a source that is not there:
        /// the worker then reads the identity of this cloud root, so
        /// that "no such object" from a root that was swapped or
        /// unmounted is not taken for the word of the adopted one.
        cloud_root_if_missing: Option<String>,
    },
    /// `begin_upload` + step loop.
    Upload(UploadRequest),
    /// `begin_download` + step loop. When the download completes, the
    /// worker also reads the local file it displaces, if `displaced`
    /// asks for that.
    Download {
        request: DownloadRequest,
        displaced: Option<crate::local_hash::DisplacedFileCheck>,
        /// The cloud root to read the identity of when the object is
        /// not there. A mirror removes the local file then, and "not
        /// there" must be said by the folder the profile adopted.
        cloud_root_if_missing: Option<String>,
    },
    /// Re-dispatched held session.
    ResumeTransfer {
        session: Box<dyn TransferSession>,
        direction: TransferDirection,
        displaced: Option<crate::local_hash::DisplacedFileCheck>,
    },
    /// The content hash of a local file before its upload: disk reads
    /// and digest work that must not run on the tick thread.
    Hash(crate::executor::StreamingFileHash),
    /// Local disk work that reads file content and belongs on no tick:
    /// the copy of a discarded file to a trash on another volume.
    LocalWork(Box<dyn FnOnce() -> Result<(), String> + Send>),
}

pub(crate) enum ProviderJobOutcome {
    /// Boxed: by far the largest outcome.
    Probe(Box<ProbeResult>),
    RemoteDelete(Result<(), ProviderError>),
    Move(Result<(), ProviderError>),
    /// Session held at its checkpoint: a gate closed or the bandwidth
    /// bucket ran dry. The executor re-dispatches on a later tick (its
    /// stage already knows the direction). A dry bucket says how long
    /// until a grant is worth a step, and the executor waits that long;
    /// dispatching sooner would spin the tick on a few bytes a pass.
    TransferHeld {
        session: Box<dyn TransferSession>,
        resume_after: Option<Duration>,
    },
    TransferCompleted {
        outcome: TransferOutcome,
        /// The hash of the local file a download displaces, when its
        /// job asked for it and only the content could say whether the
        /// file equals the payload or the bytes last synced.
        displaced: Option<crate::local_hash::LocalFileHash>,
    },
    /// The local file is hashed (or could not be read).
    Hashed(std::io::Result<String>),
    /// What a [`ProviderJobKind::LocalWork`] job answered.
    LocalWork(Result<(), String>),
    /// Hashing stopped at a step boundary because the throttle no
    /// longer allows it; the executor dispatches it again when it does.
    HashHeld(crate::executor::StreamingFileHash),
    /// A download found no object to fetch, and its job asked for the
    /// identity of the cloud root in that case.
    DownloadSourceMissing {
        cloud_root_identity: CloudRootIdentity,
    },
    /// A move found no object under its old name, and its job asked
    /// for the identity of the cloud root in that case.
    MoveSourceMissing {
        error: ProviderError,
        cloud_root_identity: CloudRootIdentity,
    },
    /// The session (or its begin call) failed; any session was already
    /// aborted worker-side.
    TransferFailed {
        error: ProviderError,
        phase: TransferPhase,
    },
    /// The job's generation was invalidated by `abort_in_flight`; any
    /// session was aborted worker-side. Harvest drops these.
    Cancelled,
    /// The provider call panicked on the worker. The worker survives and
    /// the executor fails the intent with the panic message; without
    /// this the permit and the in-flight slot would leak for the rest of
    /// the process.
    Panicked(String),
}

/// How one-off provider calls outside the executor (changes poll,
/// reconcile enumerate, cloud-root retry) run: inline for deterministic
/// tests, on a thread in production so a network round trip never holds
/// the tick loop.
#[derive(Clone, Default)]
pub(crate) enum ProviderCallMode {
    #[default]
    Inline,
    Threaded {
        waker: Option<Arc<TickWaker>>,
    },
}

/// One provider call polled by the tick loop until its result is in.
pub(crate) struct ProviderCall<T> {
    state: ProviderCallState<T>,
}

enum ProviderCallState<T> {
    Ready(Option<Result<T, String>>),
    Pending(Receiver<Result<T, String>>),
}

impl<T: Send + 'static> ProviderCall<T> {
    pub fn start(
        mode: &ProviderCallMode,
        name: &'static str,
        call: impl FnOnce() -> T + Send + 'static,
    ) -> Self {
        match mode {
            ProviderCallMode::Inline => Self {
                state: ProviderCallState::Ready(Some(Ok(call()))),
            },
            ProviderCallMode::Threaded { waker } => {
                let (tx, rx) = mpsc::channel();
                let waker = waker.clone();
                let spawned = std::thread::Builder::new()
                    .name(format!("vapor-provider-{name}"))
                    .spawn(move || {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(call))
                            .map_err(|payload| panic_message(payload.as_ref()));
                        let _ = tx.send(result);
                        if let Some(waker) = waker {
                            waker.notify();
                        }
                    });
                match spawned {
                    Ok(_) => Self {
                        state: ProviderCallState::Pending(rx),
                    },
                    Err(error) => Self {
                        state: ProviderCallState::Ready(Some(Err(format!(
                            "cannot spawn the {name} thread: {error}"
                        )))),
                    },
                }
            }
        }
    }

    /// `Some` once the call has finished (`Err` carries a panic
    /// message). Returns the result exactly once.
    pub fn take(&mut self) -> Option<Result<T, String>> {
        match &mut self.state {
            ProviderCallState::Ready(slot) => slot.take(),
            ProviderCallState::Pending(rx) => match rx.try_recv() {
                Ok(result) => {
                    self.state = ProviderCallState::Ready(None);
                    Some(result)
                }
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.state = ProviderCallState::Ready(None);
                    Some(Err(
                        "provider call thread exited without a result".to_string()
                    ))
                }
            },
        }
    }
}

pub(crate) struct CompletedJob {
    pub intent_id: i64,
    pub outcome: ProviderJobOutcome,
    /// Generation the job was dispatched under; harvest drops outcomes
    /// from before the last `abort_in_flight` so a re-leased intent
    /// (durable ids survive an abort) can never receive a stale result.
    generation: u64,
}

struct QueuedJob {
    intent_id: i64,
    generation: u64,
    context: JobContext,
    kind: ProviderJobKind,
}

enum PoolMode {
    Inline {
        pending: VecDeque<CompletedJob>,
    },
    Threaded {
        job_tx: Sender<QueuedJob>,
        job_rx: Arc<Mutex<Receiver<QueuedJob>>>,
        results_tx: Sender<CompletedJob>,
        results_rx: Receiver<CompletedJob>,
        workers: Vec<std::thread::JoinHandle<()>>,
        max_workers: usize,
        waker: Option<Arc<TickWaker>>,
        in_flight: usize,
    },
}

pub(crate) struct ProviderJobPool {
    gates: Arc<TransferGates>,
    mode: PoolMode,
}

impl ProviderJobPool {
    /// Deterministic pool: jobs run synchronously at dispatch on the
    /// calling thread.
    pub fn inline() -> Self {
        Self {
            gates: Arc::new(TransferGates::new()),
            mode: PoolMode::Inline {
                pending: VecDeque::new(),
            },
        }
    }

    /// Production pool: jobs run on up to `max_workers` lazily-spawned
    /// threads. `waker` (when given) is notified after every completed
    /// job so the tick loop harvests promptly instead of sleeping out
    /// its idle interval.
    pub fn threaded(max_workers: usize, waker: Option<Arc<TickWaker>>) -> Self {
        let (job_tx, job_rx) = channel();
        let (results_tx, results_rx) = channel();
        Self {
            gates: Arc::new(TransferGates::new()),
            mode: PoolMode::Threaded {
                job_tx,
                job_rx: Arc::new(Mutex::new(job_rx)),
                results_tx,
                results_rx,
                workers: Vec::new(),
                max_workers: max_workers.max(1),
                waker,
                in_flight: 0,
            },
        }
    }

    /// Pushes the current workgate caps to the shared gates so in-flight
    /// transfer and hash loops observe throttle transitions between
    /// steps.
    pub fn update_gates(&self, allow_uploads: bool, allow_downloads: bool, allow_hashing: bool) {
        self.gates
            .allow_uploads
            .store(allow_uploads, Ordering::Relaxed);
        self.gates
            .allow_downloads
            .store(allow_downloads, Ordering::Relaxed);
        self.gates
            .allow_hashing
            .store(allow_hashing, Ordering::Relaxed);
    }

    /// Invalidates every dispatched-but-unharvested job: their sessions
    /// abort worker-side and their outcomes harvest as `Cancelled`.
    pub fn abort_in_flight(&self) {
        self.gates.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Jobs dispatched and not yet harvested.
    #[cfg(test)]
    pub fn in_flight(&self) -> usize {
        match &self.mode {
            PoolMode::Inline { pending } => pending.len(),
            PoolMode::Threaded { in_flight, .. } => *in_flight,
        }
    }

    pub fn dispatch(&mut self, intent_id: i64, context: JobContext, kind: ProviderJobKind) {
        let generation = self.gates.generation.load(Ordering::SeqCst);
        let job = QueuedJob {
            intent_id,
            generation,
            context,
            kind,
        };
        match &mut self.mode {
            PoolMode::Inline { pending } => {
                let outcome = run_job(job, &self.gates);
                pending.push_back(outcome);
            }
            PoolMode::Threaded {
                job_tx,
                job_rx,
                results_tx,
                workers,
                max_workers,
                waker,
                in_flight,
                ..
            } => {
                *in_flight += 1;
                if job_tx.send(job).is_err() {
                    // Worker channel torn down (only on drop); nothing to do.
                    *in_flight -= 1;
                    return;
                }
                if *in_flight > workers.len() && workers.len() < *max_workers {
                    let job_rx = job_rx.clone();
                    let results_tx = results_tx.clone();
                    let gates = self.gates.clone();
                    let waker = waker.clone();
                    workers.push(std::thread::spawn(move || {
                        worker_main(job_rx, results_tx, gates, waker);
                    }));
                }
            }
        }
    }

    /// Drains every outcome that is ready right now (never blocks).
    /// Outcomes dispatched before the last `abort_in_flight` are
    /// dropped here (their sessions aborted) instead of surfacing.
    pub fn harvest(&mut self) -> Vec<CompletedJob> {
        let current = self.gates.generation.load(Ordering::SeqCst);
        let drained: Vec<CompletedJob> = match &mut self.mode {
            PoolMode::Inline { pending } => pending.drain(..).collect(),
            PoolMode::Threaded {
                results_rx,
                in_flight,
                ..
            } => {
                let mut completed = Vec::new();
                while let Ok(result) = results_rx.try_recv() {
                    *in_flight = in_flight.saturating_sub(1);
                    completed.push(result);
                }
                completed
            }
        };
        let mut live = Vec::with_capacity(drained.len());
        for job in drained {
            if job.generation == current {
                live.push(job);
            } else if let ProviderJobOutcome::TransferHeld { mut session, .. } = job.outcome {
                session.abort();
            }
        }
        live
    }
}

impl Drop for ProviderJobPool {
    fn drop(&mut self) {
        self.abort_in_flight();
        if let PoolMode::Threaded {
            job_tx, workers, ..
        } = &mut self.mode
        {
            // Replace the sender so the channel disconnects and idle
            // workers exit their recv loop.
            let (dead_tx, _) = channel();
            *job_tx = dead_tx;
            for worker in workers.drain(..) {
                let _ = worker.join();
            }
        }
    }
}

fn worker_main(
    job_rx: Arc<Mutex<Receiver<QueuedJob>>>,
    results_tx: Sender<CompletedJob>,
    gates: Arc<TransferGates>,
    waker: Option<Arc<TickWaker>>,
) {
    loop {
        let job = {
            let receiver = job_rx
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            receiver.recv()
        };
        let Ok(job) = job else {
            return;
        };
        let (intent_id, generation) = (job.intent_id, job.generation);
        let completed =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_job(job, &gates))) {
                Ok(completed) => completed,
                Err(payload) => CompletedJob {
                    intent_id,
                    outcome: ProviderJobOutcome::Panicked(panic_message(payload.as_ref())),
                    generation,
                },
            };
        if results_tx.send(completed).is_err() {
            return;
        }
        if let Some(waker) = &waker {
            waker.notify();
        }
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

fn run_job(job: QueuedJob, gates: &TransferGates) -> CompletedJob {
    let QueuedJob {
        intent_id,
        generation,
        context,
        kind,
    } = job;
    if gates.generation.load(Ordering::SeqCst) != generation {
        // The dispatching execution was aborted while this job waited in
        // the queue: abort any session and report a droppable outcome.
        if let ProviderJobKind::ResumeTransfer { mut session, .. } = kind {
            session.abort();
        }
        return CompletedJob {
            intent_id,
            outcome: ProviderJobOutcome::Cancelled,
            generation,
        };
    }
    let outcome = match kind {
        ProviderJobKind::Probe(request) => {
            let may_hash = || {
                gates.generation.load(Ordering::SeqCst) == generation
                    && gates.allow_hashing.load(Ordering::Relaxed)
            };
            ProviderJobOutcome::Probe(Box::new(run_probe(&context, request, &may_hash)))
        }
        ProviderJobKind::RemoteDelete { remote_path, op_id } => {
            ProviderJobOutcome::RemoteDelete(context.provider.delete(&remote_path, &op_id))
        }
        ProviderJobKind::Move {
            from,
            to,
            op_id,
            cloud_root_if_missing,
        } => match (
            context.provider.move_object(&from, &to, &op_id),
            cloud_root_if_missing,
        ) {
            (Err(error), Some(cloud_root))
                if error.kind == vapor_shared::ProviderErrorKind::NotFound =>
            {
                ProviderJobOutcome::MoveSourceMissing {
                    error,
                    cloud_root_identity: context.root_identity.read_after(
                        context.provider.as_ref(),
                        &cloud_root,
                        std::time::Instant::now(),
                    ),
                }
            }
            (result, _) => ProviderJobOutcome::Move(result),
        },
        ProviderJobKind::Hash(mut hasher) => loop {
            if gates.generation.load(Ordering::SeqCst) != generation {
                break ProviderJobOutcome::Cancelled;
            }
            if !gates.allow_hashing.load(Ordering::Relaxed) {
                break ProviderJobOutcome::HashHeld(hasher);
            }
            match hasher.step(vapor_shared::constants::engine::HASH_STAGE_STEP_BYTES) {
                Ok(Some(content_hash)) => break ProviderJobOutcome::Hashed(Ok(content_hash)),
                Ok(None) => {}
                Err(error) => break ProviderJobOutcome::Hashed(Err(error)),
            }
        },
        ProviderJobKind::LocalWork(work) => ProviderJobOutcome::LocalWork(work()),
        ProviderJobKind::Upload(request) => match context.provider.begin_upload(request) {
            Ok(session) => run_transfer(
                &context,
                gates,
                generation,
                session,
                TransferDirection::Upload,
            ),
            Err(error) => ProviderJobOutcome::TransferFailed {
                error,
                phase: TransferPhase::Begin,
            },
        },
        ProviderJobKind::Download {
            request,
            displaced,
            cloud_root_if_missing,
        } => match context.provider.begin_download(request) {
            Ok(session) => with_displaced_hash(
                run_transfer(
                    &context,
                    gates,
                    generation,
                    session,
                    TransferDirection::Download,
                ),
                displaced,
            ),
            Err(error) => match cloud_root_if_missing {
                Some(cloud_root) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
                    ProviderJobOutcome::DownloadSourceMissing {
                        cloud_root_identity: context.root_identity.read_after(
                            context.provider.as_ref(),
                            &cloud_root,
                            std::time::Instant::now(),
                        ),
                    }
                }
                _ => ProviderJobOutcome::TransferFailed {
                    error,
                    phase: TransferPhase::Begin,
                },
            },
        },
        ProviderJobKind::ResumeTransfer {
            session,
            direction,
            displaced,
        } => with_displaced_hash(
            run_transfer(&context, gates, generation, session, direction),
            displaced,
        ),
    };
    CompletedJob {
        intent_id,
        outcome,
        generation,
    }
}

/// Reads the local file a completed download displaces, so the apply
/// on the tick thread compares bytes it never reads.
fn with_displaced_hash(
    outcome: ProviderJobOutcome,
    check: Option<crate::local_hash::DisplacedFileCheck>,
) -> ProviderJobOutcome {
    match (outcome, check) {
        (ProviderJobOutcome::TransferCompleted { outcome, .. }, Some(check)) => {
            let displaced = check.run(outcome.bytes_total);
            ProviderJobOutcome::TransferCompleted { outcome, displaced }
        }
        (outcome, _) => outcome,
    }
}

fn run_probe(
    context: &JobContext,
    request: ProbeRequest,
    may_hash: &dyn Fn() -> bool,
) -> ProbeResult {
    let stat = request
        .want_stat
        .then(|| context.provider.stat(&request.remote_path));
    let stat_done = std::time::Instant::now();
    let fetch_hash = match &request.hash {
        ProbeHash::Never => false,
        ProbeHash::Always => true,
        ProbeHash::IfDivergedFrom { index_op_id } => match &stat {
            Some(Ok(Some(entry))) => {
                entry.op_id.as_deref() != Some(index_op_id.as_str()) && entry.content_hash.is_none()
            }
            _ => false,
        },
    };
    let content_hash = fetch_hash.then(|| context.provider.content_hash(&request.remote_path));
    let also_stat = request
        .stat_also
        .as_ref()
        .map(|path| context.provider.stat(path));
    let subtree = match (&stat, request.want_subtree_listing) {
        (Some(Ok(Some(entry))), true)
            if entry.kind == vapor_providers::RemoteEntryKind::Directory =>
        {
            Some(list_subtree(
                context.provider.as_ref(),
                &request.remote_path,
            ))
        }
        _ => None,
    };
    // A folder under the name is "no object" too for a caller that
    // asks about a file it synced: the file is gone, and a folder has
    // its name.
    let file_is_gone = matches!(&stat, Some(Ok(None)))
        || (request.local_hashes_if_absent.is_some()
            && matches!(&stat, Some(Ok(Some(entry)))
                if entry.kind == vapor_providers::RemoteEntryKind::Directory));
    let local_hashes = match &request.local_hashes_if_absent {
        Some(check) if file_is_gone => check.run(may_hash),
        _ => crate::local_hash::LocalHashes::default(),
    };
    // After the stat: a root swapped between the two calls is caught,
    // which the other order would miss.
    let cloud_root_identity = match &request.cloud_root_identity_of {
        Some(cloud_root) if stat.is_none() || file_is_gone => Some(
            context
                .root_identity
                .read_after(context.provider.as_ref(), cloud_root, stat_done),
        ),
        _ => None,
    };
    ProbeResult {
        stat,
        content_hash,
        also_stat,
        subtree,
        local_hashes,
        cloud_root_identity,
    }
}

/// Lists every entry under `root`, parents before children, stopping
/// at [`SUBTREE_LISTING_CAP`] entries.
fn list_subtree(
    provider: &dyn vapor_providers::Provider,
    root: &RemotePath,
) -> Result<Vec<RemoteEntry>, ProviderError> {
    list_subtree_with(root, |directory| provider.enumerate(directory))
}

/// The walk of [`list_subtree`] over any listing of one directory.
///
/// The listing is not one call: other work removes entries while it
/// runs. A folder below `root` that is gone when its turn comes has
/// left the subtree, so it is taken out of the result and the walk
/// goes on. Only a `root` that is gone makes the listing `NotFound`:
/// the caller reads that as "the folder itself is absent", and a
/// folder that lost one subfolder is not absent.
fn list_subtree_with(
    root: &RemotePath,
    mut enumerate: impl FnMut(&RemotePath) -> Result<Vec<RemoteEntry>, ProviderError>,
) -> Result<Vec<RemoteEntry>, ProviderError> {
    let mut collected = Vec::new();
    let mut pending = std::collections::VecDeque::from([root.clone()]);
    while let Some(directory) = pending.pop_front() {
        let entries = match enumerate(&directory) {
            Ok(entries) => entries,
            Err(error)
                if error.kind == vapor_shared::ProviderErrorKind::NotFound
                    && directory != *root =>
            {
                collected.retain(|entry: &RemoteEntry| entry.path != directory);
                continue;
            }
            Err(error) => return Err(error),
        };
        for entry in entries {
            if entry.kind == vapor_providers::RemoteEntryKind::Directory {
                pending.push_back(entry.path.clone());
            }
            collected.push(entry);
            if collected.len() >= SUBTREE_LISTING_CAP {
                return Ok(collected);
            }
        }
    }
    Ok(collected)
}

/// A transfer that waits for bandwidth resumes when the bucket holds
/// this share of one second of rate: four resumes a second at most.
const USEFUL_GRANTS_PER_SECOND: u64 = 4;

/// Steps a transfer session until it completes, fails, is cancelled, or
/// must hold (gate closed / bandwidth dry). The byte budget of a step
/// is the session's own chunk size capped by the shared shaper, so one
/// step is always bounded and interruptible.
fn run_transfer(
    context: &JobContext,
    gates: &TransferGates,
    generation: u64,
    mut session: Box<dyn TransferSession>,
    direction: TransferDirection,
) -> ProviderJobOutcome {
    let mut zero_progress_steps: u32 = 0;
    loop {
        if gates.generation.load(Ordering::SeqCst) != generation {
            session.abort();
            return ProviderJobOutcome::Cancelled;
        }
        let allowed = match direction {
            TransferDirection::Upload => gates.allow_uploads.load(Ordering::Relaxed),
            TransferDirection::Download => gates.allow_downloads.load(Ordering::Relaxed),
        };
        if !allowed {
            return ProviderJobOutcome::TransferHeld {
                session,
                resume_after: None,
            };
        }
        // No more than the session can still move: a small file must
        // not empty the bucket for the others.
        let step_bytes = session.step_bytes();
        let step_bytes = session
            .remaining_bytes()
            .map_or(step_bytes, |left| left.min(step_bytes));
        let now = context.clock.now();
        // An old estimate of the link gets one step without the
        // limiter: a short probe, so the ceiling follows the link.
        let probing = context.bandwidth.take_probe(direction, now);
        // A step with no limiter on it reads the link.
        let mut unlimited = probing;
        let grant = if probing {
            Ok(step_bytes.min(context.bandwidth.probe_bytes(direction, now)))
        } else {
            let mut shaper = context.bandwidth.shaper(direction);
            unlimited = shaper.rate().is_none();
            let useful = shaper
                .rate()
                .map(|rate| rate / USEFUL_GRANTS_PER_SECOND)
                .unwrap_or(step_bytes);
            shaper.grant_or_wait(step_bytes, useful, now)
        };
        let grant = match grant {
            Ok(grant) => grant,
            // Bucket dry: hold at the checkpoint until a useful grant
            // has refilled.
            Err(wait) => {
                return ProviderJobOutcome::TransferHeld {
                    session,
                    resume_after: Some(wait),
                };
            }
        };
        // What a step that ends the transfer moved: the session knows
        // only before the step.
        let left_before = session.remaining_bytes();
        match session.step(grant) {
            Ok(TransferStep::Progressed { bytes_transferred }) => {
                let done = context.clock.now();
                context.bandwidth.record_step(
                    direction,
                    bytes_transferred,
                    done.saturating_duration_since(now),
                    done,
                    if unlimited {
                        crate::bandwidth::StepReading::Link
                    } else {
                        crate::bandwidth::StepReading::AtLeast
                    },
                );
                // Return the slack: a step routinely spends less than its
                // grant (chunk alignment / short final chunk), and keeping
                // it would undershoot the configured rate. A probe took
                // nothing from the bucket.
                if !probing {
                    refund(context, direction, grant.saturating_sub(bytes_transferred));
                    // A session that cannot send less than a whole chunk
                    // moved more than its grant: the bucket pays for
                    // those bytes too.
                    charge(context, direction, bytes_transferred.saturating_sub(grant));
                }
                if bytes_transferred == 0 {
                    zero_progress_steps += 1;
                    if zero_progress_steps
                        >= vapor_shared::constants::engine::MAX_ZERO_PROGRESS_TRANSFER_STEPS
                    {
                        session.abort();
                        return ProviderJobOutcome::TransferFailed {
                            error: ProviderError::transient(format!(
                                "transfer made no progress for {zero_progress_steps} consecutive steps"
                            )),
                            phase: TransferPhase::Step,
                        };
                    }
                } else {
                    zero_progress_steps = 0;
                }
            }
            Ok(TransferStep::Completed(outcome)) => {
                let done = context.clock.now();
                if !probing {
                    // What was left is what this step moved, at most
                    // the whole object: a source that shrank during the
                    // transfer moved less than was left.
                    let moved = left_before.unwrap_or(0).min(outcome.bytes_total);
                    charge(context, direction, moved.saturating_sub(grant));
                }
                // The step that ends a transfer also finishes it at
                // the provider, and a provider that sends a small file
                // in one request sends it here: the time covers more
                // than these bytes.
                context.bandwidth.record_step(
                    direction,
                    left_before.unwrap_or(0).min(grant),
                    done.saturating_duration_since(now),
                    done,
                    crate::bandwidth::StepReading::AtLeast,
                );
                return ProviderJobOutcome::TransferCompleted {
                    outcome,
                    displaced: None,
                };
            }
            Err(error) => {
                // A failed step moved zero bytes: refund the whole grant
                // so a retry storm cannot burn the shared bucket.
                if !probing {
                    refund(context, direction, grant);
                }
                session.abort();
                return ProviderJobOutcome::TransferFailed {
                    error,
                    phase: TransferPhase::Step,
                };
            }
        }
    }
}

fn refund(context: &JobContext, direction: TransferDirection, unused: u64) {
    if unused == 0 {
        return;
    }
    context.bandwidth.shaper(direction).refund(unused);
}

fn charge(context: &JobContext, direction: TransferDirection, extra: u64) {
    if extra == 0 {
        return;
    }
    context.bandwidth.shaper(direction).charge(extra);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use std::sync::atomic::AtomicUsize;

    struct StubProvider {
        delete_calls: AtomicUsize,
        panic_on_delete: bool,
    }

    impl StubProvider {
        fn new() -> Self {
            Self {
                delete_calls: AtomicUsize::new(0),
                panic_on_delete: false,
            }
        }

        fn panicking() -> Self {
            Self {
                delete_calls: AtomicUsize::new(0),
                panic_on_delete: true,
            }
        }
    }

    impl Provider for StubProvider {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn capabilities(&self) -> vapor_providers::ProviderCapabilities {
            vapor_providers::ProviderCapabilities::FILESYSTEM
        }
        fn ensure_cloud_root(&self, _dir: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        fn enumerate(&self, _dir: &RemotePath) -> Result<Vec<RemoteEntry>, ProviderError> {
            Ok(Vec::new())
        }
        fn stat(&self, _path: &RemotePath) -> Result<Option<RemoteEntry>, ProviderError> {
            Ok(None)
        }
        fn content_hash(&self, _path: &RemotePath) -> Result<String, ProviderError> {
            Ok("hash".to_string())
        }
        fn begin_upload(
            &self,
            _request: UploadRequest,
        ) -> Result<Box<dyn TransferSession>, ProviderError> {
            Err(ProviderError::permanent("stub cannot upload"))
        }
        fn begin_download(
            &self,
            _request: DownloadRequest,
        ) -> Result<Box<dyn TransferSession>, ProviderError> {
            Err(ProviderError::permanent("stub cannot download"))
        }
        fn delete(&self, _path: &RemotePath, _op_id: &str) -> Result<(), ProviderError> {
            self.delete_calls.fetch_add(1, Ordering::SeqCst);
            assert!(!self.panic_on_delete, "simulated provider bug");
            Ok(())
        }
        fn poll_changes(
            &self,
            _cursor: Option<&str>,
            _max: usize,
        ) -> Result<vapor_providers::ChangesPoll, ProviderError> {
            Ok(vapor_providers::ChangesPoll::Page(
                vapor_providers::RemoteChangesPage {
                    changes: Vec::new(),
                    next_cursor: String::new(),
                },
            ))
        }
    }

    /// Counts the identity reads a provider serves.
    struct CountingIdentity {
        reads: std::sync::atomic::AtomicUsize,
    }

    #[test]
    fn one_identity_read_serves_every_worker_whose_call_returned_before_it_started() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = temp.path().join("cloud");
        std::fs::create_dir_all(&root).expect("root");
        let provider = vapor_providers::FilesystemProvider::with_root(&root).expect("provider");
        let text = root.to_string_lossy().into_owned();
        provider.adopt_root(&text, "testdev").expect("adopt");
        let counted = CountingIdentity {
            reads: std::sync::atomic::AtomicUsize::new(0),
        };
        let reads = RootIdentityReads::default();
        let read = |not_before: std::time::Instant| {
            let before = reads
                .last
                .lock()
                .expect("lock")
                .as_ref()
                .map(|last| last.started);
            let answer = reads.read_after(&provider, &text, not_before);
            let after = reads
                .last
                .lock()
                .expect("lock")
                .as_ref()
                .map(|last| last.started);
            if before != after {
                counted.reads.fetch_add(1, Ordering::SeqCst);
            }
            answer
        };

        // Two workers learned "gone" before any read started: one read.
        let learned = std::time::Instant::now();
        let first = read(learned).expect("identity");
        let second = read(learned).expect("identity");
        assert_eq!(first, second);
        assert_eq!(counted.reads.load(Ordering::SeqCst), 1);

        // A worker that learned it after that read needs a newer one: a
        // root swapped in between must not be vouched for by the old
        // answer.
        read(std::time::Instant::now()).expect("identity");
        assert_eq!(counted.reads.load(Ordering::SeqCst), 2);
    }

    fn context(provider: Arc<dyn Provider>) -> JobContext {
        JobContext {
            provider,
            bandwidth: Arc::default(),
            clock: Arc::new(SystemClock),
            root_identity: Arc::default(),
        }
    }

    #[test]
    fn inline_pool_completes_jobs_at_dispatch() {
        let provider: Arc<dyn Provider> = Arc::new(StubProvider::new());
        let mut pool = ProviderJobPool::inline();
        pool.dispatch(
            7,
            context(provider),
            ProviderJobKind::RemoteDelete {
                remote_path: RemotePath::root().join("a.txt").expect("path"),
                op_id: "op".to_string(),
            },
        );
        let completed = pool.harvest();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].intent_id, 7);
        assert!(matches!(
            completed[0].outcome,
            ProviderJobOutcome::RemoteDelete(Ok(()))
        ));
    }

    #[test]
    fn threaded_pool_runs_jobs_and_notifies_waker() {
        let provider = Arc::new(StubProvider::new());
        let waker = Arc::new(TickWaker::default());
        let mut pool = ProviderJobPool::threaded(2, Some(waker.clone()));
        pool.dispatch(
            1,
            context(provider.clone()),
            ProviderJobKind::RemoteDelete {
                remote_path: RemotePath::root().join("a.txt").expect("path"),
                op_id: "op".to_string(),
            },
        );
        // The waker unblocks as soon as the worker finishes; harvest then
        // returns the outcome without any sleep-based polling.
        let mut harvested = Vec::new();
        for _ in 0..1_000 {
            waker.wait_timeout(Duration::from_millis(50));
            harvested = pool.harvest();
            if !harvested.is_empty() {
                break;
            }
        }
        assert_eq!(harvested.len(), 1);
        assert_eq!(provider.delete_calls.load(Ordering::SeqCst), 1);
        assert_eq!(pool.in_flight(), 0);
    }

    #[test]
    fn threaded_worker_survives_a_panicking_provider_call() {
        let waker = Arc::new(TickWaker::default());
        let mut pool = ProviderJobPool::threaded(1, Some(waker.clone()));
        let harvest_one = |pool: &mut ProviderJobPool| {
            for _ in 0..1_000 {
                waker.wait_timeout(Duration::from_millis(50));
                let harvested = pool.harvest();
                if !harvested.is_empty() {
                    return harvested;
                }
            }
            Vec::new()
        };
        pool.dispatch(
            11,
            context(Arc::new(StubProvider::panicking())),
            ProviderJobKind::RemoteDelete {
                remote_path: RemotePath::root().join("a.txt").expect("path"),
                op_id: "op".to_string(),
            },
        );
        let harvested = harvest_one(&mut pool);
        assert_eq!(harvested.len(), 1);
        assert_eq!(harvested[0].intent_id, 11);
        assert!(matches!(
            &harvested[0].outcome,
            ProviderJobOutcome::Panicked(message) if message.contains("simulated provider bug")
        ));
        assert_eq!(pool.in_flight(), 0);

        // The single worker is still alive: a healthy job completes.
        let provider = Arc::new(StubProvider::new());
        pool.dispatch(
            12,
            context(provider.clone()),
            ProviderJobKind::RemoteDelete {
                remote_path: RemotePath::root().join("b.txt").expect("path"),
                op_id: "op".to_string(),
            },
        );
        let harvested = harvest_one(&mut pool);
        assert_eq!(harvested.len(), 1);
        assert!(matches!(
            harvested[0].outcome,
            ProviderJobOutcome::RemoteDelete(Ok(()))
        ));
        assert_eq!(provider.delete_calls.load(Ordering::SeqCst), 1);
    }

    struct StalledSession;

    impl TransferSession for StalledSession {
        fn step(&mut self, _max_bytes: u64) -> Result<TransferStep, ProviderError> {
            Ok(TransferStep::Progressed {
                bytes_transferred: 0,
            })
        }
        fn abort(&mut self) {}
    }

    #[test]
    fn a_session_that_never_moves_a_byte_fails_instead_of_spinning() {
        let provider: Arc<dyn Provider> = Arc::new(StubProvider::new());
        let gates = TransferGates::new();
        let outcome = run_transfer(
            &context(provider),
            &gates,
            gates.generation.load(Ordering::SeqCst),
            Box::new(StalledSession),
            TransferDirection::Download,
        );
        assert!(matches!(
            outcome,
            ProviderJobOutcome::TransferFailed { error, phase: TransferPhase::Step }
                if error.kind == vapor_shared::ProviderErrorKind::Transient
                    && error.message.contains("no progress")
        ));
    }

    /// A session that sends a whole chunk whatever the grant was, as a
    /// resumable upload to Google Drive does.
    struct WholeChunkSession {
        left: u64,
    }

    const CHUNK_BYTES: u64 = 256 * 1024;

    impl TransferSession for WholeChunkSession {
        fn step(&mut self, _max_bytes: u64) -> Result<TransferStep, ProviderError> {
            let moved = CHUNK_BYTES.min(self.left);
            self.left -= moved;
            Ok(TransferStep::Progressed {
                bytes_transferred: moved,
            })
        }
        fn step_bytes(&self) -> u64 {
            CHUNK_BYTES
        }
        fn remaining_bytes(&self) -> Option<u64> {
            Some(self.left.max(1))
        }
        fn abort(&mut self) {}
    }

    #[test]
    fn a_step_that_moves_more_than_its_grant_is_paid_for_before_the_next_step() {
        let provider: Arc<dyn Provider> = Arc::new(StubProvider::new());
        let context = context(provider);
        // 64 000 bytes a second: a quarter of one chunk.
        context.bandwidth.pin_capacity_kbps(512);
        context.bandwidth.apply_ceiling(100, context.clock.now());
        let gates = TransferGates::new();

        let outcome = run_transfer(
            &context,
            &gates,
            gates.generation.load(Ordering::SeqCst),
            Box::new(WholeChunkSession {
                left: 4 * CHUNK_BYTES,
            }),
            TransferDirection::Upload,
        );

        // The first grant is one second of rate at most, and the step
        // sent a whole chunk on it. The session holds until the refill
        // has paid for the three quarters that no grant covered.
        let ProviderJobOutcome::TransferHeld {
            resume_after: Some(wait),
            ..
        } = outcome
        else {
            panic!("the transfer holds for the bucket");
        };
        assert!(
            wait >= Duration::from_secs(3),
            "about 198 KiB of debt at 64 000 bytes a second, got {wait:?}"
        );
    }

    #[test]
    fn threaded_provider_call_delivers_its_result_and_reports_panics() {
        let waker = Arc::new(TickWaker::default());
        let mode = ProviderCallMode::Threaded {
            waker: Some(waker.clone()),
        };
        let mut call = ProviderCall::start(&mode, "test", || 41 + 1);
        let mut result = None;
        for _ in 0..1_000 {
            waker.wait_timeout(Duration::from_millis(50));
            result = call.take();
            if result.is_some() {
                break;
            }
        }
        assert_eq!(result, Some(Ok(42)));
        assert!(call.take().is_none(), "a result is handed out once");

        let mut call = ProviderCall::start(&mode, "test", || -> u8 { panic!("boom") });
        let mut result = None;
        for _ in 0..1_000 {
            waker.wait_timeout(Duration::from_millis(50));
            result = call.take();
            if result.is_some() {
                break;
            }
        }
        assert!(matches!(result, Some(Err(message)) if message.contains("boom")));

        let mut inline = ProviderCall::start(&ProviderCallMode::Inline, "test", || "now");
        assert_eq!(inline.take(), Some(Ok("now")));
    }

    #[test]
    fn aborted_generation_cancels_undispatched_work() {
        let provider: Arc<dyn Provider> = Arc::new(StubProvider::new());
        let mut pool = ProviderJobPool::inline();
        pool.abort_in_flight();
        // Jobs dispatched after the bump run under the new generation.
        pool.dispatch(
            2,
            context(provider),
            ProviderJobKind::Probe(ProbeRequest {
                remote_path: RemotePath::root().join("b.txt").expect("path"),
                want_stat: true,
                hash: ProbeHash::Never,
                want_subtree_listing: false,
                stat_also: None,
                local_hashes_if_absent: None,
                cloud_root_identity_of: None,
            }),
        );
        let completed = pool.harvest();
        assert!(matches!(completed[0].outcome, ProviderJobOutcome::Probe(_)));
    }

    fn listed(path: &str, kind: vapor_providers::RemoteEntryKind) -> RemoteEntry {
        RemoteEntry {
            path: RemotePath::new(path).expect("remote path"),
            kind,
            size_bytes: 0,
            modified_at: std::time::SystemTime::UNIX_EPOCH,
            content_hash: None,
            op_id: None,
            id: None,
        }
    }

    fn gone(path: &RemotePath) -> ProviderError {
        ProviderError::new(
            vapor_shared::ProviderErrorKind::NotFound,
            format!("{} is gone", path.as_str()),
        )
    }

    #[test]
    fn a_subfolder_that_leaves_during_the_listing_is_not_the_folder_itself() {
        use vapor_providers::RemoteEntryKind::{Directory, File};
        let root = RemotePath::new("tree").expect("root");
        // Another job removes `tree/deeper` between the listing of
        // `tree` and the listing of `tree/deeper`.
        let listing = list_subtree_with(&root, |directory| match directory.as_str() {
            "tree" => Ok(vec![
                listed("tree/deeper", Directory),
                listed("tree/one.txt", File),
            ]),
            _ => Err(gone(directory)),
        })
        .expect("the folder is there, so its listing is");
        let paths: Vec<&str> = listing.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(paths, vec!["tree/one.txt"]);
    }

    #[test]
    fn a_folder_that_is_gone_itself_makes_the_listing_not_found() {
        let root = RemotePath::new("tree").expect("root");
        let error = list_subtree_with(&root, |directory| Err(gone(directory)))
            .expect_err("the folder is gone");
        assert_eq!(error.kind, vapor_shared::ProviderErrorKind::NotFound);
    }

    #[test]
    fn a_failure_below_the_folder_that_is_not_an_absence_stops_the_listing() {
        use vapor_providers::RemoteEntryKind::Directory;
        let root = RemotePath::new("tree").expect("root");
        let error = list_subtree_with(&root, |directory| match directory.as_str() {
            "tree" => Ok(vec![listed("tree/deeper", Directory)]),
            _ => Err(ProviderError::transient("the listing timed out")),
        })
        .expect_err("a listing that failed says nothing about the entries");
        assert_ne!(error.kind, vapor_shared::ProviderErrorKind::NotFound);
    }
}
