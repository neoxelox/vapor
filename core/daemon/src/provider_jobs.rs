//! Off-thread execution of blocking provider I/O.
//!
//! Every provider call the staged executor makes — remote stats and
//! content hashes during planning, `begin_upload`/`begin_download`,
//! transfer-session steps, and remote deletes — is a blocking network
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
//! once the gate reopens — the same hold-at-checkpoint discipline the
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
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use vapor_providers::{
    BandwidthShaper, DownloadRequest, Provider, ProviderError, RemoteEntry, RemotePath,
    TransferOutcome, TransferSession, TransferStep, UploadRequest,
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
    /// Bumped on abort/shutdown; jobs dispatched under an older
    /// generation abort their session and report `Cancelled`.
    generation: AtomicU64,
}

impl TransferGates {
    fn new() -> Self {
        Self {
            allow_uploads: AtomicBool::new(true),
            allow_downloads: AtomicBool::new(true),
            generation: AtomicU64::new(0),
        }
    }
}

/// Everything a job needs from the dispatching tick, captured by value
/// so the worker never touches tick-thread state. The provider is the
/// app's `Arc` (swappable in tests); shaper and step knob are the
/// daemon-wide shared instances, so auto-tune and rate changes reach
/// in-flight transfers on their next step.
pub(crate) struct JobContext {
    pub provider: Arc<dyn Provider>,
    pub bandwidth: Arc<Mutex<BandwidthShaper>>,
    pub transfer_step_bytes: Arc<AtomicU64>,
    pub clock: Arc<dyn Clock>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransferDirection {
    Upload,
    Download,
}

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
/// remote read on filesystem-backed providers — only fetched when the
/// planner's decision actually needs it).
pub(crate) enum ProbeHash {
    Never,
    /// Unconditional hash fetch (upload-gate verification).
    Always,
    /// Fetch only when the stat found an entry whose op-id differs from
    /// the sync index's and which carries no hash of its own — the
    /// divergence check would otherwise have nothing to compare.
    IfDivergedFrom {
        index_op_id: String,
    },
}

pub(crate) struct ProbeRequest {
    pub remote_path: RemotePath,
    pub want_stat: bool,
    pub hash: ProbeHash,
}

pub(crate) struct ProbeResult {
    /// `None` when the probe did not request a stat.
    pub stat: Option<Result<Option<RemoteEntry>, ProviderError>>,
    /// `Some` only when a hash fetch was attempted.
    pub content_hash: Option<Result<String, ProviderError>>,
}

impl ProbeResult {
    /// The fetched hash, folded to `None` on fetch failure — matching
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
    /// `begin_upload` + step loop.
    Upload(UploadRequest),
    /// `begin_download` + step loop.
    Download(DownloadRequest),
    /// Re-dispatched held session.
    ResumeTransfer {
        session: Box<dyn TransferSession>,
        direction: TransferDirection,
    },
}

pub(crate) enum ProviderJobOutcome {
    Probe(ProbeResult),
    RemoteDelete(Result<(), ProviderError>),
    /// Session held at its checkpoint: a gate closed or the bandwidth
    /// bucket ran dry. The executor re-dispatches on a later tick (its
    /// stage already knows the direction).
    TransferHeld {
        session: Box<dyn TransferSession>,
    },
    TransferCompleted(TransferOutcome),
    /// The session (or its begin call) failed; any session was already
    /// aborted worker-side.
    TransferFailed {
        error: ProviderError,
        phase: TransferPhase,
    },
    /// The job's generation was invalidated by `abort_in_flight`; any
    /// session was aborted worker-side. Harvest drops these.
    Cancelled,
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
    /// transfer loops observe throttle transitions between steps.
    pub fn update_gates(&self, allow_uploads: bool, allow_downloads: bool) {
        self.gates
            .allow_uploads
            .store(allow_uploads, Ordering::Relaxed);
        self.gates
            .allow_downloads
            .store(allow_downloads, Ordering::Relaxed);
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
        let completed = run_job(job, &gates);
        if results_tx.send(completed).is_err() {
            return;
        }
        if let Some(waker) = &waker {
            waker.notify();
        }
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
        ProviderJobKind::Probe(request) => ProviderJobOutcome::Probe(run_probe(&context, request)),
        ProviderJobKind::RemoteDelete { remote_path, op_id } => {
            ProviderJobOutcome::RemoteDelete(context.provider.delete(&remote_path, &op_id))
        }
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
        ProviderJobKind::Download(request) => match context.provider.begin_download(request) {
            Ok(session) => run_transfer(
                &context,
                gates,
                generation,
                session,
                TransferDirection::Download,
            ),
            Err(error) => ProviderJobOutcome::TransferFailed {
                error,
                phase: TransferPhase::Begin,
            },
        },
        ProviderJobKind::ResumeTransfer { session, direction } => {
            run_transfer(&context, gates, generation, session, direction)
        }
    };
    CompletedJob {
        intent_id,
        outcome,
        generation,
    }
}

fn run_probe(context: &JobContext, request: ProbeRequest) -> ProbeResult {
    let stat = request
        .want_stat
        .then(|| context.provider.stat(&request.remote_path));
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
    ProbeResult { stat, content_hash }
}

/// Steps a transfer session until it completes, fails, is cancelled, or
/// must hold (gate closed / bandwidth dry). Byte budgets per step come
/// from the shared auto-tuned knob capped by the shared shaper, so one
/// step is always bounded and interruptible.
fn run_transfer(
    context: &JobContext,
    gates: &TransferGates,
    generation: u64,
    mut session: Box<dyn TransferSession>,
    direction: TransferDirection,
) -> ProviderJobOutcome {
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
            return ProviderJobOutcome::TransferHeld { session };
        }
        let step_bytes = context.transfer_step_bytes.load(Ordering::Relaxed);
        let grant = context
            .bandwidth
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .budget(step_bytes, context.clock.now());
        if grant == 0 {
            // Bucket dry: hold at the checkpoint until tokens refill.
            return ProviderJobOutcome::TransferHeld { session };
        }
        match session.step(grant) {
            Ok(TransferStep::Progressed { bytes_transferred }) => {
                // Return the slack: a step routinely spends less than its
                // grant (chunk alignment / short final chunk), and keeping
                // it would undershoot the configured rate.
                refund(context, grant.saturating_sub(bytes_transferred));
            }
            Ok(TransferStep::Completed(outcome)) => {
                return ProviderJobOutcome::TransferCompleted(outcome);
            }
            Err(error) => {
                // A failed step moved zero bytes: refund the whole grant
                // so a retry storm cannot burn the shared bucket.
                refund(context, grant);
                session.abort();
                return ProviderJobOutcome::TransferFailed {
                    error,
                    phase: TransferPhase::Step,
                };
            }
        }
    }
}

fn refund(context: &JobContext, unused: u64) {
    if unused == 0 {
        return;
    }
    context
        .bandwidth
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .refund(unused);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    struct StubProvider {
        delete_calls: AtomicUsize,
    }

    impl StubProvider {
        fn new() -> Self {
            Self {
                delete_calls: AtomicUsize::new(0),
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
        fn ensure_cloud_sync_directory(&self, _dir: &str) -> Result<(), ProviderError> {
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
            Ok(())
        }
        fn rename(
            &self,
            _from: &RemotePath,
            _to: &RemotePath,
            _op_id: &str,
        ) -> Result<(), ProviderError> {
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

    fn context(provider: Arc<dyn Provider>) -> JobContext {
        JobContext {
            provider,
            bandwidth: Arc::new(Mutex::new(BandwidthShaper::unlimited())),
            transfer_step_bytes: Arc::new(AtomicU64::new(1024)),
            clock: Arc::new(SystemClock),
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
            }),
        );
        let completed = pool.harvest();
        assert!(matches!(completed[0].outcome, ProviderJobOutcome::Probe(_)));
    }
}
