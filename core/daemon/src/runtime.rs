use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use vapor_platform::fs_caps::NativeFilesystemCapabilities;
use vapor_platform::fs_watch::native_watcher_available;
use vapor_providers::Provider;
use vapor_providers::tags::OpIdTagStore;
use vapor_shared::{RunState, constants};

use crate::clock::{SharedClock, system_clock};
use crate::debounce::{DebounceLoop, DebounceWindows};
use crate::event_intents::BoundedFsEventRecorder;
use crate::executor::{ExecutionEnv, StagedExecutor, StagedExecutorSnapshot};
use crate::fs_events::{
    FsEventErrorRecord, FsEventKind, FsEventRecord, FsEventRecording, FsEventsWatcher,
    FsEventsWatcherError, normalize_watch_root,
};
use crate::ipc_service::{DaemonStatusSnapshot, StatusPublisher};
use crate::logging;
use crate::metrics::{MetricsSampler, StaticMetricsSampler};
use crate::path_filter::EventPathFilterOptions;
use crate::remote_sync::{RemotePollReport, RemotePoller};
use crate::scheduler::KeyedSupersedingScheduler;
use crate::self_write_cache::SelfWriteCache;
use crate::state_db::{DurableIntentRecord, DurableStateDb, StateDbError};
use crate::sync_directories::SyncScope;
use crate::throttle::ThrottleInputs;
use crate::{DaemonApp, event_intents::PendingIntentKind};

/// The profile id of a runtime that no profile set names (a runtime on
/// its own).
pub const DEFAULT_PROFILE_ID: &str = constants::profile::DEFAULT_PROFILE_ID;

/// Consecutive tick failures tolerated before the runtime loop gives up.
/// One inconsistent row or transient I/O error is logged and survived;
/// a structurally broken database fails fast after this many attempts.
const MAX_CONSECUTIVE_TICK_ERRORS: u32 = 5;

/// Why nothing moves before the first ignore rules are in.
const IGNORE_RULES_NOT_READ: &str =
    "reading the ignore files of the folder; no work starts before the rules are in";

/// What the whole-scope reconcile is doing, as `vapor status` reports
/// it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReconcileStatus {
    /// Nothing queued or running.
    Idle,
    /// A scan is queued and the throttle is holding it; `reason`
    /// names what it waits for.
    Waiting { reason: String },
    /// A scan holds the reconcile permit.
    Running { root: String },
}

impl ReconcileStatus {
    /// Wire label for the status payload.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => constants::reconcile_state::IDLE,
            Self::Waiting { .. } => constants::reconcile_state::WAITING,
            Self::Running { .. } => constants::reconcile_state::RUNNING,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Self::Idle => String::new(),
            Self::Waiting { reason } => reason.clone(),
            Self::Running { root } => format!("scanning {root}"),
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Self::Idle => 0,
            Self::Waiting { .. } => 1,
            Self::Running { .. } => 2,
        }
    }

    /// The more active of two, for the daemon-wide summary.
    pub fn more_active(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

#[derive(Debug)]
pub enum DaemonRuntimeError {
    Watcher(FsEventsWatcherError),
    StateDb(StateDbError),
    /// The local root the profile adopted is not there. Waited for by
    /// the caller; never re-created.
    LocalRootMissing(PathBuf),
    /// The state database did not take the writes of a tick (a full
    /// drive, an I/O error at the commit). What the runtime holds in
    /// memory is ahead of the database from here on, so the caller
    /// composes the profile again from the database, as after a kill
    /// inside a tick.
    TickNotSaved(StateDbError),
}

impl From<FsEventsWatcherError> for DaemonRuntimeError {
    fn from(value: FsEventsWatcherError) -> Self {
        Self::Watcher(value)
    }
}

/// What a cloud-root ensure or identity probe found, computed on the
/// worker so the tick thread never waits on the provider.
#[derive(Debug)]
enum CloudRootOutcome {
    /// Present, ensured, and carrying the recorded identity (or just
    /// adopted: the identity to record is carried along).
    Ready {
        adopted: Option<String>,
    },
    Missing,
    Unreachable(String),
    Replaced {
        found: Option<String>,
    },
    /// The provider refused the sign-in, so nothing about the root is
    /// known. Carries the provider's message.
    SignInRequired(String),
}

/// What a runtime does on its own while it is built.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Startup {
    /// A runtime on its own. It starts a watcher when asked, and it
    /// probes the cloud root before `build` returns.
    Standalone { start_watcher: bool },
    /// One profile of the daemon. The daemon owns the watchers, and the
    /// first cloud-root probe waits for the first tick, where it runs
    /// on a worker: a provider that does not answer must not delay the
    /// daemon's start, and with it the IPC server.
    DaemonProfile,
}

#[derive(Clone, Debug)]
struct RootHold {
    side: crate::root_identity::RootSide,
    reason: String,
    /// The open decision the hold waits on (`root-missing` or
    /// `root-replaced`).
    decision_id: Option<i64>,
    /// An absence rather than a replacement.
    missing: bool,
}

/// Ensures or checks the cloud root, on whichever thread calls it.
/// With no recorded identity the root is ensured (created when the
/// backend allows) and adopted; with one it is only checked, never
/// created, and ensured when it matches so the provider's root cache
/// is primed.
fn probe_cloud_root(
    provider: Arc<dyn Provider>,
    cloud_root: &str,
    recorded: Option<String>,
    device_id: &str,
) -> CloudRootOutcome {
    use crate::root_identity::{RootStatus, classify_cloud_probe};
    let unreachable = |error: vapor_providers::ProviderError| {
        if error.kind == vapor_shared::ProviderErrorKind::Authentication {
            CloudRootOutcome::SignInRequired(error.message)
        } else {
            CloudRootOutcome::Unreachable(error.message)
        }
    };
    match recorded {
        None => match provider.ensure_cloud_root(cloud_root) {
            Ok(()) => match provider.adopt_root(cloud_root, device_id) {
                Ok(identity) => {
                    logging::info(
                        "Adopted the cloud root",
                        &[
                            ("cloud_root", cloud_root.to_string()),
                            (
                                "identity",
                                identity.clone().unwrap_or_else(|| "none".to_string()),
                            ),
                        ],
                    );
                    CloudRootOutcome::Ready {
                        adopted: Some(identity.unwrap_or_default()),
                    }
                }
                Err(error) => unreachable(error),
            },
            Err(error) => unreachable(error),
        },
        Some(recorded) => {
            let probe = match provider.root_identity(cloud_root) {
                Err(error) if error.kind == vapor_shared::ProviderErrorKind::Authentication => {
                    return CloudRootOutcome::SignInRequired(error.message);
                }
                probe => probe,
            };
            match classify_cloud_probe(&recorded, probe) {
                RootStatus::Ready => match provider.ensure_cloud_root(cloud_root) {
                    Ok(()) => CloudRootOutcome::Ready { adopted: None },
                    Err(error) => unreachable(error),
                },
                RootStatus::Missing => CloudRootOutcome::Missing,
                RootStatus::Unreachable(message) => CloudRootOutcome::Unreachable(message),
                RootStatus::Replaced { found, .. } => CloudRootOutcome::Replaced { found },
            }
        }
    }
}

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Tick waker the shutdown path pings so the loop exits immediately
/// instead of sleeping out a full idle interval. Set by the running
/// multi-profile runtime. The shutdown handler runs on a dedicated
/// thread (not a raw signal context), so taking this lock is safe.
static SHUTDOWN_WAKER: Mutex<Option<Arc<TickWaker>>> = Mutex::new(None);

pub fn register_shutdown_waker(waker: Arc<TickWaker>) {
    *SHUTDOWN_WAKER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(waker);
}

pub fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
    if let Some(waker) = SHUTDOWN_WAKER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
    {
        waker.notify();
    }
}

pub fn is_shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
}

impl From<StateDbError> for DaemonRuntimeError {
    fn from(value: StateDbError) -> Self {
        Self::StateDb(value)
    }
}

/// Wakes the runtime loop out of its inter-tick sleep. Signaled by the
/// fs-event callback (via [`NotifyingRecorder`]) and by IPC control
/// requests so the daemon reacts immediately instead of waiting out the
/// poll interval, which in turn lets a fully idle daemon sleep at the
/// longer [`constants::engine::IDLE_TICK_MILLIS`] cadence.
#[derive(Debug, Default)]
pub struct TickWaker {
    signaled: Mutex<bool>,
    condvar: Condvar,
}

impl TickWaker {
    pub fn notify(&self) {
        let mut signaled = self.signaled.lock().expect("TickWaker mutex poisoned");
        *signaled = true;
        self.condvar.notify_all();
    }

    /// Blocks until notified or until `timeout` elapses, whichever comes
    /// first, then clears the signal.
    pub fn wait_timeout(&self, timeout: Duration) {
        let mut signaled = self.signaled.lock().expect("TickWaker mutex poisoned");
        if !*signaled {
            let (guard, _) = self
                .condvar
                .wait_timeout(signaled, timeout)
                .expect("TickWaker mutex poisoned");
            signaled = guard;
        }
        *signaled = false;
    }
}

/// Recorder adapter handed to the fs watcher: forwards every callback to
/// the real bounded recorder and pings the tick waker so the runtime
/// drains promptly.
struct NotifyingRecorder {
    inner: Arc<BoundedFsEventRecorder>,
    waker: Arc<TickWaker>,
}

impl FsEventRecording for NotifyingRecorder {
    fn record_event(&self, event: FsEventRecord) {
        self.inner.record_event(event);
        self.waker.notify();
    }

    fn record_error(&self, error: FsEventErrorRecord) {
        self.inner.record_error(error);
        self.waker.notify();
    }
}

/// The retry state of an answer that could not be applied.
#[derive(Debug)]
struct DecisionRetry {
    failures: usize,
    next_attempt: SystemTime,
    /// The reason last logged, so the same reason is logged once.
    last_error: Option<String>,
}

impl Default for DecisionRetry {
    fn default() -> Self {
        Self {
            failures: 0,
            next_attempt: SystemTime::UNIX_EPOCH,
            last_error: None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeTickReport {
    pub released_deferred_reconciles: usize,
    pub drained_pending_intents: usize,
    pub stabilized_events: usize,
    /// Stabilized local events suppressed as echoes of the daemon's own
    /// local applies (loop prevention).
    pub suppressed_local_echoes: usize,
    pub durable_enqueues: usize,
    pub leased_intents: usize,
    pub started_staged_intents: usize,
    pub completed_intents: usize,
    pub requeued_intents: usize,
    /// Intents finalized as terminal failures this tick.
    pub failed_intents: usize,
    /// Strict-mirror reverts observed this tick (one-way modes).
    pub mirror_reverts: usize,
    /// Strict-mirror deletions performed this tick (one-way modes).
    pub mirror_deletes: usize,
    /// Keep-both conflict copies created this tick.
    pub conflicts: usize,
    /// The files those copies belong to, one path for each copy.
    pub conflict_paths: Vec<PathBuf>,
    /// Renames carried out as moves (server-side or local) instead of
    /// transfers.
    pub moves: usize,
    /// What this tick removed from the cloud, each with the cause.
    pub cloud_deletes: Vec<(PathBuf, &'static str)>,
    pub started_reconcile_root: Option<PathBuf>,
    pub completed_reconcile_root: Option<PathBuf>,
    pub staged_executor: StagedExecutorSnapshot,
    /// Remote changes-feed poll outcome.
    pub remote_poll: RemotePollReport,
}

pub struct DaemonRuntime {
    app: DaemonApp,
    sync_scope: SyncScope,
    state_db: DurableStateDb,
    recorder: Option<Arc<BoundedFsEventRecorder>>,
    debounce: DebounceLoop,
    staged_executor: StagedExecutor,
    scheduler: KeyedSupersedingScheduler,
    watcher: Option<FsEventsWatcher>,
    /// Ignore-rule filter for this scope, shared with the watcher (when
    /// one runs in-process) or with the multi-profile deduplicated
    /// watcher for the same root. Consulted by the reconcile walk and
    /// the remote-change mapping so ignore rules apply symmetrically:
    /// an ignored name never syncs in either direction.
    path_filter: Option<Arc<crate::fs_events::SharedEventPathFilter>>,
    metrics_sampler: Arc<dyn MetricsSampler>,
    clock: SharedClock,
    tick_waker: Arc<TickWaker>,
    runtime_control: Option<Arc<crate::runtime_control::RuntimeControl>>,
    status_publisher: Option<Arc<dyn StatusPublisher>>,
    last_throttle_sample_inst: Option<Instant>,
    last_stale_lease_sweep_inst: Option<Instant>,
    /// Wall time of the last whole-scope scan (its start counts until
    /// one completes). Wall time, because a device that sleeps for the
    /// night is a day older when it wakes and the monotonic clock is
    /// not. Every start queues a whole-scope scan, so this starts at
    /// the start.
    last_whole_scope_scan_at: Option<SystemTime>,
    /// The names that cannot be synced which the whole-scope walk
    /// under way has met so far, over all of its slices.
    unsyncable_seen_by_walk: Vec<crate::unsyncable::UnsyncableName>,
    /// Why the local root cannot be read, while a walk found it so (a
    /// privacy permission the user refused, an I/O error). The run
    /// state carries it until a whole-scope walk reads the root again.
    local_root_unreadable: Option<String>,
    local_root_unreadable_this_walk: bool,
    running_reconcile_intent_id: Option<i64>,
    /// Set while a queued reconcile was refused a start because the
    /// throttle is not idle; cleared the moment one starts. Carries
    /// the throttle reason so status can say what it waits for.
    reconcile_wait_reason: Option<String>,
    startup_reconstruction_barrier: bool,
    startup_barrier_expires_inst: Option<Instant>,
    tick_interval: Duration,
    idle_tick_interval: Duration,
    throttle_sample_interval: Duration,
    stale_lease_sweep_interval: Duration,
    startup_barrier_deadline: Duration,
    /// Local-side op-id tag store (downloads tag applied files; echo
    /// suppression reads the tags back).
    tags: OpIdTagStore,
    /// Echo cache keyed by local path (suppresses watcher echoes of
    /// downloads / local applies).
    local_echoes: SelfWriteCache,
    /// Echo cache keyed by remote path (suppresses feed echoes of
    /// uploads / remote deletes).
    remote_echoes: SelfWriteCache,
    remote_poller: RemotePoller,
    /// Inline in tests; threaded once `enable_transfer_workers` runs so
    /// the changes poll, reconcile enumerate and cloud-root retry never
    /// hold the tick thread on a network round trip.
    provider_call_mode: crate::provider_jobs::ProviderCallMode,
    /// A cloud-root ensure or identity probe started on an earlier tick.
    cloud_root_call: Option<crate::provider_jobs::ProviderCall<CloudRootOutcome>>,
    /// A replaced or missing root holds the profile: nothing is leased
    /// and the status names why, until the root returns or a
    /// `root-replaced` decision is answered.
    root_hold: Option<RootHold>,
    /// Set while the profile is in a one-way mode that the user did
    /// not acknowledge yet: the reason for status. Nothing syncs.
    one_way_hold: Option<String>,
    last_root_check_inst: Option<Instant>,
    /// A watcher signal asked for a whole-scope reconcile; it is
    /// queued once the next root check finds the adopted root in
    /// place.
    reconcile_after_root_check: bool,
    /// Whether the provider-side sync root has been ensured. While
    /// `false`, no work is leased and the remote feed is not polled;
    /// ingest keeps capturing intent state durably.
    cloud_root_ready: bool,
    /// The folder at the local root is not the one the watcher was
    /// started on: the root went away and came back, or the user
    /// reattached another folder. A watch that follows the folder, not
    /// the path (inotify), reports nothing from the new one, so the
    /// owner of the watchers starts them again.
    local_root_changed: bool,
    /// The generation of the ignore rules this runtime has reconciled
    /// for (`SharedEventPathFilter::rules_generation`).
    seen_rules_generation: u64,
    /// The number of the last ignore-rule rebuild whose landing this
    /// runtime acted on.
    seen_rules_landed: u64,
    /// How many local entries the ignore rules hid from the last
    /// whole-scope scan that finished. `None` until one has.
    ignored_paths: Option<u64>,
    /// The cloud had no room for an upload: the provider's message and
    /// when it last said so. Uploads wait and come back on their own;
    /// status says why until an upload gets through again.
    cloud_full: Option<(String, Instant)>,
    /// The user paused this profile. A hold that comes and goes while
    /// the pause is on (a root that went away, a full cloud) changes
    /// what the state says, and never ends the pause.
    user_paused: bool,
    /// A profile of the daemon has not had the answer of its first
    /// cloud-root probe yet. That answer is the start of the profile,
    /// not the end of an outage.
    first_cloud_probe_pending: bool,
    last_cloud_root_attempt_inst: Option<Instant>,
    /// The provider refused the sign-in. Like an unavailable root it
    /// stops admission and the changes poll while ingest keeps
    /// recording intents; unlike one, nothing about the tree is in
    /// doubt, so release resumes the queue as it stands.
    sign_in_hold: bool,
    /// Incremental comparison walk of the currently-running reconcile.
    /// Survives slice pauses so a large tree converges across slices
    /// instead of restarting from scratch (strict mirror).
    reconcile_walker: Option<crate::reconcile_walk::ReconcileWalker>,
    /// A whole-scope reconcile was asked for while a whole-scope walk
    /// was under way. That walk passed some folders before the request,
    /// so one more runs after it.
    whole_scope_rescan_wanted: bool,
    /// Folders that appeared with an ignore file inside, and when. The
    /// files under them are reported once the rules of that file are in
    /// force; reported before, a file the rules hide would upload.
    folders_waiting_for_rules: Vec<(PathBuf, SystemTime)>,
    /// Folders a reconcile walk left for the same reason: each gets a
    /// walk of its own when the rules are in.
    walks_waiting_for_rules: Vec<PathBuf>,
    /// Cumulative strict-mirror observability counters:
    /// one-way modes must never be silent about the data they rewrite.
    mirror_revert_count: u64,
    mirror_delete_count: u64,
    /// Cumulative keep-both conflict copies created.
    conflict_count: u64,
    /// Daemon-wide activity timeline; shared across profiles.
    timeline: Option<Arc<crate::timeline::TimelineBuffer>>,
    /// Daemon-wide resource budget; shared across profiles.
    resource_budget: Arc<Mutex<crate::resource_budget::ResourceBudget>>,
    /// Daemon-wide bandwidth state (the two buckets and the measured
    /// link capacity); shared across profiles.
    bandwidth: Arc<crate::bandwidth::TransferBandwidth>,
    /// User-idle signal for the idle-boost gates. The daemon shares the
    /// host's notifier across profiles; a runtime on its own counts as
    /// always idle.
    idle_notifier: Arc<dyn vapor_platform::IdleNotifier>,
    /// Latest published ceilings + last sampled inputs.
    latest_ceilings: Option<crate::resource_budget::EffectiveCeilings>,
    last_sampled_inputs: Option<ThrottleInputs>,
    /// Last states emitted to the timeline, so transitions emit once.
    last_timeline_run_state: Option<RunState>,
    last_timeline_throttle: Option<vapor_shared::ThrottleState>,
    /// Stable device identifier. Resolved and persisted by the
    /// bootstrap; ephemeral (hostname-derived, unpersisted) in ad-hoc
    /// embeddings and tests.
    device_id: String,
    /// The resolved profile id this runtime serves. Used to attribute
    /// activity on the shared multi-profile timeline; defaults to the
    /// implicit `default` profile.
    profile_id: String,
    /// Timeline capacity captured before the first memory-pressure
    /// squeeze, so the configured limit is restored once pressure clears.
    timeline_default_entries: Option<usize>,
    /// Where a file Vapor removes on this device goes. `None` until
    /// the profile runtime attaches one; the bare fixtures unlink.
    trash: Option<Arc<crate::trash::LocalTrash>>,
    last_trash_purge_inst: Option<Instant>,
    /// The trash purge in flight, if any.
    trash_purge: Option<crate::provider_jobs::ProviderCall<usize>>,
    last_conflict_sync_inst: Option<Instant>,
    /// Bumped whenever this runtime opens or closes a decision, so the
    /// status publisher knows the open count moved without querying it
    /// on every idle tick.
    decision_changes: u64,
    /// Answers that could not be applied, by decision id: how many
    /// tries failed and when the next one is due. In memory only; a
    /// restart tries each answer again at once.
    decision_retries: std::collections::HashMap<i64, DecisionRetry>,
    /// Deadline of the reconcile boost (monotonic). `vapor reconcile`
    /// starts it; while it runs, deferred reconciles release at once,
    /// whatever their not-before time.
    reconcile_boost_until_inst: Option<Instant>,
}

impl DaemonRuntime {
    pub fn start(
        sync_scope: SyncScope,
        state_db: DurableStateDb,
        provider: Box<dyn Provider>,
    ) -> Result<Self, DaemonRuntimeError> {
        Self::start_with_sampler(
            sync_scope,
            state_db,
            provider,
            Arc::new(StaticMetricsSampler::default()),
        )
    }

    pub fn start_with_sampler(
        sync_scope: SyncScope,
        state_db: DurableStateDb,
        provider: Box<dyn Provider>,
        metrics_sampler: Arc<dyn MetricsSampler>,
    ) -> Result<Self, DaemonRuntimeError> {
        Self::start_with_sampler_and_clock(
            sync_scope,
            state_db,
            provider,
            metrics_sampler,
            system_clock(),
        )
    }

    pub fn start_with_sampler_and_clock(
        sync_scope: SyncScope,
        state_db: DurableStateDb,
        provider: Box<dyn Provider>,
        metrics_sampler: Arc<dyn MetricsSampler>,
        clock: SharedClock,
    ) -> Result<Self, DaemonRuntimeError> {
        Self::start_configured(
            sync_scope,
            EventPathFilterOptions::from_config(&vapor_shared::config::VaporConfig::default()),
            state_db,
            provider,
            metrics_sampler,
            clock,
        )
    }

    /// Fully-parameterized start used by the daemon bootstrap: explicit
    /// filter options resolved from `vapor.json`.
    pub fn start_configured(
        sync_scope: SyncScope,
        filter_options: EventPathFilterOptions,
        state_db: DurableStateDb,
        provider: Box<dyn Provider>,
        metrics_sampler: Arc<dyn MetricsSampler>,
        clock: SharedClock,
    ) -> Result<Self, DaemonRuntimeError> {
        let mut runtime = Self::build(
            sync_scope,
            filter_options,
            state_db,
            provider,
            metrics_sampler,
            clock.clone(),
            true,
        )?;
        runtime.enqueue_startup_reconstruction_reconcile(clock.now_system())?;
        Ok(runtime)
    }

    pub fn run_forever(&mut self) -> Result<(), DaemonRuntimeError> {
        let mut consecutive_tick_errors = 0u32;
        while !is_shutdown_requested() {
            match self.tick(self.clock.now_system()) {
                Ok(report) => {
                    consecutive_tick_errors = 0;
                    // Fully idle → coarser cadence; fs events and IPC
                    // requests re-arm the waker for an immediate tick.
                    let wait = if self.has_pending_work(&report) {
                        self.tick_interval
                    } else {
                        self.idle_tick_interval
                    };
                    self.tick_waker.wait_timeout(wait);
                }
                Err(error) => {
                    consecutive_tick_errors += 1;
                    logging::error(
                        "Daemon tick failed",
                        &[
                            ("error", format!("{error:?}")),
                            ("consecutive_failures", consecutive_tick_errors.to_string()),
                        ],
                    );
                    if consecutive_tick_errors >= MAX_CONSECUTIVE_TICK_ERRORS {
                        return Err(error);
                    }
                    thread::sleep(self.tick_interval);
                }
            }
        }
        let flushed = self.flush_for_shutdown(self.clock.now_system())?;
        logging::info(
            "Received shutdown signal; exiting daemon runtime loop cleanly",
            &[("intents_flushed", flushed.to_string())],
        );
        Ok(())
    }

    /// Whether anything is in flight or queued that justifies keeping the
    /// fast tick cadence. When this is false, the loop parks on the tick
    /// waker at the idle cadence instead.
    fn has_pending_work(&self, report: &RuntimeTickReport) -> bool {
        let report_saw_work = report.released_deferred_reconciles > 0
            || report.drained_pending_intents > 0
            || report.stabilized_events > 0
            || report.durable_enqueues > 0
            || report.leased_intents > 0
            || report.started_staged_intents > 0
            || report.completed_intents > 0
            || report.requeued_intents > 0
            || report.failed_intents > 0
            || report.remote_poll.enqueued_intents > 0;
        if report_saw_work
            || report.staged_executor.active_total > 0
            || self.running_reconcile_intent_id.is_some()
            || self.scheduler.pending_count() > 0
            || !self.folders_waiting_for_rules.is_empty()
            || !self.walks_waiting_for_rules.is_empty()
        {
            return true;
        }

        // A deferred scan is work to come back for only while it can be
        // released. Held, it would keep the loop at the working cadence
        // for as long as the hold lasts.
        let scans_can_be_released = self.deferred_scans_can_be_released();
        self.recorder
            .as_ref()
            .map(|recorder| {
                recorder.with_state(|maps| {
                    maps.pending_event_count() > 0
                        || maps.pending_intent_count() > 0
                        || (scans_can_be_released && maps.deferred_reconcile_count() > 0)
                })
            })
            .unwrap_or(false)
    }

    /// Whether a scan that was put off can start when its time comes:
    /// not during a pause, a hold of the profile, or a `Suspended`
    /// throttle.
    fn deferred_scans_can_be_released(&self) -> bool {
        let snapshot = self.app.snapshot();
        snapshot.run_state != RunState::Paused
            && snapshot.throttle_state != vapor_shared::ThrottleState::Suspended
            && self.cloud_root_ready
            && self.root_hold.is_none()
            && self.one_way_hold.is_none()
            && !self.sign_in_hold
    }

    pub fn tick(&mut self, now: SystemTime) -> Result<RuntimeTickReport, DaemonRuntimeError> {
        let inputs = self.metrics_sampler.sample();
        self.tick_with_inputs(now, inputs)
    }

    /// Attach a `RuntimeControl` so the runtime tick observes IPC-driven
    /// control requests (pause / resume / flush / reconcile). The
    /// `vapor` CLI commands write into this control through the IPC
    /// service. The control also gets the tick waker so an IPC request
    /// wakes the loop immediately instead of waiting out the sleep.
    pub fn attach_control(&mut self, control: Arc<crate::runtime_control::RuntimeControl>) {
        control.set_waker(self.tick_waker.clone());
        self.runtime_control = Some(control);
    }

    /// Attach a `StatusPublisher` so consumers like the IPC service see
    /// fresh `DaemonStatusSnapshot`s after every tick. Without this, the
    /// IPC `Status` endpoint would return whatever snapshot the
    /// publisher was constructed with, i.e. it would lie about
    /// pause/throttle changes the tick loop made.
    pub fn attach_status_publisher(&mut self, publisher: Arc<dyn StatusPublisher>) {
        // Publish the current state immediately so consumers don't wait
        // until the first post-startup tick to see anything other than
        // their construction-time snapshot.
        publisher.publish(DaemonStatusSnapshot::from_app(&self.app));
        self.status_publisher = Some(publisher);
    }

    pub fn tick_with_inputs(
        &mut self,
        now: SystemTime,
        throttle_inputs: ThrottleInputs,
    ) -> Result<RuntimeTickReport, DaemonRuntimeError> {
        // Every write of the tick is one write to the log of the state
        // database. What the tick did before an error or a panic stays
        // done, as it did when each statement was a transaction of its
        // own, and the batch never outlives the tick: a profile that is
        // suspended after a panic is composed again on this database.
        //
        // A commit that fails takes every write of the tick with it,
        // while the memory of the runtime moved on (the cursor of the
        // changes feed, the changes the scheduler handed over). That
        // error has a name of its own, and the caller ends this runtime.
        self.state_db.begin_batch()?;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let report = self.tick_in_batch(now, throttle_inputs);
            // After the last harvest of the tick and inside its batch:
            // a provider reports the work it owes for a write before
            // the transfer ends, so the record is saved with the sync
            // index row of that transfer. Also after a tick that
            // failed: what it harvested before the failure stays
            // written.
            let saved = crate::follow_ups::save(&mut self.state_db, self.app.provider(), now);
            let report = report?;
            saved?;
            Ok(report)
        }));
        let committed = self.state_db.commit_batch();
        match outcome {
            Ok(report) => {
                committed.map_err(DaemonRuntimeError::TickNotSaved)?;
                report
            }
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    fn tick_in_batch(
        &mut self,
        now: SystemTime,
        throttle_inputs: ThrottleInputs,
    ) -> Result<RuntimeTickReport, DaemonRuntimeError> {
        self.apply_pending_control_requests(now)?;
        self.sample_throttle_inputs(now, throttle_inputs);
        self.queue_whole_scope_scan_when_due(now)?;
        self.reload_path_filter_if_requested(now)?;
        self.sweep_stale_leases_if_due(now)?;
        self.purge_trash_if_due(now);
        self.retry_cloud_root_if_needed()?;
        self.clear_cloud_full_if_over(now);
        self.check_roots_if_due(now)?;
        self.apply_resolved_decisions(now)?;
        self.sync_conflict_decisions(now)?;
        if self.one_way_hold.is_some() {
            self.release_one_way_hold_if_acknowledged(now)?;
        }

        let (stabilized_events, suppressed_local_echoes, stabilize_mirror_reverts) =
            self.stabilize_events(now);

        // Pause semantics ("stops admitting new work"): ingest, debounce,
        // and the durable flush keep running so intent state is never
        // lost, and work already in flight runs to completion, but no
        // new work is released or leased while paused. An unavailable
        // cloud root blocks the same way.
        let paused = self.app.snapshot().run_state == RunState::Paused
            || !self.cloud_root_ready
            || self.root_hold.is_some()
            || self.one_way_hold.is_some()
            || self.sign_in_hold
            // The ignore files of the tree are not read yet: a name
            // they hide must not be polled, walked, or leased as work.
            || self
                .path_filter
                .as_ref()
                .is_some_and(|filter| !filter.rules_ready());
        self.mirror_revert_count += stabilize_mirror_reverts as u64;
        let mut report = RuntimeTickReport {
            released_deferred_reconciles: if paused {
                0
            } else {
                self.release_ready_deferred_reconciles(now)
            },
            drained_pending_intents: self.drain_pending_intents(),
            stabilized_events,
            suppressed_local_echoes,
            mirror_reverts: stabilize_mirror_reverts,
            durable_enqueues: self.flush_scheduler_to_durable_queue()?,
            ..RuntimeTickReport::default()
        };

        // Remote→local ingest: poll the provider changes feed on its
        // throttle-aware cadence and durably enqueue surviving changes.
        // Requires an ensured cloud root; a paused daemon also skips
        // polling (nothing would be leased anyway) but keeps every
        // already-enqueued intent durable.
        if self.cloud_root_ready && !paused {
            report.remote_poll = self.remote_poller.poll_if_due(
                &mut self.app,
                &mut self.state_db,
                crate::remote_sync::PollEnv {
                    remote_echoes: &mut self.remote_echoes,
                    local_echoes: &mut self.local_echoes,
                    tags: Some(&self.tags),
                    local_root: self.sync_scope.local_root.as_deref(),
                    path_filter: self.path_filter.as_deref(),
                    sync_mode: self.sync_scope.sync_mode,
                },
                &self.clock,
                now,
                &self.provider_call_mode,
            )?;
            report.mirror_reverts += report.remote_poll.mirror_reverts;
            report.mirror_deletes += report.remote_poll.mirror_deletes;
            report.moves += report.remote_poll.renamed_on_device;
            if report.remote_poll.observed_changes > 0 {
                // The cloud is busy: the next change is likely near.
                self.remote_poller.note_activity(self.clock.now());
            }
            if report.remote_poll.sign_in_refused
                && let Some(message) = self.remote_poller.take_refused_sign_in()
            {
                self.mark_sign_in_required(&message, now);
            }
            if report.remote_poll.name_collisions > 0
                && let Some(timeline) = &self.timeline
            {
                for (wanted, existing) in self.remote_poller.take_name_collisions() {
                    timeline.push(
                        constants::timeline::KIND_COLLISION,
                        self.profile_id.clone(),
                        name_collision_message(&wanted, &existing),
                        now,
                    );
                }
            }
            self.mirror_revert_count += report.remote_poll.mirror_reverts as u64;
            self.mirror_delete_count += report.remote_poll.mirror_deletes as u64;
        }

        // One view of the renames in flight for the whole tick.
        let mut move_hints = crate::executor::MoveHints::default();
        let local_changes_pending = self.scheduler.pending_count() > 0
            || self
                .recorder
                .as_ref()
                .is_some_and(|recorder| recorder.with_state(|maps| maps.pending_event_count() > 0));
        let remote_changes_pending = self.remote_poller.more_changes_expected();
        let staged_report = {
            let mut env = ExecutionEnv {
                local_root: self.sync_scope.local_root.as_deref(),
                cloud_root: Some(self.sync_scope.cloud_root.as_str()),
                sync_mode: self.sync_scope.sync_mode,
                device_id: &self.device_id,
                profile_id: &self.profile_id,
                hash_algorithm: self.app.provider().content_hash_algorithm(),
                bandwidth: &self.bandwidth,
                move_hints: &mut move_hints,
                tags: &self.tags,
                local_echoes: &mut self.local_echoes,
                remote_echoes: &mut self.remote_echoes,
                trash: self.trash.as_ref(),
                local_changes_pending,
                remote_changes_pending,
                path_filter: self.path_filter.as_deref(),
            };
            self.staged_executor
                .advance(&mut self.app, &mut self.state_db, &mut env, now)?
        };
        if staged_report.completed > 0 {
            // This device is syncing: another device may answer soon.
            self.remote_poller.note_activity(self.clock.now());
        }
        if staged_report.remote_change_expected {
            // An intent waits for the feed to say where a cloud object
            // went.
            self.remote_poller.request_immediate_poll();
        }
        report.completed_intents += staged_report.completed;
        report.requeued_intents += staged_report.retried;
        report.failed_intents += staged_report.failed;
        report.mirror_deletes += staged_report.mirror_deletes;
        self.mirror_delete_count += staged_report.mirror_deletes as u64;
        report.conflicts += staged_report.conflict_paths.len();
        self.conflict_count += staged_report.conflict_paths.len() as u64;
        report.conflict_paths.extend(staged_report.conflict_paths);
        report.moves += staged_report.moves;
        report.cloud_deletes.extend(staged_report.cloud_deletes);
        if staged_report.cloud_root_unavailable > 0 {
            self.mark_cloud_root_unavailable("a provider transfer reported the root missing", now);
        }
        if staged_report.local_root_suspect || staged_report.cloud_root_suspect {
            self.last_root_check_inst = None;
        }
        if staged_report.cloud_root_suspect {
            // The cloud side is checked at once too, not at its cadence.
            self.last_cloud_root_attempt_inst = None;
        }
        if let Some(message) = &staged_report.cloud_storage_full {
            self.mark_cloud_full(message, now);
        }
        self.reconcile_unsyncable_names(&staged_report.unsyncable_names, false, now)?;
        if let Some(message) = &staged_report.authentication_required {
            self.mark_sign_in_required(message, now);
        }

        if let Some(reconcile_intent_id) = self.running_reconcile_intent_id {
            // A running reconcile performs one bounded chunk of real
            // comparison work per tick, then answers to the controller's
            // slice/throttle checkpoint. The walker survives pauses so a
            // large tree converges across slices instead of restarting.
            match self.process_reconcile_walk(now) {
                Err(walk_error) => {
                    if let crate::reconcile_walk::WalkError::Provider(provider_error) = &walk_error
                        && provider_error.kind
                            == vapor_shared::ProviderErrorKind::CloudRootUnavailable
                    {
                        // Not a walk bug: the cloud root itself vanished.
                        // Block admission and let the ensure-retry loop
                        // recreate it instead of retrying the walk forever.
                        self.mark_cloud_root_unavailable(&provider_error.message, now);
                    } else if let crate::reconcile_walk::WalkError::Provider(provider_error) =
                        &walk_error
                        && provider_error.kind == vapor_shared::ProviderErrorKind::Authentication
                    {
                        self.mark_sign_in_required(&provider_error.message, now);
                    } else {
                        logging::warning(
                            "Reconcile comparison walk failed; yielding and retrying later",
                            &[("error", format!("{walk_error:?}"))],
                        );
                    }
                    self.reconcile_walker = None;
                    self.app.abort_reconcile(&mut self.scheduler, now);
                    self.requeue_runtime_intent(
                        reconcile_intent_id,
                        now + blocked_intent_requeue_delay(),
                        "reconcile comparison walk failed; will retry",
                    )?;
                    self.running_reconcile_intent_id = None;
                    report.requeued_intents += 1;
                }
                Ok(walk_done) => {
                    if let Some(walker) = self.reconcile_walker.as_mut() {
                        let (mirror_reverts, mirror_deletes) = walker.take_mirror_deltas();
                        report.mirror_reverts += mirror_reverts;
                        report.mirror_deletes += mirror_deletes;
                        self.mirror_revert_count += mirror_reverts as u64;
                        self.mirror_delete_count += mirror_deletes as u64;
                        let mismatches = walker.take_type_mismatches();
                        let unsyncable = walker.take_unsyncable_names();
                        let whole_scope = walker.is_whole_scope();
                        let collisions = walker.take_name_collisions();
                        // A folder that was not read was not compared:
                        // it is walked again later, sooner than the
                        // daily whole-scope scan.
                        let retry_at = now
                            + Duration::from_secs(
                                constants::engine::RECONCILE_UNREADABLE_RETRY_SECONDS,
                            );
                        let unreadable_dirs = walker.take_unreadable_dirs();
                        self.walks_waiting_for_rules
                            .extend(walker.take_dirs_waiting_for_rules());
                        // The root itself: nothing under it can sync,
                        // and status has to say why.
                        if let Some((root, kind)) = unreadable_dirs.iter().find(|(directory, _)| {
                            self.sync_scope.local_root.as_ref() == Some(directory)
                        }) {
                            let reason = if *kind == std::io::ErrorKind::PermissionDenied {
                                format!(
                                    "cannot read {}: permission denied; {}",
                                    root.display(),
                                    constants::FOLDER_ACCESS_HINT
                                )
                            } else {
                                format!("cannot read {}: {kind}", root.display())
                            };
                            if self.app.snapshot().run_state != RunState::Paused {
                                self.app.set_run_state(RunState::Error, reason.clone());
                            }
                            self.local_root_unreadable = Some(reason);
                            self.local_root_unreadable_this_walk = true;
                        }
                        // A folder that is gone from this device is not
                        // one that cannot be read: there is nothing to
                        // wait for. Its walk runs now and ends at once
                        // when the cloud has none either, in place of a
                        // marker that sits in the queue for minutes.
                        let unreadable: Vec<_> = unreadable_dirs
                            .into_iter()
                            .map(|(directory, _)| {
                                // Only "no such folder" means gone. A
                                // folder that cannot be looked at (no
                                // permission on the way to it) is the
                                // other case, and its walk waits.
                                let gone =
                                    std::fs::symlink_metadata(&directory).is_err_and(|error| {
                                        error.kind() == std::io::ErrorKind::NotFound
                                    });
                                let at = if gone { now } else { retry_at };
                                (directory, PendingIntentKind::ReconcileSubtree, at)
                            })
                            .collect();
                        if !unreadable.is_empty() {
                            self.state_db.enqueue_intents_coalesced(
                                &unreadable,
                                crate::safeguards::IntentSource::ReconcileBacklog,
                            )?;
                            logging::info(
                                "Queued another walk of the folders that could not be read",
                                &[("folders", unreadable.len().to_string())],
                            );
                        }
                        // The walk renamed these on this device, for a
                        // name or a place that the cloud changed. The
                        // watcher reports each one as the old path
                        // leaving and the new one coming.
                        for (old_path, new_path) in walker.take_renamed() {
                            self.local_echoes.record_rename(&old_path, &new_path, now);
                            report.moves += 1;
                        }
                        for path in mismatches {
                            self.open_type_mismatch_decision(&path, now)?;
                        }
                        // Each slice of a walk asks about the names it
                        // met. Which questions to withdraw is known only
                        // when the whole scope was walked: a slice saw a
                        // part of the tree, and a name it did not see
                        // is not a name that is gone.
                        self.reconcile_unsyncable_names(&unsyncable, false, now)?;
                        if whole_scope {
                            self.unsyncable_seen_by_walk.extend(unsyncable);
                        }
                        if let Some(timeline) = &self.timeline {
                            for (wanted, existing) in collisions {
                                timeline.push(
                                    constants::timeline::KIND_COLLISION,
                                    self.profile_id.clone(),
                                    name_collision_message(&wanted, &existing),
                                    now,
                                );
                            }
                        }
                    }
                    if walk_done {
                        // A finished walk completes regardless of the slice
                        // checkpoint: there is nothing left to slice, and
                        // yielding here would re-lease the done walk every
                        // tick (spinning forever under a coarse clock while
                        // holding the startup barrier up and starving every
                        // non-reconcile intent behind it).
                        if let Some(completed_root) = self.complete_running_reconcile()? {
                            let ignored = self
                                .reconcile_walker
                                .as_mut()
                                .map(|walker| walker.take_ignored_local_entries());
                            self.reconcile_walker = None;
                            self.state_db.complete_leased(reconcile_intent_id)?;
                            self.running_reconcile_intent_id = None;
                            let seen = std::mem::take(&mut self.unsyncable_seen_by_walk);
                            if self.sync_scope.local_root.as_ref() == Some(&completed_root) {
                                self.reconcile_unsyncable_names(&seen, true, now)?;
                                self.last_whole_scope_scan_at = Some(now);
                                self.ignored_paths = ignored;
                                // A whole-scope walk that ended without
                                // reporting the root unreadable read it.
                                if !std::mem::take(&mut self.local_root_unreadable_this_walk)
                                    && self.local_root_unreadable.take().is_some()
                                {
                                    self.restore_run_state_after_root_recovery(
                                        "local root is readable again",
                                    );
                                }
                                if std::mem::take(&mut self.whole_scope_rescan_wanted) {
                                    // The merge flag stays for the walk
                                    // that follows: the request may be
                                    // the one that set it.
                                    self.state_db
                                        .enqueue_startup_reconcile_intent(&completed_root, now)?;
                                    logging::info(
                                        "A whole-scope reconcile was requested while one walked; walking once more",
                                        &[("root", completed_root.display().to_string())],
                                    );
                                } else {
                                    // The merge flag covers one whole-scope walk.
                                    self.state_db.delete_state(
                                        constants::state::MERGE_WITHOUT_DELETIONS_KEY,
                                    )?;
                                }
                                if self.startup_reconstruction_barrier {
                                    self.startup_reconstruction_barrier = false;
                                    self.startup_barrier_expires_inst = None;
                                }
                            }
                            report.completed_intents += 1;
                            report.completed_reconcile_root = Some(completed_root);
                        } else {
                            // The controller lost the running reconcile:
                            // abort defensively so the durable intent can
                            // retry instead of wedging leased.
                            self.reconcile_walker = None;
                            self.app.abort_reconcile(&mut self.scheduler, now);
                            self.requeue_runtime_intent(
                                reconcile_intent_id,
                                now + blocked_intent_requeue_delay(),
                                "reconcile completed its walk without a controller; will retry",
                            )?;
                            self.running_reconcile_intent_id = None;
                            report.requeued_intents += 1;
                        }
                    } else if self
                        .app
                        .checkpoint_reconcile(&mut self.scheduler, now)
                        .is_some()
                    {
                        // A walk that used its slice on the tick thread
                        // gives the next tick to other work. One that
                        // waits for a listing used none of it: it is
                        // due at once, so the tick that the listing
                        // wakes goes on with the walk. With a wait for
                        // the timer of the loop after each slice, a
                        // walk of a cloud API took a second a folder.
                        //
                        // A scan holds a planner slot and a read token.
                        // It takes them back in this tick only when
                        // that leaves one of each for other work: in a
                        // throttle state with one read token, a walk
                        // that never let go would keep every hash, and
                        // so every upload, waiting until it ends.
                        let waits_for_a_listing = self
                            .reconcile_walker
                            .as_ref()
                            .is_some_and(|walker| walker.waits_for_a_listing());
                        let gate = self.app.workgate_snapshot();
                        let leaves_room = gate.available_planner_workers() > 1
                            && gate.available_read_tokens() > 1;
                        let due = if waits_for_a_listing && leaves_room {
                            now
                        } else {
                            now + self.tick_interval
                        };
                        self.requeue_runtime_intent(
                            reconcile_intent_id,
                            due,
                            "reconcile yielded for next safe slice",
                        )?;
                        self.running_reconcile_intent_id = None;
                        report.requeued_intents += 1;
                    }
                }
            }
        }

        if !paused {
            let started_intent_ids = self.process_ready_queue(now, &mut report)?;
            if !started_intent_ids.is_empty() {
                // Freshly-leased intents start planning in their lease
                // tick: without this pass every file would pay one full
                // tick of dead latency between admission and its planner.
                let mut env = ExecutionEnv {
                    local_root: self.sync_scope.local_root.as_deref(),
                    cloud_root: Some(self.sync_scope.cloud_root.as_str()),
                    sync_mode: self.sync_scope.sync_mode,
                    device_id: &self.device_id,
                    profile_id: &self.profile_id,
                    hash_algorithm: self.app.provider().content_hash_algorithm(),
                    bandwidth: &self.bandwidth,
                    move_hints: &mut move_hints,
                    tags: &self.tags,
                    local_echoes: &mut self.local_echoes,
                    remote_echoes: &mut self.remote_echoes,
                    trash: self.trash.as_ref(),
                    local_changes_pending,
                    remote_changes_pending,
                    path_filter: self.path_filter.as_deref(),
                };
                let mut admission_report = crate::executor::StagedExecutorReport::default();
                self.staged_executor.advance_intents(
                    &mut self.app,
                    &mut self.state_db,
                    &mut env,
                    now,
                    &started_intent_ids,
                    &mut admission_report,
                )?;
                if admission_report.remote_change_expected {
                    self.remote_poller.request_immediate_poll();
                }
                report.completed_intents += admission_report.completed;
                report.requeued_intents += admission_report.retried;
                report.failed_intents += admission_report.failed;
                report.mirror_deletes += admission_report.mirror_deletes;
                self.mirror_delete_count += admission_report.mirror_deletes as u64;
                report.conflicts += admission_report.conflict_paths.len();
                self.conflict_count += admission_report.conflict_paths.len() as u64;
                report
                    .conflict_paths
                    .extend(admission_report.conflict_paths.iter().cloned());
                report.moves += admission_report.moves;
                report
                    .cloud_deletes
                    .extend(admission_report.cloud_deletes.iter().cloned());
                if let Some(timeline) = &self.timeline {
                    for (wanted, existing) in &admission_report.name_collisions {
                        timeline.push(
                            constants::timeline::KIND_COLLISION,
                            self.profile_id.clone(),
                            name_collision_message(wanted, existing),
                            now,
                        );
                    }
                }
                if admission_report.cloud_root_unavailable > 0 {
                    self.mark_cloud_root_unavailable(
                        "a provider transfer reported the root missing",
                        now,
                    );
                }
                if admission_report.local_root_suspect || admission_report.cloud_root_suspect {
                    self.last_root_check_inst = None;
                }
                if admission_report.cloud_root_suspect {
                    self.last_cloud_root_attempt_inst = None;
                }
                if let Some(message) = &admission_report.cloud_storage_full {
                    self.mark_cloud_full(message, now);
                }
                self.reconcile_unsyncable_names(&admission_report.unsyncable_names, false, now)?;
                if let Some(message) = &admission_report.authentication_required {
                    self.mark_sign_in_required(message, now);
                }
            }
        }

        report.staged_executor = self.staged_executor.snapshot();
        self.emit_timeline_events(&report, now);

        if let Some(publisher) = self.status_publisher.as_ref() {
            publisher.publish(DaemonStatusSnapshot::from_app(&self.app));
        }

        Ok(report)
    }

    /// Whether a reconcile walk has to wait for the changes feed: the
    /// provider has a feed, a poll may run now, and the poller has no
    /// cursor that a poll confirmed. Without a feed, or while no poll
    /// may run, there is nothing to wait for.
    fn walk_waits_for_cursor(&mut self) -> bool {
        self.remote_poller.awaits_cursor()
            && self.cloud_root_ready
            && self.sync_scope.local_root.is_some()
            && self
                .app
                .provider()
                .capabilities()
                .supports_remote_changes_feed
            && self.app.remote_poll_allowed()
    }

    /// Moves an ignore-rule rebuild forward (an ignore file changed, or
    /// a rule key in `vapor.json`). The tree walk and the compilation
    /// run on a worker; the rules in force stay until the new ones are
    /// in. When the new rules decide a path another way, a whole-scope
    /// reconcile follows: a file the old rules hid is in scope now, and
    /// nothing else would notice it until it changes.
    fn reload_path_filter_if_requested(
        &mut self,
        now: SystemTime,
    ) -> Result<(), DaemonRuntimeError> {
        let Some(path_filter) = self.path_filter.clone() else {
            return Ok(());
        };
        path_filter.drive_rebuild(&self.provider_call_mode);
        // Profiles with one local root share the filter, and any of
        // them can be the one whose call puts a rebuild in force. So
        // each one reads the number of the last rebuild that landed. A
        // rebuild that failed lands nothing: the folders keep waiting,
        // and the filter tries again.
        let landed = path_filter.rebuilds_landed();
        if landed != self.seen_rules_landed {
            self.seen_rules_landed = landed;
            for (folder, observed_at) in std::mem::take(&mut self.folders_waiting_for_rules) {
                let reported = self.report_subtree_events(&folder, observed_at);
                logging::info(
                    "Reported the children of a directory that appeared with ignore rules of its own",
                    &[
                        ("path", folder.display().to_string()),
                        ("files", reported.to_string()),
                    ],
                );
            }
            let mut walks = std::mem::take(&mut self.walks_waiting_for_rules);
            walks.sort();
            walks.dedup();
            if !walks.is_empty() {
                let batch: Vec<_> = walks
                    .into_iter()
                    .map(|folder| (folder, PendingIntentKind::ReconcileSubtree, now))
                    .collect();
                self.state_db.enqueue_intents_coalesced(
                    &batch,
                    crate::safeguards::IntentSource::ReconcileBacklog,
                )?;
                logging::info(
                    "Queued a walk of the folders that waited for their ignore rules",
                    &[("folders", batch.len().to_string())],
                );
            }
        }
        let generation = path_filter.rules_generation();
        if generation != self.seen_rules_generation {
            self.seen_rules_generation = generation;
            logging::info(
                "The ignore rules changed; a whole-scope reconcile syncs what they freed",
                &[],
            );
            self.enqueue_startup_reconstruction_reconcile(now)?;
        }
        Ok(())
    }

    /// Keeps one `conflict` decision per conflicted file, listing every
    /// version: a file with copies gets its question, a question whose
    /// versions changed is rewritten in place, and a question with no
    /// copy left (resolved in the Finder, on another device, or through
    /// `vapor decisions resolve`) closes. Runs on its own cadence, and
    /// not while the throttle is `Suspended`.
    fn sync_conflict_decisions(&mut self, now: SystemTime) -> Result<(), DaemonRuntimeError> {
        let now_inst = self.clock.now();
        let interval = Duration::from_secs(constants::conflict::DECISION_SYNC_SECONDS);
        let due = self
            .last_conflict_sync_inst
            .map(|last| now_inst.saturating_duration_since(last) >= interval)
            .unwrap_or(true);
        if !due || self.app.snapshot().throttle_state == vapor_shared::ThrottleState::Suspended {
            return Ok(());
        }
        self.last_conflict_sync_inst = Some(now_inst);

        let originals: std::collections::BTreeSet<PathBuf> = self
            .state_db
            .conflict_copy_paths()?
            .iter()
            .filter(|copy| copy.is_file())
            .filter_map(|copy| crate::conflict::original_path(copy))
            .collect();
        let trash_enabled = self
            .trash
            .as_ref()
            .is_none_or(|trash| trash.settings().enabled);
        let mut asked = std::collections::BTreeSet::new();
        for decision in self.state_db.decisions(false)? {
            if decision.kind != constants::decision::KIND_CONFLICT {
                continue;
            }
            let Some(original) = decision.path.clone() else {
                continue;
            };
            let conflict = match crate::conflict::Conflict::find(&original) {
                Ok(found) => found,
                Err(error) => {
                    // Cannot look (an unplugged drive): the question
                    // stands until the folder can be read again.
                    logging::debug(
                        "Cannot list a conflicted file's folder; leaving its question as it is",
                        &[
                            ("decision_id", decision.id.to_string()),
                            ("error", error.to_string()),
                        ],
                    );
                    asked.insert(original);
                    continue;
                }
            };
            let Some(conflict) = conflict else {
                self.withdraw_decision(decision.id, now)?;
                logging::info(
                    "Closed a conflict question: no copy is left",
                    &[("decision_id", decision.id.to_string())],
                );
                continue;
            };
            let fresh = crate::conflict::question(&conflict, &self.device_id, trash_enabled);
            if fresh.options != decision.options
                || fresh.question != decision.question
                || fresh.evidence != decision.evidence
            {
                self.state_db.update_open_decision(
                    decision.id,
                    &fresh.question,
                    &fresh.options,
                    &fresh.evidence,
                )?;
            }
            asked.insert(conflict.original);
        }
        // An answer the CLI is applying keeps its versions for a moment;
        // asking again would duplicate the question.
        for decision in self.state_db.resolved_unapplied_decisions()? {
            if decision.kind == constants::decision::KIND_CONFLICT
                && let Some(original) = decision.path
            {
                asked.insert(original);
            }
        }

        let mut opened = Vec::new();
        for original in originals.difference(&asked) {
            // A keep-both moves this device's edit aside before the
            // cloud's version lands under the name; ask once both are
            // there. While the name is still empty, a copy made by
            // answering a file/folder clash with `keep-both` is that
            // answer (the folder is on its way), not a question.
            if !original.exists() {
                let landing = !self.state_db.queued_kinds_at(original)?.is_empty();
                let kept_both = self.state_db.decision_answered(
                    crate::type_mismatch::DECISION_KIND,
                    original,
                    crate::type_mismatch::OPTION_KEEP_BOTH,
                )?;
                if landing || kept_both {
                    continue;
                }
            }
            let Ok(Some(conflict)) = crate::conflict::Conflict::find(original) else {
                continue;
            };
            // With no file under the original name, only a copy that
            // this device made is a conflict to ask about. A file that
            // came with such a name from elsewhere (an export, another
            // tool, a device whose original is long gone) is a file.
            if conflict.versions.iter().all(|version| {
                version
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(crate::conflict::parse_conflict_copy_name)
                    .is_some_and(|copy| copy.device_id != self.device_id)
            }) {
                continue;
            }
            let asking = crate::conflict::question(&conflict, &self.device_id, trash_enabled);
            opened.push(self.state_db.create_decision(
                constants::decision::KIND_CONFLICT,
                crate::state_db::DecisionScope::Path,
                Some(original),
                &asking.question,
                &asking.options,
                &asking.evidence,
                now,
            )?);
        }
        if !opened.is_empty() {
            logging::info(
                "Asked which version of a conflicted file stays",
                &[
                    ("profile_id", self.profile_id.clone()),
                    ("questions", opened.len().to_string()),
                ],
            );
            self.announce_decisions(&opened, now);
        }
        Ok(())
    }

    /// Drops managed-trash entries past their retention, at startup and
    /// then on a slow cadence. A directory scan of the trash, cheap at
    /// the cadence and only while the daemon is otherwise ticking.
    fn purge_trash_if_due(&mut self, now: SystemTime) {
        if let Some(purged) = self.trash_purge.as_mut().and_then(|call| call.take()) {
            self.trash_purge = None;
            match purged {
                Ok(0) => {}
                Ok(purged) => logging::info(
                    "Purged expired trash entries",
                    &[
                        ("profile_id", self.profile_id.clone()),
                        ("purged", purged.to_string()),
                    ],
                ),
                Err(panic) => logging::warning(
                    "The trash purge failed; it runs again at the next interval",
                    &[("error", panic)],
                ),
            }
        }
        let Some(trash) = &self.trash else {
            return;
        };
        let now_inst = self.clock.now();
        let interval = Duration::from_secs(constants::trash::PURGE_INTERVAL_SECONDS);
        let due = self
            .last_trash_purge_inst
            .map(|last| now_inst.saturating_duration_since(last) >= interval)
            .unwrap_or(true);
        if !due || self.trash_purge.is_some() {
            return;
        }
        self.last_trash_purge_inst = Some(now_inst);
        // Removing a discarded tree is disk work that grows with the
        // tree: it runs on a worker, and a later tick logs the count.
        self.trash_purge = Some(crate::provider_jobs::ProviderCall::start(
            &self.provider_call_mode,
            "trash-purge",
            trash.purge_job(now),
        ));
    }

    /// Periodic in-run recovery of leases that exceeded the lease
    /// timeout (an orphaned execution). Startup recovery handles dead
    /// processes; this sweep handles a lease lost *within* a live run.
    fn sweep_stale_leases_if_due(&mut self, now: SystemTime) -> Result<(), DaemonRuntimeError> {
        let now_inst = self.clock.now();
        let due = self
            .last_stale_lease_sweep_inst
            .map(|last| now_inst.saturating_duration_since(last) >= self.stale_lease_sweep_interval)
            .unwrap_or(true);
        if !due {
            return Ok(());
        }

        self.last_stale_lease_sweep_inst = Some(now_inst);
        // Renew the leases the executor still holds so an in-flight large
        // transfer (or a Suspended stall longer than the lease timeout) is
        // not reclaimed as "orphaned" and duplicated.
        let mut live_ids = self.staged_executor.active_intent_ids();
        if let Some(reconcile_id) = self.running_reconcile_intent_id {
            live_ids.push(reconcile_id);
        }
        self.state_db.renew_leases(&live_ids, now)?;
        let recovered = self.state_db.recover_stale_leases(now)?;
        if recovered > 0 {
            logging::warning(
                "Recovered stale leases during in-run sweep",
                &[("recovered_leases", recovered.to_string())],
            );
        }
        Ok(())
    }

    pub fn app(&self) -> &DaemonApp {
        &self.app
    }

    #[cfg(test)]
    pub(crate) fn state_db_mut(&mut self) -> &mut DurableStateDb {
        &mut self.state_db
    }

    pub fn state_db(&self) -> &DurableStateDb {
        &self.state_db
    }

    pub fn sync_scope(&self) -> &SyncScope {
        &self.sync_scope
    }

    pub fn has_live_watcher(&self) -> bool {
        self.watcher.is_some()
    }

    /// The bounded event recorder this runtime ingests from. The
    /// multi-profile runtime fans deduplicated watcher callbacks into
    /// these per-profile recorders.
    pub(crate) fn event_recorder(&self) -> Option<Arc<BoundedFsEventRecorder>> {
        self.recorder.clone()
    }

    /// This scope's ignore-rule filter (present whenever a local sync
    /// directory is configured). The multi-profile runtime keys these
    /// by canonical root to share one instance per watched directory.
    pub(crate) fn shared_path_filter(
        &self,
    ) -> Option<Arc<crate::fs_events::SharedEventPathFilter>> {
        self.path_filter.clone()
    }

    /// Replaces this runtime's filter with a shared per-root instance so
    /// profiles watching the same directory, and the deduplicated
    /// watcher feeding them: reload ignore-rule changes together.
    pub(crate) fn adopt_shared_path_filter(
        &mut self,
        path_filter: Arc<crate::fs_events::SharedEventPathFilter>,
    ) {
        self.path_filter = Some(path_filter);
    }

    /// Whether any queued or in-flight work justifies the fast tick
    /// cadence: the multi-profile loop aggregates this across
    /// profiles.
    pub(crate) fn profile_has_pending_work(&self, report: &RuntimeTickReport) -> bool {
        self.has_pending_work(report)
    }

    /// Schedules the startup whole-scope reconcile (exposed for the
    /// multi-profile composition, which builds runtimes watcher-less).
    pub(crate) fn schedule_startup_reconcile(
        &mut self,
        now: SystemTime,
    ) -> Result<(), DaemonRuntimeError> {
        self.enqueue_startup_reconstruction_reconcile(now)
    }

    #[cfg(test)]
    pub(crate) fn replace_provider_for_testing(
        &mut self,
        provider: Box<dyn vapor_providers::Provider>,
    ) {
        self.app.replace_provider_for_testing(provider);
    }

    fn build(
        sync_scope: SyncScope,
        filter_options: EventPathFilterOptions,
        state_db: DurableStateDb,
        provider: Box<dyn Provider>,
        metrics_sampler: Arc<dyn MetricsSampler>,
        clock: SharedClock,
        start_watcher: bool,
    ) -> Result<Self, DaemonRuntimeError> {
        let app = DaemonApp::new_with_clock(provider, clock.clone());
        Self::build_with_app(
            sync_scope,
            filter_options,
            state_db,
            app,
            metrics_sampler,
            clock,
            Startup::Standalone { start_watcher },
        )
    }

    /// Composition seam for the multi-profile runtime: the caller
    /// builds the `DaemonApp` (possibly around a shared workgate) and
    /// manages watchers itself.
    pub(crate) fn build_with_app(
        mut sync_scope: SyncScope,
        filter_options: EventPathFilterOptions,
        mut state_db: DurableStateDb,
        mut app: DaemonApp,
        metrics_sampler: Arc<dyn MetricsSampler>,
        clock: SharedClock,
        startup: Startup,
    ) -> Result<Self, DaemonRuntimeError> {
        let start_watcher = startup
            == Startup::Standalone {
                start_watcher: true,
            };
        let now = clock.now_system();
        let recovered_count = state_db.recover_leased(now)?;
        match state_db.prune_failed_intents(now) {
            Ok(pruned) if pruned > 0 => logging::info(
                "Pruned terminally-failed intents past the retention window / cap",
                &[("pruned", pruned.to_string())],
            ),
            Ok(_) => {}
            Err(error) => logging::warning(
                "Failed-intent pruning failed; continuing",
                &[("error", error.to_string())],
            ),
        }
        logging::info(
            "Durable queue/state DB is ready",
            &[
                ("database_path", state_db.path().display().to_string()),
                ("recovered_leased_intents", recovered_count.to_string()),
            ],
        );

        if let Some(slowdown_until) = app.restore_retry_slowdown(&mut state_db, now)? {
            logging::warning(
                "Restored retry slowdown window from durable state",
                &[("retry_slowdown_until", format!("{:?}", slowdown_until))],
            );
        }
        // The provider of this run starts with nothing in memory: it
        // gets the work that the last run stored and did not end.
        crate::follow_ups::resume(&mut state_db, app.provider())?;

        let device_id = vapor_shared::device_id::for_process();
        let mut local_replacement: Option<Option<String>> = None;
        if let Some(local_root) = sync_scope.local_root.as_deref() {
            match crate::root_identity::check_local_root(
                &mut state_db,
                local_root,
                &device_id,
                now,
            )? {
                crate::root_identity::RootStatus::Ready => {}
                crate::root_identity::RootStatus::Missing => {
                    return Err(DaemonRuntimeError::LocalRootMissing(
                        local_root.to_path_buf(),
                    ));
                }
                crate::root_identity::RootStatus::Replaced { found, .. } => {
                    local_replacement = Some(found);
                }
                crate::root_identity::RootStatus::Unreachable(_) => {}
            }
        }
        sync_scope.local_root = sync_scope
            .local_root
            .take()
            .map(normalize_watch_root)
            .transpose()?;
        if let Some(local_root) = sync_scope.local_root.as_deref() {
            vapor_shared::paths::note_sync_root(local_root);
        }
        // The provider of this run knows no id yet: it gets the ids of
        // the synced files and folders, so its feed can say which path
        // a rename or a deletion is about.
        crate::remote_ids::restore(&state_db, app.provider(), sync_scope.local_root.as_deref())?;

        // The cloud root: adopted on first contact, checked against the
        // recorded identity afterwards. A runtime on its own probes it
        // here; a profile of the daemon leaves the first probe to the
        // first tick, on a worker like every later one. A runtime with
        // no local root (a parked profile) never touches the cloud.
        let first_cloud_probe_pending =
            startup == Startup::DaemonProfile && sync_scope.local_root.is_some();
        let cloud_outcome = if first_cloud_probe_pending {
            CloudRootOutcome::Unreachable("not probed yet".to_string())
        } else if sync_scope.local_root.is_some() {
            probe_cloud_root(
                app.provider_handle(),
                sync_scope.cloud_root.as_str(),
                crate::root_identity::recorded_identity(
                    &state_db,
                    crate::root_identity::RootSide::Cloud,
                )?,
                &device_id,
            )
        } else {
            CloudRootOutcome::Missing
        };
        let mut cloud_replacement: Option<Option<String>> = None;
        let mut cloud_missing = false;
        let mut startup_sign_in_hold: Option<String> = None;
        let cloud_root_ready = match cloud_outcome {
            CloudRootOutcome::Ready { adopted } => {
                if let Some(identity) = adopted {
                    crate::root_identity::record_identity(
                        &mut state_db,
                        crate::root_identity::RootSide::Cloud,
                        &identity,
                        now,
                    )?;
                }
                true
            }
            CloudRootOutcome::Replaced { found } => {
                cloud_replacement = Some(found);
                true
            }
            CloudRootOutcome::Missing => {
                cloud_missing = sync_scope.local_root.is_some();
                false
            }
            CloudRootOutcome::Unreachable(_) => false,
            CloudRootOutcome::SignInRequired(message) => {
                startup_sign_in_hold = Some(message);
                false
            }
        };

        let tick_waker = Arc::new(TickWaker::default());
        let recorder = sync_scope
            .local_root
            .as_ref()
            .map(|watch_root| Arc::new(BoundedFsEventRecorder::new(watch_root.clone())));
        // Built even when no watcher starts (multi-profile, tests): the
        // reconcile walk and remote-change mapping filter through it, so
        // remote-side ingest honors the same rules as local ingest.
        let path_filter = sync_scope.local_root.as_ref().map(|watch_root| {
            Arc::new(crate::fs_events::SharedEventPathFilter::new(
                watch_root,
                filter_options,
            ))
        });
        let watcher = if start_watcher {
            match (&sync_scope.local_root, &recorder, &path_filter) {
                (Some(watch_root), Some(recorder), Some(path_filter)) => {
                    if native_watcher_available() {
                        Some(FsEventsWatcher::start_with_shared_filter(
                            watch_root.clone(),
                            Arc::new(NotifyingRecorder {
                                inner: recorder.clone(),
                                waker: tick_waker.clone(),
                            }),
                            path_filter.clone(),
                        )?)
                    } else {
                        // A missing capability is not a startup failure: the
                        // runtime comes up without a watcher and the run
                        // state below says so. A native watcher that exists
                        // but fails to start still aborts startup.
                        logging::warning(
                            "No native filesystem watcher on this host; local changes are not detected until one ships",
                            &[("watch_root", watch_root.display().to_string())],
                        );
                        None
                    }
                }
                _ => None,
            }
        } else {
            None
        };

        if first_cloud_probe_pending {
            app.set_run_state(
                RunState::Starting,
                format!("checking the cloud folder {}", sync_scope.cloud_root),
            );
        } else if !cloud_root_ready {
            app.set_run_state(
                RunState::Error,
                format!(
                    "cloud root {} is unavailable; sync work is blocked until it can be ensured",
                    sync_scope.cloud_root
                ),
            );
        } else if let Some(local_root) = &sync_scope.local_root {
            if start_watcher && watcher.is_none() {
                app.set_run_state(
                    RunState::Error,
                    format!(
                        "no native filesystem watcher on this host yet; local changes under {} are not detected",
                        local_root.display()
                    ),
                );
            } else {
                app.set_run_state(
                    RunState::Running,
                    format!("watching {}", local_root.display()),
                );
            }
        } else {
            app.set_run_state(RunState::Paused, "no local root configured");
        }

        let debounce =
            DebounceLoop::with_windows_and_clock(DebounceWindows::default(), clock.clone());
        let tick_interval = debounce.tick_interval();
        let mut runtime = Self {
            app,
            sync_scope,
            state_db,
            recorder,
            debounce,
            staged_executor: StagedExecutor::with_clock(clock.clone()),
            scheduler: KeyedSupersedingScheduler::default(),
            watcher,
            path_filter,
            metrics_sampler,
            clock,
            tick_waker,
            runtime_control: None,
            status_publisher: None,
            last_throttle_sample_inst: None,
            last_stale_lease_sweep_inst: None,
            last_whole_scope_scan_at: None,
            unsyncable_seen_by_walk: Vec::new(),
            local_root_unreadable: None,
            local_root_unreadable_this_walk: false,
            running_reconcile_intent_id: None,
            reconcile_wait_reason: None,
            startup_reconstruction_barrier: false,
            startup_barrier_expires_inst: None,
            tick_interval,
            idle_tick_interval: Duration::from_millis(constants::engine::IDLE_TICK_MILLIS),
            throttle_sample_interval: Duration::from_millis(
                constants::engine::THROTTLE_SAMPLE_INTERVAL_MILLIS,
            ),
            stale_lease_sweep_interval: Duration::from_millis(
                constants::engine::STALE_LEASE_SWEEP_INTERVAL_MILLIS,
            ),
            startup_barrier_deadline: Duration::from_millis(
                constants::engine::STARTUP_RECONSTRUCTION_BARRIER_DEADLINE_MILLIS,
            ),
            tags: OpIdTagStore::new(Arc::new(NativeFilesystemCapabilities::for_current_host())),
            local_echoes: SelfWriteCache::new(),
            remote_echoes: SelfWriteCache::new(),
            remote_poller: RemotePoller::new(DEFAULT_PROFILE_ID),
            provider_call_mode: crate::provider_jobs::ProviderCallMode::Inline,
            cloud_root_call: None,
            root_hold: None,
            one_way_hold: None,
            last_root_check_inst: None,
            reconcile_after_root_check: false,
            profile_id: DEFAULT_PROFILE_ID.to_string(),
            timeline_default_entries: None,
            cloud_root_ready,
            local_root_changed: false,
            seen_rules_generation: 0,
            seen_rules_landed: 0,
            ignored_paths: None,
            cloud_full: None,
            user_paused: false,
            first_cloud_probe_pending,
            last_cloud_root_attempt_inst: None,
            sign_in_hold: false,
            reconcile_walker: None,
            whole_scope_rescan_wanted: false,
            folders_waiting_for_rules: Vec::new(),
            walks_waiting_for_rules: Vec::new(),
            mirror_revert_count: 0,
            mirror_delete_count: 0,
            conflict_count: 0,
            timeline: None,
            resource_budget: Arc::new(Mutex::new(crate::resource_budget::ResourceBudget::new(
                crate::resource_budget::EffectiveBudgetConfig::resolve(
                    &vapor_shared::config::VaporConfig::default(),
                ),
            ))),
            bandwidth: Arc::default(),
            idle_notifier: Arc::new(vapor_platform::AlwaysIdleNotifier),
            latest_ceilings: None,
            last_sampled_inputs: None,
            last_timeline_run_state: None,
            last_timeline_throttle: None,
            device_id,
            trash: None,
            last_trash_purge_inst: None,
            trash_purge: None,
            last_conflict_sync_inst: None,
            decision_changes: 0,
            decision_retries: std::collections::HashMap::new(),
            reconcile_boost_until_inst: None,
        };
        if let Some(found) = local_replacement {
            runtime.hold_for_replaced_root(crate::root_identity::RootSide::Local, found, now)?;
        } else if let Some(found) = cloud_replacement {
            runtime.hold_for_replaced_root(crate::root_identity::RootSide::Cloud, found, now)?;
        } else if cloud_missing {
            runtime.hold_for_missing_root(crate::root_identity::RootSide::Cloud, now)?;
        } else if let Some(message) = startup_sign_in_hold {
            runtime.mark_sign_in_required(&message, now);
        }
        runtime.hold_until_one_way_is_acknowledged(now)?;
        runtime.recall_uploads_that_wait_for_room()?;
        if !cloud_root_ready && !first_cloud_probe_pending {
            runtime.last_cloud_root_attempt_inst = Some(runtime.clock.now());
        }
        Ok(runtime)
    }

    /// Whether the profile waits for a local root that is not there.
    /// No watcher can start on such a folder, and the runtime says
    /// when it is back ([`Self::take_local_root_changed`]).
    pub(crate) fn waits_for_its_local_root(&self) -> bool {
        self.root_hold
            .as_ref()
            .is_some_and(|hold| hold.missing && hold.side == crate::root_identity::RootSide::Local)
    }

    /// Whether the folder at the local root changed since the last
    /// call; the caller that owns the watchers starts them again.
    pub(crate) fn take_local_root_changed(&mut self) -> bool {
        std::mem::take(&mut self.local_root_changed)
    }

    /// Wires the shared daemon activity timeline in.
    pub fn attach_timeline(&mut self, timeline: Arc<crate::timeline::TimelineBuffer>) {
        self.timeline = Some(timeline);
    }

    /// Switches this runtime's provider I/O (probes, transfer sessions,
    /// remote deletes) onto worker threads so provider RTT never stalls
    /// the tick loop. Production only: tests keep the deterministic
    /// inline mode. `waker` is notified on every completed job so the
    /// tick loop harvests promptly.
    pub fn enable_transfer_workers(&mut self, waker: Option<Arc<TickWaker>>) {
        self.staged_executor.enable_worker_threads(waker.clone());
        self.provider_call_mode = crate::provider_jobs::ProviderCallMode::Threaded { waker };
    }

    /// Attaches the profile's trash. Every local removal the engine
    /// performs from then on goes through it.
    pub fn attach_trash(&mut self, trash: crate::trash::LocalTrash) {
        self.trash = Some(Arc::new(trash));
    }

    /// Applies the `trash` config group to the attached trash.
    pub fn configure_trash(&mut self, settings: crate::trash::TrashSettings) {
        // A worker may hold the trash for a copy that runs: the new
        // settings go into a new one.
        if let Some(trash) = self.trash.take() {
            self.trash = Some(Arc::new(trash.with_settings(settings)));
        }
    }

    pub fn trash(&self) -> Option<&crate::trash::LocalTrash> {
        self.trash.as_deref()
    }

    /// Passes the explicit transfer-concurrency ceiling to the app (see
    /// `resourceLimits.concurrentTransfers`).
    pub fn set_concurrent_transfers(&mut self, ceiling: Option<usize>) {
        self.app.set_concurrent_transfers(ceiling);
    }

    /// Wires the daemon-wide resource management set in:
    /// the budget, the bandwidth shaper, the shared transfer step knob,
    /// and the idle signal. The multi-profile runtime shares one of
    /// each across every profile.
    pub fn attach_resource_management(
        &mut self,
        budget: Arc<Mutex<crate::resource_budget::ResourceBudget>>,
        bandwidth: Arc<crate::bandwidth::TransferBandwidth>,
        idle_notifier: Arc<dyn vapor_platform::IdleNotifier>,
    ) {
        self.resource_budget = budget;
        self.bandwidth = bandwidth;
        self.idle_notifier = idle_notifier;
    }

    /// Fixes the link capacity instead of measuring it (the static
    /// throttle inputs of the test harnesses).
    pub fn pin_link_capacity_kbps(&mut self, kbps: u32) {
        self.bandwidth.pin_capacity_kbps(kbps);
    }

    pub fn set_idle_notifier(&mut self, idle_notifier: Arc<dyn vapor_platform::IdleNotifier>) {
        self.idle_notifier = idle_notifier;
    }

    /// Latest effective ceilings + utilization for the IPC surface
    ///. `None` until the first 1s sample.
    pub fn resources_status(&self) -> Option<vapor_ipc::ResourcesStatus> {
        let ceilings = self.latest_ceilings.as_ref()?;
        let inputs = self.last_sampled_inputs.as_ref();
        let now_inst = self.clock.now();
        let capacity_kbps = |direction| {
            self.bandwidth
                .capacity_bytes_per_second(direction, now_inst)
                .map(|bytes_per_second| bytes_per_second * 8 / 1_000)
        };
        Some(vapor_ipc::ResourcesStatus {
            cpu: vapor_ipc::Ceiling {
                limit_percent: ceilings.cpu_percent,
                used_percent: inputs.map(|i| i.vapor_cpu_load_percent).unwrap_or(0),
            },
            memory: vapor_ipc::Ceiling {
                limit_percent: ceilings.memory_percent,
                used_percent: inputs
                    .and_then(|i| memory_share_percent(i.vapor_memory_bytes, i.device_memory_bytes))
                    .unwrap_or(0),
            },
            bandwidth: vapor_ipc::BandwidthCeiling {
                limit_percent: ceilings.bandwidth_percent,
                used_kbps: self.bandwidth.used_kbps(now_inst),
                upload_capacity_kbps: capacity_kbps(crate::bandwidth::TransferDirection::Upload),
                download_capacity_kbps: capacity_kbps(
                    crate::bandwidth::TransferDirection::Download,
                ),
            },
            idle_boost: vapor_ipc::StateReason {
                state: ceilings.boost_state.to_string(),
                reason: ceilings.reason.clone(),
            },
        })
    }

    /// Cumulative loop-prevention suppressions across both echo caches.
    pub fn loop_suppression_count(&self) -> u64 {
        self.local_echoes.suppressed_count() + self.remote_echoes.suppressed_count()
    }

    /// Events dropped at the bounded ingest boundary since startup.
    pub fn dropped_incoming_event_count(&self) -> u64 {
        self.recorder
            .as_ref()
            .map(|recorder| recorder.dropped_incoming_event_count() as u64)
            .unwrap_or(0)
    }

    /// Per-intent "why stuck" rows: active executor stages plus
    /// the oldest queued/retrying durable rows, each with a
    /// human-readable blocker.
    pub fn work_items(
        &self,
        profile_id: &str,
        limit: usize,
        now: SystemTime,
    ) -> Vec<vapor_ipc::WorkItem> {
        let mut rows = Vec::new();
        let mut active_ids = std::collections::BTreeSet::new();
        let workgate = self.app.workgate_snapshot();
        let paused = self.app.snapshot().run_state == RunState::Paused;

        // The lane of small transfers, when it is open: a small
        // transfer waits only while the lane and the slots are full.
        let small_lane = |active: usize, slots: usize| {
            if workgate.small_transfers_per_slot == 0 {
                return String::new();
            }
            format!(
                ", small transfers {active}/{}",
                slots * workgate.small_transfers_per_slot
            )
        };
        for (intent_id, path, kind, stage, elapsed_ms, attempt_count, last_error) in
            self.staged_executor.active_stages()
        {
            active_ids.insert(intent_id);
            let blocker_reason = match stage {
                crate::executor::ExecutionStage::WaitingForHash => format!(
                    "hash workers {}/{} and read tokens {}/{} in use",
                    workgate.active_hash_workers,
                    workgate.caps.hash_workers,
                    workgate.active_read_tokens,
                    workgate.caps.read_tokens
                ),
                crate::executor::ExecutionStage::WaitingForUpload => format!(
                    "upload concurrency {}/{} in use{}",
                    workgate.active_uploads,
                    workgate.caps.upload_concurrency,
                    small_lane(
                        workgate.active_small_uploads,
                        workgate.caps.upload_concurrency
                    )
                ),
                crate::executor::ExecutionStage::WaitingForDownload => format!(
                    "download concurrency {}/{} in use{}",
                    workgate.active_downloads,
                    workgate.caps.download_concurrency,
                    small_lane(
                        workgate.active_small_downloads,
                        workgate.caps.download_concurrency
                    )
                ),
                _ => String::new(),
            };
            rows.push(vapor_ipc::WorkItem {
                id: intent_id,
                profile_id: profile_id.to_string(),
                path: path.to_string_lossy().into_owned(),
                action: kind.wire_action().to_string(),
                stage: stage.wire_name().to_string(),
                elapsed_ms,
                attempts: attempt_count,
                error: last_error,
                reason: blocker_reason,
            });
            if rows.len() >= limit {
                return rows;
            }
        }

        let queued = match self.state_db.list_queue_intents(limit) {
            Ok(queued) => queued,
            Err(error) => {
                logging::warning(
                    "Could not list queue intents for diagnostics",
                    &[("error", error.to_string())],
                );
                return rows;
            }
        };
        for intent in queued {
            if active_ids.contains(&intent.id) || rows.len() >= limit {
                continue;
            }
            let retrying = intent.available_at > now;
            let stage = if retrying {
                constants::intent::STAGE_RETRYING
            } else {
                constants::intent::STAGE_QUEUED
            };
            let blocker_reason = if paused {
                "daemon is paused".to_string()
            } else if let Some(reason) = self.sign_in_reason() {
                reason
            } else if !self.cloud_root_ready {
                "cloud root is unavailable".to_string()
            } else if self
                .path_filter
                .as_ref()
                .is_some_and(|filter| !filter.rules_ready())
            {
                IGNORE_RULES_NOT_READ.to_string()
            } else if self.startup_reconstruction_barrier {
                "waiting for the startup reconstruction reconcile".to_string()
            } else if retrying {
                format!(
                    "retry backoff active (attempt {}); next try in {}",
                    intent.attempt_count,
                    vapor_shared::duration_text(
                        intent.available_at.duration_since(now).unwrap_or_default()
                    )
                )
            } else {
                format!(
                    "waiting for admission; planner workers {}/{} in use",
                    workgate.active_planner_workers, workgate.caps.planner_workers
                )
            };
            let elapsed_in_stage_ms = now
                .duration_since(intent.enqueued_at)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0);
            rows.push(vapor_ipc::WorkItem {
                id: intent.id,
                profile_id: profile_id.to_string(),
                path: intent.path.to_string_lossy().into_owned(),
                action: intent.kind.wire_action().to_string(),
                stage: stage.to_string(),
                elapsed_ms: elapsed_in_stage_ms,
                attempts: intent.attempt_count,
                error: intent.last_error.unwrap_or_default(),
                reason: blocker_reason,
            });
        }
        rows
    }

    /// Emits timeline entries for state transitions and notable tick
    /// outcomes. Cheap: only fires on changes and non-zero
    /// counters.
    fn emit_timeline_events(&mut self, report: &RuntimeTickReport, now: SystemTime) {
        let Some(timeline) = self.timeline.clone() else {
            return;
        };
        let profile_id = self.profile_id.as_str();

        let run_state = self.app.snapshot().run_state;
        if self.last_timeline_run_state != Some(run_state) {
            timeline.push(
                constants::timeline::KIND_RUN_STATE,
                profile_id,
                format!("{}: {}", run_state.wire_name(), self.app.snapshot().reason),
                now,
            );
            self.last_timeline_run_state = Some(run_state);
        }
        let throttle_state = self.app.snapshot().throttle_state;
        if self.last_timeline_throttle != Some(throttle_state) {
            timeline.push(
                constants::timeline::KIND_THROTTLE,
                profile_id,
                format!(
                    "{}: {}",
                    throttle_state.wire_name(),
                    self.app
                        .throttle_decision()
                        .map(|decision| decision.reason.clone())
                        .unwrap_or_default()
                ),
                now,
            );
            self.last_timeline_throttle = Some(throttle_state);
        }
        if let Some(path) = report.conflict_paths.first() {
            let others = report.conflict_paths.len() - 1;
            let message = if others == 0 {
                format!("kept both versions of {}", path.display())
            } else {
                format!(
                    "kept both versions of {} and {others} more path(s)",
                    path.display()
                )
            };
            timeline.push(constants::timeline::KIND_CONFLICT, profile_id, message, now);
        }
        if report.failed_intents > 0 {
            timeline.push(
                constants::timeline::KIND_FAILURE,
                profile_id,
                format!("{} change(s) failed for good", report.failed_intents),
                now,
            );
        }
        if report.mirror_reverts > 0 || report.mirror_deletes > 0 {
            timeline.push(
                constants::timeline::KIND_MIRROR,
                profile_id,
                format!(
                    "strict mirror reverted {} and removed {} path(s)",
                    report.mirror_reverts, report.mirror_deletes
                ),
                now,
            );
        }
        if let Some((path, cause)) = report.cloud_deletes.first() {
            let others = report.cloud_deletes.len() - 1;
            let message = if others == 0 {
                format!("removed {} from the cloud: {cause}", path.display())
            } else {
                format!(
                    "removed {} and {others} more path(s) from the cloud: {cause}",
                    path.display()
                )
            };
            timeline.push(
                constants::timeline::KIND_CLOUD_DELETE,
                profile_id,
                message,
                now,
            );
        }
        if let Some(root) = &report.completed_reconcile_root {
            timeline.push(
                constants::timeline::KIND_RECONCILE,
                profile_id,
                format!("reconcile completed for {}", root.display()),
                now,
            );
        }
        if report.remote_poll.cursor_expired {
            timeline.push(
                constants::timeline::KIND_FEED,
                profile_id,
                "remote changes cursor expired; whole-scope reconcile scheduled",
                now,
            );
        }
    }

    /// Sets the device identifier. The daemon passes the value that
    /// `vapor.json` holds, which is written one time and kept.
    pub fn set_device_id(&mut self, device_id: impl Into<String>) {
        self.device_id = device_id.into();
    }

    /// Sets the resolved profile id (multi-profile shell). Rebuilds the
    /// remote poller so its durable cursor key is namespaced per profile,
    /// and re-attributes timeline events to this profile.
    pub fn set_profile_id(&mut self, profile_id: impl Into<String>) {
        let profile_id = profile_id.into();
        self.remote_poller = RemotePoller::new(&profile_id);
        self.profile_id = profile_id;
        // A hold found at startup was phrased before the id was known.
        if self.app.snapshot().run_state != RunState::Paused
            && let Some(reason) = self.sign_in_reason()
        {
            self.app.set_run_state(RunState::Error, reason);
        }
    }

    /// The provider refused the sign-in and the profile waits for the
    /// user to sign in again.
    pub fn sign_in_required(&self) -> bool {
        self.sign_in_hold
    }

    /// The run-state reason while the sign-in hold is on: what happened
    /// and the command that ends it.
    fn sign_in_reason(&self) -> Option<String> {
        self.sign_in_hold.then(|| {
            format!(
                "Vapor needs to sign in to {}: sync is on hold until then \
                 (vapor auth login --profile {})",
                self.app.provider_name(),
                self.profile_id
            )
        })
    }

    /// Holds the profile because the provider refused the sign-in (an
    /// expired or revoked token, no token at all). Admission and the
    /// changes poll stop; the root check keeps probing, and the first
    /// probe that gets through releases the hold.
    /// The cloud had no room for an upload. The first time, the user is
    /// told (a WARNING, the timeline, the run reason); each later
    /// failure only keeps the state alive.
    fn mark_cloud_full(&mut self, message: &str, now: SystemTime) {
        let newly_full = self.cloud_full.is_none();
        self.cloud_full = Some((message.to_string(), self.clock.now()));
        if !newly_full {
            return;
        }
        logging::warning(
            "The cloud has no room; uploads wait and are tried again later",
            &[
                ("profile_id", self.profile_id.clone()),
                ("provider", self.app.provider_name().to_string()),
                ("error", message.to_string()),
                (
                    "retry_in_seconds",
                    constants::engine::CLOUD_FULL_RETRY_SECONDS.to_string(),
                ),
            ],
        );
        if let Some(timeline) = &self.timeline {
            timeline.push(
                constants::timeline::KIND_CLOUD_FULL,
                self.profile_id.clone(),
                format!("The cloud has no room: uploads wait. {message}"),
                now,
            );
        }
        self.restore_run_state_after_root_recovery("the cloud is full");
    }

    /// Ends the cloud-full state when the uploads that waited have had
    /// their next try and none of them said "full" again.
    fn clear_cloud_full_if_over(&mut self, now: SystemTime) {
        let Some((_, last_said)) = &self.cloud_full else {
            return;
        };
        let quiet_for = self.clock.now().saturating_duration_since(*last_said);
        // One retry interval, and time for the retried uploads to run.
        let over = Duration::from_secs(constants::engine::CLOUD_FULL_RETRY_SECONDS + 60);
        if quiet_for < over {
            return;
        }
        self.cloud_full = None;
        logging::info(
            "Uploads get through again; the cloud has room",
            &[("profile_id", self.profile_id.clone())],
        );
        if let Some(timeline) = &self.timeline {
            timeline.push(
                constants::timeline::KIND_CLOUD_FULL,
                self.profile_id.clone(),
                "The cloud has room again; uploads resume",
                now,
            );
        }
        self.restore_run_state_after_root_recovery("the cloud has room again");
    }

    fn mark_sign_in_required(&mut self, message: &str, now: SystemTime) {
        if self.sign_in_hold {
            return;
        }
        self.sign_in_hold = true;
        logging::warning(
            "The provider refused the sign-in; sync is on hold until the user signs in again",
            &[
                ("profile_id", self.profile_id.clone()),
                ("provider", self.app.provider_name().to_string()),
                ("error", message.to_string()),
            ],
        );
        if self.app.snapshot().run_state != RunState::Paused
            && let Some(reason) = self.sign_in_reason()
        {
            self.app.set_run_state(RunState::Error, reason);
        }
        if let Some(timeline) = &self.timeline {
            timeline.push(
                constants::timeline::KIND_SIGN_IN,
                self.profile_id.clone(),
                format!(
                    "Vapor needs to sign in to {}; sync is on hold until then",
                    self.app.provider_name()
                ),
                now,
            );
        }
    }

    /// Ends the sign-in hold when a call failed for another reason than
    /// the sign-in: the cloud is not reachable, and the root stays not
    /// ready, so nothing is released to run. A sign-in that the cloud
    /// refuses once it is reachable puts the hold back. Returns whether
    /// a hold was ended.
    fn note_stored_sign_in(&mut self, now: SystemTime) -> bool {
        if !std::mem::take(&mut self.sign_in_hold) {
            return false;
        }
        logging::info(
            "A sign-in is stored; the cloud is not reachable yet",
            &[
                ("profile_id", self.profile_id.clone()),
                ("provider", self.app.provider_name().to_string()),
            ],
        );
        if let Some(timeline) = &self.timeline {
            timeline.push(
                constants::timeline::KIND_SIGN_IN,
                self.profile_id.clone(),
                format!(
                    "a sign-in to {} is stored; the cloud is not reachable yet",
                    self.app.provider_name()
                ),
                now,
            );
        }
        true
    }

    /// Ends the sign-in hold after a provider call got through. Returns
    /// whether a hold was released; the caller re-derives the run state.
    fn release_sign_in_hold(&mut self, now: SystemTime) -> bool {
        if !std::mem::take(&mut self.sign_in_hold) {
            return false;
        }
        logging::info(
            "Signed in again; sync resumes",
            &[
                ("profile_id", self.profile_id.clone()),
                ("provider", self.app.provider_name().to_string()),
            ],
        );
        if let Some(timeline) = &self.timeline {
            timeline.push(
                constants::timeline::KIND_SIGN_IN,
                self.profile_id.clone(),
                format!(
                    "signed in to {} again; sync resumes",
                    self.app.provider_name()
                ),
                now,
            );
        }
        true
    }

    /// How many entries on this device the ignore rules keep out of
    /// sync, as the last whole-scope scan counted them.
    pub fn ignored_paths(&self) -> Option<u64> {
        self.ignored_paths
    }

    /// Cumulative keep-both conflict copies created since daemon start.
    pub fn conflict_count(&self) -> u64 {
        self.conflict_count
    }

    /// What the whole-scope scan is doing, for status: running,
    /// waiting on the throttle (with the reason), or nothing pending.
    pub fn reconcile_status(&self) -> ReconcileStatus {
        if let Some(root) = self.app.running_reconcile_root() {
            return ReconcileStatus::Running {
                root: root.display().to_string(),
            };
        }
        if let Some(reason) = &self.reconcile_wait_reason {
            return ReconcileStatus::Waiting {
                reason: reason.clone(),
            };
        }
        // The scan of a start waits for the ignore rules, and a tree
        // that is slow to read must not look like a profile at rest.
        if self
            .path_filter
            .as_ref()
            .is_some_and(|filter| !filter.rules_ready())
        {
            return ReconcileStatus::Waiting {
                reason: IGNORE_RULES_NOT_READ.to_string(),
            };
        }
        // A folder that took a burst waits for one scan of the folder.
        // Its changes are in no queue until then, and a status that
        // said "nothing to do" for them would be wrong.
        if let Some(reason) = self.deferred_scan_reason() {
            return ReconcileStatus::Waiting { reason };
        }
        ReconcileStatus::Idle
    }

    /// What `vapor status` says while the changes of a burst wait for
    /// the scan of their folder: how many, where, and when the scan
    /// starts.
    fn deferred_scan_reason(&self) -> Option<String> {
        let recorder = self.recorder.as_ref()?;
        let deferred = recorder.with_state(|maps| maps.deferred_reconcile_summaries());
        let (folder, _, _) = deferred.first()?;
        let changes: usize = deferred.iter().map(|(_, held, _)| held).sum();
        let shown = self
            .sync_scope
            .local_root
            .as_deref()
            .and_then(|root| folder.strip_prefix(root).ok())
            .filter(|below| !below.as_os_str().is_empty())
            .map_or_else(
                || "the sync folder".to_string(),
                |below| below.display().to_string(),
            );
        let place = match deferred.len() {
            1 => shown,
            more => format!("{shown} and {} more", more - 1),
        };
        if !self.deferred_scans_can_be_released() {
            return Some(format!(
                "{changes} change(s) in {place} wait for one scan, which starts when sync runs again"
            ));
        }
        let wait = if self.app.snapshot().throttle_state == vapor_shared::ThrottleState::Idle {
            constants::engine::DEFERRED_RECONCILE_IDLE_QUIET_RELEASE_MILLIS
        } else {
            constants::engine::DEFERRED_RECONCILE_DELAY_MILLIS
        };
        Some(format!(
            "{changes} change(s) in {place} wait for one scan, which starts {} s after the burst there ends",
            wait.div_ceil(1_000)
        ))
    }

    /// Cumulative count of strict-mirror reverts / deletions performed
    /// by the one-way modes since daemon start (diagnostics).
    pub fn mirror_counters(&self) -> (u64, u64) {
        (self.mirror_revert_count, self.mirror_delete_count)
    }

    /// Retries ensuring the provider-side sync root while it is
    /// unavailable. Between attempts, sync work stays blocked
    /// and intents accumulate durably, never dropped.
    /// Flips the daemon into the blocked cloud-root state when the root
    /// vanishes *mid-run* (deleted, unmounted, remote folder removed).
    /// Admission stops on the next tick (`paused` derives from
    /// `cloud_root_ready`), the periodic ensure-retry recreates the root,
    /// and recovery schedules a whole-scope reconcile: the same
    /// self-healing path a missing root takes at startup.
    fn mark_cloud_root_unavailable(&mut self, reason: &str, now: SystemTime) {
        if !self.cloud_root_ready {
            return;
        }
        self.cloud_root_ready = false;
        // Retry immediately on the next tick, then at the ensure cadence.
        self.last_cloud_root_attempt_inst = None;
        logging::warning(
            "Cloud root became unavailable; blocking sync work until it is re-ensured",
            &[
                ("cloud_root", self.sync_scope.cloud_root.clone()),
                ("reason", reason.to_string()),
            ],
        );
        // Preserve an explicit pause: the root recovery path must not
        // silently resume either.
        if self.app.snapshot().run_state != RunState::Paused {
            self.app.set_run_state(
                RunState::Error,
                format!(
                    "cloud root {} is unavailable; sync work is blocked until it can be ensured",
                    self.sync_scope.cloud_root
                ),
            );
        }
        if let Some(timeline) = &self.timeline {
            timeline.push(
                constants::timeline::KIND_CLOUD_ROOT,
                self.profile_id.clone(),
                "cloud root became unavailable; sync blocked until it is restored",
                now,
            );
        }
    }

    fn retry_cloud_root_if_needed(&mut self) -> Result<(), DaemonRuntimeError> {
        if self.cloud_root_ready {
            return Ok(());
        }
        let now_inst = self.clock.now();
        // A root that was adopted is probed at the root-check cadence
        // (a stat, cheap); creating one that never existed retries at
        // the slower ensure cadence. A probe held by the sign-in fails
        // before any request, so it runs at the fast cadence too and a
        // new sign-in is noticed quickly.
        let interval = if self.root_hold.is_some() || self.sign_in_hold {
            Duration::from_secs(constants::engine::ROOT_CHECK_INTERVAL_SECONDS)
        } else {
            Duration::from_secs(constants::engine::CLOUD_ROOT_ENSURE_RETRY_SECONDS)
        };
        let Some(outcome) = self.harvest_or_start_cloud_probe(now_inst, interval) else {
            return Ok(());
        };
        let now = self.clock.now_system();
        let first_probe = std::mem::take(&mut self.first_cloud_probe_pending);
        match outcome {
            CloudRootOutcome::Ready { adopted } => {
                if let Some(identity) = adopted
                    && let Err(error) = crate::root_identity::record_identity(
                        &mut self.state_db,
                        crate::root_identity::RootSide::Cloud,
                        &identity,
                        now,
                    )
                {
                    logging::warning(
                        "Could not record the adopted cloud root identity",
                        &[("error", error.to_string())],
                    );
                }
                self.cloud_root_ready = true;
                self.release_sign_in_hold(now);
                self.release_root_hold(crate::root_identity::RootSide::Cloud, now);
                if first_probe {
                    // The profile's start, not the end of an outage:
                    // the queue holds what the last run left, and the
                    // startup reconcile is queued already.
                    self.restore_run_state_after_root_recovery("cloud root ready");
                    return Ok(());
                }
                if let Err(error) = self.forget_the_outage() {
                    logging::warning(
                        "Could not drop the deletions queued during the outage",
                        &[("error", error.to_string())],
                    );
                }
                // A recovered root may have drifted while unreachable: a
                // whole-scope reconcile converges it. The two-way walk
                // applies a cloud deletion here only to a file the sync
                // index vouches for.
                if let Err(error) = self.enqueue_startup_reconstruction_reconcile(now) {
                    logging::warning(
                        "Could not schedule the post-recovery whole-scope reconcile",
                        &[("error", format!("{error:?}"))],
                    );
                }
                self.restore_run_state_after_root_recovery("cloud root recovered");
            }
            CloudRootOutcome::Replaced { found } => {
                self.cloud_root_ready = true;
                if let Err(error) =
                    self.hold_for_replaced_root(crate::root_identity::RootSide::Cloud, found, now)
                {
                    logging::warning(
                        "Could not open the root-replaced decision",
                        &[("error", format!("{error:?}"))],
                    );
                }
            }
            CloudRootOutcome::Missing => {
                self.hold_for_missing_root(crate::root_identity::RootSide::Cloud, now)?;
            }
            CloudRootOutcome::Unreachable(message) => {
                logging::debug(
                    "Cloud root probe failed; retrying at the ensure cadence",
                    &[("error", message)],
                );
                // A provider that cannot read its sign-in says so before
                // it sends anything. This failure came later: a sign-in
                // is stored, and the way to the cloud is what is
                // missing. The status must not go on asking for a
                // sign-in that the user has just given.
                let sign_in_is_stored = self.note_stored_sign_in(now);
                if first_probe || sign_in_is_stored {
                    self.restore_run_state_after_root_recovery("cloud root checked");
                }
            }
            CloudRootOutcome::SignInRequired(message) => self.mark_sign_in_required(&message, now),
        }
        Ok(())
    }

    /// Harvests a cloud probe started on an earlier tick, or starts one
    /// when the cadence allows. `None` while nothing has landed.
    fn harvest_or_start_cloud_probe(
        &mut self,
        now_inst: Instant,
        interval: Duration,
    ) -> Option<CloudRootOutcome> {
        if let Some(call) = self.cloud_root_call.as_mut() {
            let outcome = call.take()?;
            self.cloud_root_call = None;
            return match outcome {
                Ok(outcome) => Some(outcome),
                Err(panic) => Some(CloudRootOutcome::Unreachable(panic)),
            };
        }
        let due = self
            .last_cloud_root_attempt_inst
            .map(|last| now_inst.saturating_duration_since(last) >= interval)
            .unwrap_or(true);
        if !due {
            return None;
        }
        self.last_cloud_root_attempt_inst = Some(now_inst);
        let recorded = match crate::root_identity::recorded_identity(
            &self.state_db,
            crate::root_identity::RootSide::Cloud,
        ) {
            Ok(recorded) => recorded,
            Err(error) => {
                logging::warning(
                    "Could not read the recorded cloud root identity",
                    &[("error", error.to_string())],
                );
                return None;
            }
        };
        let provider = self.app.provider_handle();
        let cloud_root = self.sync_scope.cloud_root.clone();
        let device_id = self.device_id.clone();
        let mut call = crate::provider_jobs::ProviderCall::start(
            &self.provider_call_mode,
            "probe-cloud-root",
            move || probe_cloud_root(provider, cloud_root.as_str(), recorded, &device_id),
        );
        match call.take() {
            Some(Ok(outcome)) => Some(outcome),
            Some(Err(panic)) => Some(CloudRootOutcome::Unreachable(panic)),
            None => {
                self.cloud_root_call = Some(call);
                None
            }
        }
    }

    /// Re-derives the run state once a root is back. An explicit pause
    /// (the user's) stays a pause: recovery never resumes silently.
    fn restore_run_state_after_root_recovery(&mut self, what: &str) {
        if self.app.snapshot().run_state != RunState::Paused
            && let Some(reason) = self.sign_in_reason()
        {
            self.app.set_run_state(RunState::Error, reason);
            return;
        }
        if self.root_hold.is_some() {
            let reason = self
                .root_hold
                .as_ref()
                .map(|hold| hold.reason.clone())
                .unwrap_or_default();
            self.app.set_run_state(RunState::Error, reason);
            return;
        }
        if let Some(reason) = self.one_way_hold.clone() {
            self.app.set_run_state(RunState::Error, reason);
            return;
        }
        if !self.cloud_root_ready {
            self.app.set_run_state(
                RunState::Error,
                format!(
                    "cloud root {} is unavailable; sync work is blocked until it comes back",
                    self.sync_scope.cloud_root
                ),
            );
            return;
        }
        if self.app.snapshot().run_state == RunState::Paused {
            return;
        }
        if let Some(reason) = self.local_root_unreadable.clone() {
            self.app.set_run_state(RunState::Error, reason);
            return;
        }
        if let Some((message, _)) = &self.cloud_full {
            // Downloads and deletions go on; the state says what waits.
            self.app
                .set_run_state(RunState::Error, format!("uploads wait: {message}"));
            return;
        }
        if self.user_paused {
            // A hold that sat over the pause is gone. The pause is not.
            self.app
                .set_run_state(RunState::Paused, "user paused via vapor pause");
            return;
        }
        match self.sync_scope.local_root.as_ref() {
            Some(path) => self.app.set_run_state(
                RunState::Running,
                format!("{what}; watching {}", path.display()),
            ),
            None => self
                .app
                .set_run_state(RunState::Paused, "no local root configured"),
        }
    }

    /// A one-way mode overwrites and removes on one side. It runs only
    /// after the user acknowledged that for this mode and this pair of
    /// roots; until then the profile holds behind a decision and syncs
    /// nothing. A decision left from another mode or another pair of
    /// roots is withdrawn.
    fn hold_until_one_way_is_acknowledged(
        &mut self,
        now: SystemTime,
    ) -> Result<(), DaemonRuntimeError> {
        use crate::one_way;
        let mode = self.sync_scope.sync_mode;
        let Some(local_root) = self.sync_scope.local_root.clone() else {
            return Ok(());
        };
        let cloud_root = self.sync_scope.cloud_root.clone();
        let wanted = one_way::fingerprint(mode, &local_root, &cloud_root);
        let open = self
            .state_db
            .open_decision(one_way::DECISION_KIND, Some(&local_root))?;
        let asks_for_this_mirror = |decision: &crate::state_db::DecisionRecord| {
            decision.evidence["fingerprint"].as_str() == Some(wanted.as_str())
        };
        if let Some(stale) = open.as_ref().filter(|decision| {
            mode == vapor_shared::SyncMode::TwoWay || !asks_for_this_mirror(decision)
        }) {
            self.withdraw_decision(stale.id, now)?;
        }
        if mode == vapor_shared::SyncMode::TwoWay {
            // A confirmation is for the mirror that ran. A profile that
            // synced both ways since then holds changes the user made
            // on both sides, so a return to the mirror asks again.
            self.state_db
                .delete_state(constants::state::ONE_WAY_ACKNOWLEDGED_KEY)?;
            return Ok(());
        }
        if one_way::acknowledged(&self.state_db, mode, &local_root, &cloud_root)? {
            // Confirmed on another surface while this question was open.
            if let Some(answered) = open.filter(asks_for_this_mirror) {
                self.withdraw_decision(answered.id, now)?;
            }
            return Ok(());
        }
        let decision_id = match open.filter(asks_for_this_mirror) {
            Some(decision) => decision.id,
            None => {
                let id = self.state_db.create_decision(
                    one_way::DECISION_KIND,
                    crate::state_db::DecisionScope::Profile,
                    Some(&local_root),
                    &one_way::question(mode, &local_root, &cloud_root),
                    &one_way::options(mode),
                    &serde_json::json!({
                        "mode": mode.as_config_value(),
                        "localRoot": local_root.display().to_string(),
                        "cloudRoot": cloud_root,
                        "fingerprint": wanted,
                    }),
                    now,
                )?;
                logging::warning(
                    "A one-way mode needs the user's confirmation; holding the profile behind a decision",
                    &[
                        ("sync_mode", mode.as_config_value().to_string()),
                        ("decision_id", id.to_string()),
                    ],
                );
                self.announce_decisions(&[id], now);
                id
            }
        };
        let reason = format!(
            "{} needs your confirmation before it runs: it overwrites and removes files; \
             answer decision #{decision_id} (vapor decisions list)",
            mode.as_config_value()
        );
        if self.app.snapshot().run_state != RunState::Paused {
            self.app.set_run_state(RunState::Error, reason.clone());
        }
        self.one_way_hold = Some(reason);
        Ok(())
    }

    /// Lets a held one-way profile run when the confirmation arrived
    /// from another surface: `vapor profiles add` on a terminal asks the
    /// question itself and records the answer in the state database,
    /// and the daemon can compose the profile before that write lands.
    fn release_one_way_hold_if_acknowledged(
        &mut self,
        now: SystemTime,
    ) -> Result<(), DaemonRuntimeError> {
        use crate::one_way;
        let mode = self.sync_scope.sync_mode;
        let Some(local_root) = self.sync_scope.local_root.clone() else {
            return Ok(());
        };
        let cloud_root = self.sync_scope.cloud_root.clone();
        if !one_way::acknowledged(&self.state_db, mode, &local_root, &cloud_root)? {
            return Ok(());
        }
        if let Some(open) = self
            .state_db
            .open_decision(one_way::DECISION_KIND, Some(&local_root))?
        {
            self.withdraw_decision(open.id, now)?;
        }
        self.one_way_hold = None;
        self.restore_run_state_after_root_recovery("one-way mode confirmed");
        logging::info(
            "One-way mode was confirmed on another surface; the profile runs as a strict mirror",
            &[("sync_mode", mode.as_config_value().to_string())],
        );
        Ok(())
    }

    /// Applies an `enable` answer: records the acknowledgement for the
    /// mirror the question was about and lets the profile run.
    fn enable_one_way_mode(
        &mut self,
        decision: &crate::state_db::DecisionRecord,
        now: SystemTime,
    ) -> Result<String, String> {
        use crate::one_way;
        let mode = self.sync_scope.sync_mode;
        let local_root = self
            .sync_scope
            .local_root
            .clone()
            .ok_or_else(|| "the profile has no local root".to_string())?;
        let cloud_root = self.sync_scope.cloud_root.clone();
        if decision.evidence["fingerprint"].as_str()
            != Some(one_way::fingerprint(mode, &local_root, &cloud_root).as_str())
        {
            // The answer is about a mirror this profile no longer is.
            return Ok("ignored: the profile's mode or roots changed since the question".into());
        }
        one_way::acknowledge(&mut self.state_db, mode, &local_root, &cloud_root, now)
            .map_err(|error| error.to_string())?;
        self.one_way_hold = None;
        self.restore_run_state_after_root_recovery("one-way mode confirmed");
        logging::info(
            "One-way mode confirmed by the user; the profile runs as a strict mirror",
            &[("sync_mode", mode.as_config_value().to_string())],
        );
        Ok(format!(
            "enabled: {} runs for these two folders",
            mode.as_config_value()
        ))
    }

    /// Records the acknowledgement of this profile's one-way mode, the
    /// way an `enable` answer does.
    #[cfg(test)]
    pub(crate) fn acknowledge_one_way_mode(&mut self, now: SystemTime) {
        let open = self
            .state_db
            .open_decision(
                crate::one_way::DECISION_KIND,
                self.sync_scope.local_root.as_deref(),
            )
            .expect("read decision")
            .expect("a one-way profile asks before it runs");
        self.state_db
            .resolve_decision(open.id, crate::one_way::OPTION_ENABLE, now)
            .expect("answer");
        self.apply_resolved_decisions(now).expect("apply");
        assert!(self.one_way_hold.is_none(), "the answer releases the hold");
    }

    /// Opens (or finds) the `root-replaced` decision for `side` and holds
    /// the profile behind it.
    fn hold_for_replaced_root(
        &mut self,
        side: crate::root_identity::RootSide,
        found: Option<String>,
        now: SystemTime,
    ) -> Result<(), DaemonRuntimeError> {
        use crate::root_identity::{DECISION_KIND, OPTION_REATTACH, RootSide, question};
        if self
            .root_hold
            .as_ref()
            .is_some_and(|hold| hold.side == side && !hold.missing)
        {
            return Ok(());
        }
        self.withdraw_root_decisions(side, crate::root_identity::MISSING_DECISION_KIND, now)?;
        let root = match side {
            RootSide::Local => self
                .sync_scope
                .local_root
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            RootSide::Cloud => self.sync_scope.cloud_root.clone(),
        };
        let path = match side {
            RootSide::Local => self.sync_scope.local_root.clone(),
            RootSide::Cloud => None,
        };
        let decision_id = match self
            .state_db
            .open_decision(DECISION_KIND, path.as_deref())?
        {
            Some(decision) => decision.id,
            None => {
                let question = question(side, &root, found.as_deref());
                let options = [crate::state_db::DecisionOption {
                    key: OPTION_REATTACH.to_string(),
                    label: "Reattach this folder and merge, deleting nothing".to_string(),
                }];
                let evidence = serde_json::json!({
                    "side": side.label(),
                    "root": root,
                    "found_identity": found,
                });
                let id = self.state_db.create_decision(
                    DECISION_KIND,
                    crate::state_db::DecisionScope::Profile,
                    path.as_deref(),
                    &question,
                    &options,
                    &evidence,
                    now,
                )?;
                logging::warning(
                    "Sync root replaced; holding the profile behind a decision",
                    &[
                        ("side", side.label().to_string()),
                        ("root", root.clone()),
                        ("decision_id", id.to_string()),
                    ],
                );
                self.announce_decisions(&[id], now);
                id
            }
        };
        let reason = format!(
            "{} root {root} is not the folder this profile adopted; answer decision #{decision_id} (vapor decisions list) or put the original folder back",
            side.label()
        );
        self.root_hold = Some(RootHold {
            side,
            reason: reason.clone(),
            decision_id: Some(decision_id),
            missing: false,
        });
        if self.app.snapshot().run_state != RunState::Paused {
            self.app.set_run_state(RunState::Error, reason);
        }
        Ok(())
    }

    /// Holds the profile because a root went away, with a `root-missing`
    /// decision so the user can ask for it to be re-created instead of
    /// waiting.
    fn hold_for_missing_root(
        &mut self,
        side: crate::root_identity::RootSide,
        now: SystemTime,
    ) -> Result<(), DaemonRuntimeError> {
        use crate::root_identity::{
            MISSING_DECISION_KIND, OPTION_RECREATE, RootSide, missing_question,
        };
        if self
            .root_hold
            .as_ref()
            .is_some_and(|hold| hold.side == side && hold.missing)
        {
            return Ok(());
        }
        self.withdraw_root_decisions(side, crate::root_identity::DECISION_KIND, now)?;
        let root = match side {
            RootSide::Local => self
                .sync_scope
                .local_root
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            RootSide::Cloud => self.sync_scope.cloud_root.clone(),
        };
        let path = match side {
            RootSide::Local => self.sync_scope.local_root.clone(),
            RootSide::Cloud => None,
        };
        let decision_id = match self
            .state_db
            .open_decision(MISSING_DECISION_KIND, path.as_deref())?
        {
            Some(decision) => decision.id,
            None => {
                let question = missing_question(side, &root);
                let options = [crate::state_db::DecisionOption {
                    key: OPTION_RECREATE.to_string(),
                    label: format!(
                        "Re-create the folder empty and let the {} fill it",
                        match side {
                            RootSide::Local => "cloud",
                            RootSide::Cloud => "device",
                        }
                    ),
                }];
                let evidence = serde_json::json!({
                    "side": side.label(),
                    "root": root,
                });
                let id = self.state_db.create_decision(
                    MISSING_DECISION_KIND,
                    crate::state_db::DecisionScope::Profile,
                    path.as_deref(),
                    &question,
                    &options,
                    &evidence,
                    now,
                )?;
                logging::warning(
                    "Sync root is missing; holding the profile until it returns",
                    &[
                        ("side", side.label().to_string()),
                        ("root", root.clone()),
                        ("decision_id", id.to_string()),
                    ],
                );
                self.announce_decisions(&[id], now);
                id
            }
        };
        let reason = format!(
            "{} root {root} is missing; Vapor waits for it and never re-creates a folder it synced before (decision #{decision_id}: vapor decisions list)",
            side.label()
        );
        self.root_hold = Some(RootHold {
            side,
            reason: reason.clone(),
            decision_id: Some(decision_id),
            missing: true,
        });
        if self.app.snapshot().run_state != RunState::Paused {
            self.app.set_run_state(RunState::Error, reason);
        }
        Ok(())
    }

    /// Withdraws every open decision of `kind` about `side`: the
    /// condition it asked about is gone.
    fn withdraw_root_decisions(
        &mut self,
        side: crate::root_identity::RootSide,
        kind: &str,
        now: SystemTime,
    ) -> Result<(), DaemonRuntimeError> {
        for decision in self.state_db.decisions(false)? {
            if decision.kind == kind && decision.evidence["side"] == side.label() {
                self.withdraw_decision(decision.id, now)?;
                if let Some(timeline) = &self.timeline {
                    timeline.push(
                        constants::timeline::KIND_DECISION,
                        self.profile_id.clone(),
                        format!(
                            "Decision {} ({kind}) withdrawn: the {} root changed state",
                            decision.id,
                            side.label()
                        ),
                        now,
                    );
                }
            }
        }
        Ok(())
    }

    /// Clears a hold on `side` once its root is back or reattached,
    /// withdrawing the decision the hold was waiting on.
    fn release_root_hold(&mut self, side: crate::root_identity::RootSide, now: SystemTime) {
        let Some(hold) = self.root_hold.clone() else {
            return;
        };
        if hold.side != side {
            return;
        }
        if let Some(decision_id) = hold.decision_id
            && let Ok(Some(decision)) = self.state_db.decision(decision_id)
            && decision.is_open()
        {
            if let Err(error) = self.withdraw_decision(decision_id, now) {
                logging::warning(
                    "Could not withdraw the root-replaced decision",
                    &[("error", error.to_string())],
                );
            }
            if let Some(timeline) = &self.timeline {
                timeline.push(
                    constants::timeline::KIND_DECISION,
                    self.profile_id.clone(),
                    format!(
                        "Decision {decision_id} ({}) withdrawn: the original {} root is back",
                        decision.kind,
                        side.label()
                    ),
                    now,
                );
            }
        }
        self.root_hold = None;
        self.local_root_changed |= side == crate::root_identity::RootSide::Local;
        logging::info(
            "Sync root is back; releasing the profile",
            &[("side", side.label().to_string())],
        );
    }

    /// Checks both roots on a cadence: the local one inline (a stat and
    /// a small read), the cloud one through a worker probe.
    fn check_roots_if_due(&mut self, now: SystemTime) -> Result<(), DaemonRuntimeError> {
        let now_inst = self.clock.now();
        let interval = Duration::from_secs(constants::engine::ROOT_CHECK_INTERVAL_SECONDS);
        let due = self
            .last_root_check_inst
            .map(|last| now_inst.saturating_duration_since(last) >= interval)
            .unwrap_or(true);
        // A cloud check that is in flight is harvested when it answers,
        // not at the next cadence: a root in doubt holds work until
        // then.
        if !due && self.cloud_root_call.is_none() {
            return Ok(());
        }
        if due {
            self.last_root_check_inst = Some(now_inst);
        }
        if due && let Some(local_root) = self.sync_scope.local_root.clone() {
            use crate::root_identity::{RootSide, RootStatus};
            match crate::root_identity::check_local_root(
                &mut self.state_db,
                &local_root,
                &self.device_id,
                now,
            )? {
                RootStatus::Ready => {
                    if self
                        .root_hold
                        .as_ref()
                        .is_some_and(|hold| hold.side == RootSide::Local)
                    {
                        self.release_root_hold(RootSide::Local, now);
                        self.enqueue_startup_reconstruction_reconcile(now)?;
                        self.restore_run_state_after_root_recovery("local root is back");
                    } else if std::mem::take(&mut self.reconcile_after_root_check) {
                        self.enqueue_startup_reconstruction_reconcile(now)?;
                    }
                    // A `root-missing` question left by a start that found
                    // no folder is moot now that the folder is here.
                    self.withdraw_root_decisions(
                        RootSide::Local,
                        crate::root_identity::MISSING_DECISION_KIND,
                        now,
                    )?;
                }
                RootStatus::Missing => {
                    self.reconcile_after_root_check = false;
                    self.hold_for_missing_root(RootSide::Local, now)?;
                }
                RootStatus::Replaced { found, .. } => {
                    self.reconcile_after_root_check = false;
                    self.hold_for_replaced_root(RootSide::Local, found, now)?;
                }
                RootStatus::Unreachable(_) => {}
            }
        }
        if self.cloud_root_ready
            && self
                .root_hold
                .as_ref()
                .is_none_or(|hold| hold.side != crate::root_identity::RootSide::Cloud)
            && let Some(outcome) = self.harvest_or_start_cloud_probe(now_inst, interval)
        {
            match outcome {
                CloudRootOutcome::Ready { .. } => {
                    if self.release_sign_in_hold(now) {
                        self.restore_run_state_after_root_recovery("signed in again");
                    }
                }
                CloudRootOutcome::SignInRequired(message) => {
                    self.mark_sign_in_required(&message, now);
                }
                CloudRootOutcome::Unreachable(message) => logging::debug(
                    "Cloud root check failed; keeping the last known state",
                    &[("error", message)],
                ),
                CloudRootOutcome::Missing => {
                    self.mark_cloud_root_unavailable("the cloud root is missing", now);
                    self.hold_for_missing_root(crate::root_identity::RootSide::Cloud, now)?;
                }
                CloudRootOutcome::Replaced { found } => {
                    self.hold_for_replaced_root(crate::root_identity::RootSide::Cloud, found, now)?;
                }
            }
        }
        Ok(())
    }

    /// Drains any pause / resume / flush / reconcile requests recorded
    /// on the attached `RuntimeControl` and applies them. Called at
    /// the top of every tick.
    fn apply_pending_control_requests(
        &mut self,
        now: SystemTime,
    ) -> Result<(), DaemonRuntimeError> {
        // Snapshot the pending requests up-front so we can drop the
        // immutable borrow on `self.runtime_control` before mutating
        // `self` via `enqueue_startup_reconstruction_reconcile`.
        let (pause_request, reconcile_request) = match self.runtime_control.as_ref() {
            Some(control) => (
                control.take_pause_request(),
                control.take_reconcile_request(),
            ),
            None => (None, false),
        };
        // `vapor reconcile` is the user's own request: a whole-scope
        // scan now, plus the boost that releases deferred work, so what
        // the scan finds moves at once.
        if reconcile_request {
            if let Some(timeline) = &self.timeline {
                timeline.push(
                    constants::timeline::KIND_RECONCILE,
                    self.profile_id.as_str(),
                    "reconcile requested by the user",
                    now,
                );
            }
            logging::info("Reconcile requested by the user", &[]);
        }

        if let Some(pause) = pause_request {
            self.user_paused = pause;
            if pause {
                self.app
                    .set_run_state(RunState::Paused, "user paused via vapor pause");
            } else {
                // The pause ends. The state then says what still stops
                // work, if anything does: a sign-in, a question about a
                // root or about a one-way mode, a cloud root that is not
                // there, a full cloud. Reporting Running over one of
                // those would hide the real blocker, since the tick loop
                // still leases nothing.
                self.app
                    .set_run_state(RunState::Running, "user resumed via vapor resume");
                self.restore_run_state_after_root_recovery("user resumed via vapor resume");
            }
        }
        if reconcile_request {
            // Execution still answers to the normal throttle ladder, so
            // device-impact invariants hold; deferred reconciles release
            // at once and the cloud feed polls on the next tick.
            let window = Duration::from_secs(constants::engine::RECONCILE_BOOST_SECONDS);
            self.reconcile_boost_until_inst = Some(self.clock.now() + window);
            self.remote_poller.request_immediate_poll();
            logging::info(
                "Reconcile boost activated",
                &[(
                    "window_seconds",
                    constants::engine::RECONCILE_BOOST_SECONDS.to_string(),
                )],
            );
        }
        if reconcile_request {
            self.enqueue_startup_reconstruction_reconcile(now)?;
            self.try_uploads_that_wait_for_room(now)?;
            // What waits in a retry backoff is tried now: the user may
            // have fixed its cause, and asked for a sync to see.
            let woken = self.state_db.wake_all_retries(now)?;
            if woken > 0 {
                logging::info(
                    "Trying the waiting retries at once on the user's request",
                    &[("retries", woken.to_string())],
                );
            }
        }
        Ok(())
    }

    /// Uploads that waited for room in the cloud when the daemon
    /// stopped still wait: the queue rows say so. The state says it
    /// from the start, not only when the next upload meets the full
    /// cloud.
    fn recall_uploads_that_wait_for_room(&mut self) -> Result<(), DaemonRuntimeError> {
        if let Some(message) = self.state_db.wait_for_cloud_room_message()? {
            self.cloud_full = Some((message, self.clock.now()));
        }
        Ok(())
    }

    /// The user asked for sync now while uploads wait for room in the
    /// cloud. The user may have just made that room, so the uploads are
    /// tried at once and the state stops saying that they wait. A cloud
    /// that is still full says so again at the first upload.
    fn try_uploads_that_wait_for_room(
        &mut self,
        now: SystemTime,
    ) -> Result<(), DaemonRuntimeError> {
        let released = self.state_db.end_wait_for_cloud_room(now)?;
        if self.cloud_full.take().is_none() && released == 0 {
            return Ok(());
        }
        logging::info(
            "Trying the uploads that wait for room in the cloud, on the user's request",
            &[
                ("profile_id", self.profile_id.clone()),
                ("uploads", released.to_string()),
            ],
        );
        if let Some(timeline) = &self.timeline {
            timeline.push(
                constants::timeline::KIND_CLOUD_FULL,
                self.profile_id.clone(),
                "Trying the waiting uploads now, on request",
                now,
            );
        }
        self.restore_run_state_after_root_recovery("uploads are tried again on request");
        Ok(())
    }

    fn sample_throttle_inputs(&mut self, now: SystemTime, inputs: ThrottleInputs) {
        // Sampling cadence uses the monotonic clock so wall-clock rewinds
        // cannot force an extra sample (or skip one). The injected clock
        // makes that property test-checkable.
        let now_inst = self.clock.now();
        let should_sample = self
            .last_throttle_sample_inst
            .map(|last| now_inst.saturating_duration_since(last) >= self.throttle_sample_interval)
            .unwrap_or(true);
        if should_sample {
            let slower_direction_kbps = [
                crate::bandwidth::TransferDirection::Upload,
                crate::bandwidth::TransferDirection::Download,
            ]
            .into_iter()
            .filter_map(|direction| {
                self.bandwidth
                    .capacity_bytes_per_second(direction, now_inst)
            })
            .min()
            .map(|bytes_per_second| {
                u32::try_from(bytes_per_second * 8 / 1_000).unwrap_or(u32::MAX)
            });
            self.app.set_link_capacity_kbps(slower_direction_kbps);
            self.app.apply_throttle_inputs(inputs);
            self.last_throttle_sample_inst = Some(now_inst);
            // Loop-prevention TTL sweeps share the 1s sampling cadence
            // (`data-flow.md §Loop prevention`).
            self.local_echoes.purge_expired(now);
            self.remote_echoes.purge_expired(now);

            // Resource budget evaluation on the same
            // cadence: resolve ceilings, scale workgate caps, retarget
            // the bandwidth shaper, and react to the memory ceiling.
            let idle_for = self.idle_notifier.idle_for();
            let throttle_state = self.app.snapshot().throttle_state;
            let ceilings = self
                .resource_budget
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .tick(throttle_state, &inputs, idle_for, now_inst);
            self.app
                .apply_resource_cpu_ceiling(ceilings.cpu_percent, now);
            // The ceiling is a share of what the link carries, and the
            // link is measured: the OS figure is only where the
            // estimate starts.
            self.bandwidth.set_os_reported_kbps(inputs.link_speed_kbps);
            self.bandwidth
                .apply_ceiling(ceilings.bandwidth_percent, now_inst);
            self.apply_memory_ceiling(ceilings.memory_percent, &inputs);
            self.latest_ceilings = Some(ceilings);
            self.last_sampled_inputs = Some(inputs);
        }
    }

    /// Memory-ceiling reactions. The ceiling is a share of device
    /// memory, compared with the resident size the host reports for
    /// the daemon. While the process sits over its share the bounded
    /// caches trim toward their documented floors; they are restored
    /// once it is clearly under (hysteresis at half the share). The
    /// caches are bounded by their own capacity, so a host without a
    /// memory sampler gets no trim: a count of cache entries says
    /// nothing about memory, and a bulk transfer, which fills the echo
    /// caches, is when an evicted echo costs a planning pass with a
    /// hash and a remote probe.
    fn apply_memory_ceiling(&mut self, memory_percent: u8, inputs: &ThrottleInputs) {
        let (Some(resident), Some(device)) =
            (inputs.vapor_memory_bytes, inputs.device_memory_bytes)
        else {
            return;
        };
        if device == 0 {
            return;
        }
        let share = device / 100 * u64::from(memory_percent);

        if resident > share {
            let squeezed_ttl =
                Duration::from_millis(vapor_shared::constants::self_write_cache::MIN_TTL_MILLIS);
            let squeezed_entries = vapor_shared::constants::self_write_cache::MIN_ENTRIES;
            self.local_echoes.set_bounds(squeezed_ttl, squeezed_entries);
            self.remote_echoes
                .set_bounds(squeezed_ttl, squeezed_entries);
            if let Some(timeline) = &self.timeline {
                // Capture the configured capacity once so it can be
                // restored when pressure clears (without it, a single
                // transient squeeze permanently shrank the timeline).
                let configured = *self
                    .timeline_default_entries
                    .get_or_insert(timeline.max_entries());
                timeline.set_max_entries(configured / 4);
            }
        } else if resident < share / 2 {
            let default_ttl = Duration::from_millis(
                vapor_shared::constants::self_write_cache::DEFAULT_TTL_MILLIS,
            );
            let default_entries = vapor_shared::constants::self_write_cache::MAX_ENTRIES;
            self.local_echoes.set_bounds(default_ttl, default_entries);
            self.remote_echoes.set_bounds(default_ttl, default_entries);
            if let (Some(timeline), Some(default)) =
                (&self.timeline, self.timeline_default_entries.take())
            {
                timeline.set_max_entries(default);
            }
        }
    }

    fn enqueue_startup_reconstruction_reconcile(
        &mut self,
        now: SystemTime,
    ) -> Result<(), DaemonRuntimeError> {
        let Some(local_root) = self.sync_scope.local_root.as_ref() else {
            return Ok(());
        };

        // The queue holds one whole-scope reconcile at a time, so a
        // request made while one walks joins it. The walk already
        // compared some folders, and the request is for a scan from
        // now: a rule that changed, events that were dropped, the
        // user's own `vapor reconcile`.
        if self
            .reconcile_walker
            .as_ref()
            .is_some_and(|walker| walker.is_whole_scope())
        {
            self.whole_scope_rescan_wanted = true;
        }
        self.state_db
            .enqueue_startup_reconcile_intent(local_root, now)?;
        self.startup_reconstruction_barrier = true;
        // Monotonic deadline: a wall-clock rewind must not extend the
        // barrier (discipline, same as every other elapsed check).
        self.startup_barrier_expires_inst = Some(self.clock.now() + self.startup_barrier_deadline);
        logging::info(
            "Queued startup whole-scope reconcile for volatile-state reconstruction",
            &[
                ("root", local_root.display().to_string()),
                (
                    "barrier_deadline_ms",
                    self.startup_barrier_deadline.as_millis().to_string(),
                ),
            ],
        );
        Ok(())
    }

    /// Events can be lost: a watcher that dropped some, a folder the
    /// walk could not read, a provider feed with a gap. A daemon that
    /// runs for weeks would carry each such miss until its next
    /// restart. One whole-scope scan a day bounds that. The scan is the
    /// quick check, so an unchanged tree costs one listing a folder.
    /// It waits while the throttle admits no scan.
    fn queue_whole_scope_scan_when_due(
        &mut self,
        now: SystemTime,
    ) -> Result<(), DaemonRuntimeError> {
        let Some(local_root) = self.sync_scope.local_root.clone() else {
            return Ok(());
        };
        let Some(last) = self.last_whole_scope_scan_at else {
            self.last_whole_scope_scan_at = Some(now);
            return Ok(());
        };
        let interval =
            Duration::from_secs(constants::engine::WHOLE_SCOPE_RECONCILE_INTERVAL_SECONDS);
        // A wall clock that went back says nothing about the age.
        let due = now
            .duration_since(last)
            .map(|age| age >= interval)
            .unwrap_or(false);
        if !due || !self.app.reconcile_admissible() {
            return Ok(());
        }
        self.state_db.enqueue_intents_coalesced(
            &[(local_root.clone(), PendingIntentKind::ReconcileSubtree, now)],
            crate::safeguards::IntentSource::ReconcileBacklog,
        )?;
        self.last_whole_scope_scan_at = Some(now);
        logging::info(
            "Queued the daily whole-scope reconcile",
            &[("root", local_root.display().to_string())],
        );
        Ok(())
    }

    fn evaluate_startup_barrier(&mut self, _now: SystemTime) {
        if !self.startup_reconstruction_barrier {
            return;
        }

        let Some(expires_inst) = self.startup_barrier_expires_inst else {
            return;
        };

        if self.clock.now() < expires_inst {
            return;
        }

        self.startup_reconstruction_barrier = false;
        self.startup_barrier_expires_inst = None;
        logging::info(
            "Cleared startup reconstruction barrier after deadline; allowing non-reconcile work to proceed",
            &[(
                "barrier_deadline_ms",
                self.startup_barrier_deadline.as_millis().to_string(),
            )],
        );
    }

    fn release_ready_deferred_reconciles(&mut self, now: SystemTime) -> usize {
        let Some(recorder) = &self.recorder else {
            return 0;
        };

        // The reconcile boost: while it runs, deferred reconciles
        // release at once, whatever their not-before time. Execution
        // still answers to the throttle ladder; the boost only moves
        // work from "deferred" to "queued".
        if self.reconcile_boost_active() {
            let scheduler = &mut self.scheduler;
            return recorder.with_mut_state(|maps| {
                let released = maps.take_all_deferred_reconcile_intents(now);
                for intent in &released {
                    scheduler.upsert_pending_intent_record(intent.clone());
                }
                released.len()
            });
        }

        let app = &mut self.app;
        let scheduler = &mut self.scheduler;
        recorder.with_mut_state(|maps| app.release_ready_deferred_reconciles(maps, scheduler, now))
    }

    fn reconcile_boost_active(&self) -> bool {
        self.reconcile_boost_until_inst
            .map(|until| self.clock.now() < until)
            .unwrap_or(false)
    }

    fn drain_pending_intents(&mut self) -> usize {
        let Some(recorder) = &self.recorder else {
            return 0;
        };

        let pending_intents = recorder.with_mut_state(|maps| maps.drain_pending_intents());
        let count = pending_intents.len();
        for intent in pending_intents {
            self.scheduler.upsert_pending_intent_record(intent);
        }
        count
    }

    fn stabilize_events(&mut self, now: SystemTime) -> (usize, usize, usize) {
        self.stabilize_events_with(now, false)
    }

    /// With `force`, every pending event stabilizes now regardless of
    /// its quiet window (shutdown flush).
    fn stabilize_events_with(&mut self, now: SystemTime, force: bool) -> (usize, usize, usize) {
        let Some(recorder) = self.recorder.clone() else {
            return (0, 0, 0);
        };

        let Some(watch_root) = self.sync_scope.local_root.clone() else {
            return (0, 0, 0);
        };
        let watch_root = watch_root.as_path();

        // While the ignore rules are being read again (the first time,
        // or after an ignore file changed), no event becomes an intent:
        // the rules in force may still let through a name that the new
        // ones hide. The events wait in the recorder; what a stop loses
        // of them, the startup reconcile finds.
        let path_filter = self.path_filter.clone();
        if path_filter
            .as_ref()
            .is_some_and(|filter| filter.rebuild_pending())
        {
            return (0, 0, 0);
        }

        let stabilized = if force {
            self.debounce.drain_all_for_recorder(&recorder, now)
        } else {
            self.debounce.run_tick_for_recorder(&recorder, now)
        };
        let mut accepted = 0;
        let mut suppressed = 0;
        let mut mirror_reverts = 0;
        for event in stabilized {
            // The watcher bridge let this event through under the rules
            // of its moment; the rules in force now decide.
            if path_filter
                .as_ref()
                .is_some_and(|filter| filter.should_ignore(&event.path))
            {
                continue;
            }
            if !crate::fs_events::resolve_event_path_within_watch_root(watch_root, &event.path) {
                // A link that points out of the root, or a path through
                // one. Links are outside the sync contract, so this is
                // routine and nobody has to act on it.
                logging::debug(
                    "Dropped stabilized event that resolves outside watch root",
                    &[
                        ("watch_root", watch_root.display().to_string()),
                        ("path", event.path.display().to_string()),
                    ],
                );
                continue;
            }
            if is_local_self_write_echo(&mut self.local_echoes, &event, now) {
                suppressed += 1;
                logging::debug(
                    "Suppressed stabilized event as a self-write echo",
                    &[("path", event.path.display().to_string())],
                );
                continue;
            }
            match own_rename_echo(&mut self.local_echoes, &self.state_db, &event, now) {
                OwnRenameEcho::No => {}
                OwnRenameEcho::Yes { changed_file } => {
                    // The event names a path that the daemon renamed.
                    // A file that the user changed under the old path
                    // is under the new one now, and its change syncs
                    // from there.
                    if let Some(changed_file) = changed_file
                        && self.sync_scope.sync_mode != vapor_shared::SyncMode::PullOnly
                    {
                        self.scheduler.upsert_intent(
                            changed_file,
                            PendingIntentKind::Upload,
                            event.last_observed_at,
                        );
                    }
                    suppressed += 1;
                    logging::debug(
                        "Suppressed stabilized event as the report of a rename the daemon made",
                        &[("path", event.path.display().to_string())],
                    );
                    continue;
                }
            }
            let intent_kind = crate::scheduler::intent_kind_for_stabilized_event(&event);
            if event.path == watch_root
                && matches!(
                    event.last_event_kind,
                    FsEventKind::Other | FsEventKind::Removed | FsEventKind::Renamed
                )
            {
                // The watcher lost events (a full kernel queue), or
                // reported the root itself removed or renamed: never a
                // deletion to mirror. The root identity check runs
                // next, and only a root that is still the adopted one
                // gets the whole-scope reconcile; a walk over a
                // replacement folder would read it as deletions.
                logging::info(
                    "The filesystem watcher dropped events or reported the root itself; checking the root, then reconciling",
                    &[("watch_root", watch_root.display().to_string())],
                );
                self.last_root_check_inst = None;
                self.reconcile_after_root_check = true;
                accepted += 1;
                continue;
            }
            if intent_kind != PendingIntentKind::Delete
                && std::fs::symlink_metadata(&event.path).is_ok_and(|metadata| metadata.is_dir())
            {
                // A directory that appeared (created, or renamed in from
                // elsewhere) carries children the watcher never reported
                // as events. Report them ourselves, as the events the
                // watcher would have delivered had the files been created
                // one by one, so they flow through the same debounce,
                // storm compaction, filters and guards as a `cp -r`. A
                // subtree too large to enumerate here becomes a deferred
                // reconcile marker, exactly as a storm would.
                if matches!(self.state_db.sync_index(&event.path), Ok(Some(_)))
                    && let Some(folder) = event.path.parent()
                {
                    // A folder where a synced file was. The file left
                    // this device, and the cloud still has it: the walk
                    // of the folder removes it there, so the files of
                    // the new folder have a place to go.
                    self.scheduler.upsert_intent(
                        folder.to_path_buf(),
                        PendingIntentKind::ReconcileSubtree,
                        event.last_observed_at,
                    );
                }
                let synthesized =
                    self.synthesize_subtree_events(&event.path, event.last_observed_at);
                logging::info(
                    "Directory appeared; reported its children as watcher events",
                    &[
                        ("path", event.path.display().to_string()),
                        ("files", synthesized.to_string()),
                    ],
                );
                accepted += 1;
                continue;
            }
            if self.sync_scope.sync_mode == vapor_shared::SyncMode::PullOnly {
                // Pull-only: local events never produce
                // local-to-remote intents. A local change is divergence
                // from the cloud source of truth, so it schedules a
                // restore-from-cloud for that path instead: the download
                // reverts edits, re-materializes deletions, and removes
                // local-only files when no remote counterpart exists. A
                // folder that appeared was expanded into its files above,
                // so what reaches here is a file or a vanished path.
                self.scheduler.upsert_intent(
                    event.path.clone(),
                    PendingIntentKind::Download,
                    event.last_observed_at,
                );
                mirror_reverts += 1;
                accepted += 1;
                continue;
            }
            self.scheduler
                .upsert_stabilized_event_as(event, intent_kind);
            accepted += 1;
        }
        (accepted, suppressed, mirror_reverts)
    }

    /// Reports the files under a directory that just appeared, as the
    /// events the watcher would have delivered for them. A directory
    /// that brings an ignore file with it (a project folder moved in)
    /// waits: its rules are not in force yet, and the watcher bridge,
    /// which asks for the reload when it sees an ignore file change,
    /// never saw this one. The reload is asked for here, and the files
    /// are reported when it is in. Returns the number of files
    /// reported now.
    fn synthesize_subtree_events(&mut self, directory: &Path, observed_at: SystemTime) -> usize {
        if let Some(filter) = self.path_filter.clone()
            && may_hold_an_ignore_file(directory, &filter)
        {
            filter.request_reload();
            self.folders_waiting_for_rules
                .push((directory.to_path_buf(), observed_at));
            return 0;
        }
        self.report_subtree_events(directory, observed_at)
    }

    /// Walks a directory and records one `Created` event per regular
    /// file underneath it, pruning ignored names the way the watcher
    /// bridge would. Stops at `SYNTHESIZED_SUBTREE_EVENT_CAP` files and
    /// leaves a subtree reconcile marker for the rest. Returns the
    /// number of files reported.
    fn report_subtree_events(&mut self, directory: &Path, observed_at: SystemTime) -> usize {
        let Some(recorder) = self.recorder.clone() else {
            return 0;
        };
        let filter = self.path_filter.clone();
        let ignored = |path: &Path| {
            filter
                .as_ref()
                .is_some_and(|filter| filter.should_ignore(path))
        };
        let mut reported = 0usize;
        let mut pending = vec![directory.to_path_buf()];
        while let Some(current) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if ignored(&path) {
                    continue;
                }
                if metadata.is_dir() {
                    pending.push(path);
                } else if metadata.is_file() {
                    if reported >= constants::engine::SYNTHESIZED_SUBTREE_EVENT_CAP {
                        self.scheduler.upsert_intent(
                            directory.to_path_buf(),
                            PendingIntentKind::ReconcileSubtree,
                            observed_at,
                        );
                        logging::info(
                            "Directory too large to report file by file; scheduled a subtree reconcile",
                            &[
                                ("path", directory.display().to_string()),
                                ("reported", reported.to_string()),
                            ],
                        );
                        return reported;
                    }
                    FsEventRecording::record_event(
                        recorder.as_ref(),
                        FsEventRecord {
                            path,
                            kind: FsEventKind::Created,
                            observed_at,
                        },
                    );
                    reported += 1;
                }
            }
        }
        reported
    }

    /// Shutdown flush: stabilizes every pending local event regardless
    /// of its debounce window and writes the resulting intents to the
    /// durable queue, so nothing the watcher reported lives only in
    /// memory when the process exits. The work a provider reported
    /// since the last tick is stored too. Returns the number of
    /// intents enqueued.
    pub fn flush_for_shutdown(&mut self, now: SystemTime) -> Result<usize, DaemonRuntimeError> {
        self.stabilize_events_with(now, true);
        self.drain_pending_intents();
        let flushed = self.flush_scheduler_to_durable_queue()?;
        // A transfer that ended after the last tick has no outcome in
        // the database. The work its provider owes for it is stored
        // all the same, so the next start does it.
        crate::follow_ups::save(&mut self.state_db, self.app.provider(), now)?;
        Ok(flushed)
    }

    /// Pushes a timeline event for every decision the executor opened
    /// this tick, so surfaces learn about the question right away.
    /// How many times this runtime opened or closed a decision; the
    /// status publisher compares it between ticks.
    pub fn decision_changes(&self) -> u64 {
        self.decision_changes
    }

    fn withdraw_decision(
        &mut self,
        id: i64,
        now: SystemTime,
    ) -> Result<(), crate::state_db::StateDbError> {
        self.decision_changes += 1;
        self.state_db.withdraw_decision(id, now)
    }

    fn announce_decisions(&mut self, ids: &[i64], now: SystemTime) {
        self.decision_changes += ids.len() as u64;
        for id in ids {
            let Ok(Some(decision)) = self.state_db.decision(*id) else {
                continue;
            };
            if let Some(timeline) = &self.timeline {
                timeline.push(
                    constants::timeline::KIND_DECISION,
                    self.profile_id.clone(),
                    format!(
                        "{} Answer with `vapor decisions resolve {} --choose <option>` ({}).",
                        decision.question,
                        decision.id,
                        decision
                            .options
                            .iter()
                            .map(|option| option.key.as_str())
                            .collect::<Vec<_>>()
                            .join(" | ")
                    ),
                    now,
                );
            }
        }
    }

    /// Asks once about each name the walk could not carry, and after a
    /// whole-scope walk withdraws the questions whose name is gone (the
    /// user renamed or removed the file). A name the user chose to
    /// skip is not asked about again.
    fn reconcile_unsyncable_names(
        &mut self,
        found: &[crate::unsyncable::UnsyncableName],
        whole_scope: bool,
        now: SystemTime,
    ) -> Result<(), DaemonRuntimeError> {
        use crate::unsyncable::{DECISION_KIND, OPTION_SKIP, options, question};
        for name in found {
            if self
                .state_db
                .open_decision(DECISION_KIND, Some(&name.shown_path))?
                .is_some()
                || self
                    .state_db
                    .decision_answered(DECISION_KIND, &name.shown_path, OPTION_SKIP)?
            {
                continue;
            }
            let id = self.state_db.create_decision(
                DECISION_KIND,
                crate::state_db::DecisionScope::Path,
                Some(&name.shown_path),
                &question(name),
                &options(),
                &serde_json::json!({
                    "reason": name.reason,
                    "side": if name.in_the_cloud { "cloud" } else { "local" },
                }),
                now,
            )?;
            logging::warning(
                if name.in_the_cloud {
                    "A file in the cloud folder has a name that cannot be synced; asking once"
                } else {
                    "A local file has a name that cannot be synced; asking once"
                },
                &[
                    ("path", name.shown_path.display().to_string()),
                    ("reason", name.reason.clone()),
                    ("decision_id", id.to_string()),
                ],
            );
            self.announce_decisions(&[id], now);
        }
        if !whole_scope {
            return Ok(());
        }
        for decision in self.state_db.decisions(false)? {
            if decision.kind != DECISION_KIND {
                continue;
            }
            let still_there = decision
                .path
                .as_ref()
                .is_some_and(|path| found.iter().any(|name| &name.shown_path == path));
            if still_there {
                continue;
            }
            self.withdraw_decision(decision.id, now)?;
            if let Some(timeline) = &self.timeline {
                timeline.push(
                    constants::timeline::KIND_DECISION,
                    self.profile_id.clone(),
                    format!(
                        "Decision {} (unsyncable-name) withdrawn: the name is gone",
                        decision.id
                    ),
                    now,
                );
            }
        }
        Ok(())
    }

    /// Acts on decisions the user answered through the CLI since the
    /// last tick, and marks each one applied.
    fn apply_resolved_decisions(&mut self, now: SystemTime) -> Result<(), DaemonRuntimeError> {
        let resolved = self.state_db.resolved_unapplied_decisions()?;
        self.decision_retries
            .retain(|id, _| resolved.iter().any(|decision| decision.id == *id));
        for decision in resolved {
            if self
                .decision_retries
                .get(&decision.id)
                .is_some_and(|retry| now < retry.next_attempt)
            {
                continue;
            }
            let choice = decision.choice.clone().unwrap_or_default();
            let summary = match (decision.kind.as_str(), choice.as_str()) {
                (crate::type_mismatch::DECISION_KIND, choice) => {
                    match self.apply_type_mismatch(&decision, choice, now) {
                        Ok(summary) => summary,
                        Err(message) => {
                            self.retry_decision_later(&decision, message, now);
                            continue;
                        }
                    }
                }
                (
                    crate::root_identity::MISSING_DECISION_KIND,
                    crate::root_identity::OPTION_RECREATE,
                )
                | (crate::root_identity::DECISION_KIND, crate::root_identity::OPTION_REATTACH) => {
                    match self.reattach_root(&decision, now) {
                        Ok(summary) => summary,
                        Err(message) => {
                            self.retry_decision_later(&decision, message, now);
                            continue;
                        }
                    }
                }
                (crate::unsyncable::DECISION_KIND, crate::unsyncable::OPTION_SKIP) => {
                    "skipped: the file stays on this device only".to_string()
                }
                (crate::one_way::DECISION_KIND, crate::one_way::OPTION_ENABLE) => {
                    match self.enable_one_way_mode(&decision, now) {
                        Ok(summary) => summary,
                        Err(message) => {
                            self.retry_decision_later(&decision, message, now);
                            continue;
                        }
                    }
                }
                (constants::decision::KIND_CONFLICT, _) => {
                    // `vapor decisions resolve` applies a conflict answer
                    // itself and records it applied; the daemon never
                    // moves conflict files. One still unapplied past the
                    // grace period belongs to a command that died: ask
                    // again, about the versions there are now.
                    let grace = Duration::from_secs(constants::conflict::ANSWER_GRACE_SECONDS);
                    let stale = decision
                        .resolved_at
                        .and_then(|at| now.duration_since(at).ok())
                        .is_some_and(|age| age >= grace);
                    if stale {
                        self.state_db.reopen_decision(decision.id)?;
                        self.decision_changes += 1;
                        logging::info(
                            "Reopened a conflict question whose answer was never applied",
                            &[("decision_id", decision.id.to_string())],
                        );
                    }
                    continue;
                }
                (kind, choice) => {
                    // A kind this runtime does not know how to apply is
                    // left answered-but-unapplied for a build that does;
                    // it is reported, never silently consumed.
                    let message = format!("this build has no applier for {kind} / {choice}");
                    self.retry_decision_later(&decision, message, now);
                    continue;
                }
            };
            self.decision_retries.remove(&decision.id);
            self.state_db.mark_decision_applied(decision.id, now)?;
            logging::info(
                "Applied a resolved decision",
                &[
                    ("decision_id", decision.id.to_string()),
                    ("kind", decision.kind.clone()),
                    ("choice", choice.clone()),
                    ("outcome", summary.clone()),
                ],
            );
            if let Some(timeline) = &self.timeline {
                timeline.push(
                    constants::timeline::KIND_DECISION,
                    self.profile_id.clone(),
                    format!("Decision {} ({}) {}", decision.id, decision.kind, summary),
                    now,
                );
            }
        }
        Ok(())
    }

    /// Leaves an answer that could not be applied for a later try. The
    /// first failure is a WARNING with the reason; the tries after it
    /// say nothing until the reason changes, so an answer that stays
    /// unappliable costs one log line and one try per wait.
    fn retry_decision_later(
        &mut self,
        decision: &crate::state_db::DecisionRecord,
        error: String,
        now: SystemTime,
    ) {
        let waits = constants::decision::APPLY_RETRY_SECONDS;
        let retry = self.decision_retries.entry(decision.id).or_default();
        let wait = Duration::from_secs(waits[retry.failures.min(waits.len() - 1)]);
        retry.failures += 1;
        retry.next_attempt = now + wait;
        if retry.last_error.as_deref() != Some(error.as_str()) {
            logging::warning(
                "Could not apply an answered decision; it is tried again later",
                &[
                    ("decision_id", decision.id.to_string()),
                    ("kind", decision.kind.clone()),
                    ("choice", decision.choice.clone().unwrap_or_default()),
                    ("error", error.clone()),
                    ("retry_in_seconds", wait.as_secs().to_string()),
                ],
            );
            retry.last_error = Some(error);
        }
    }

    /// A path that is a file on one side and a directory on the other
    /// gets a `type-mismatch` decision, once; both sides stay untouched
    /// until it is answered.
    fn open_type_mismatch_decision(
        &mut self,
        path: &Path,
        now: SystemTime,
    ) -> Result<(), DaemonRuntimeError> {
        use crate::type_mismatch::{DECISION_KIND, options, question};
        if self
            .state_db
            .open_decision(DECISION_KIND, Some(path))?
            .is_some()
        {
            return Ok(());
        }
        let grace = Duration::from_secs(constants::engine::DECISION_APPLY_GRACE_SECONDS);
        if self.state_db.decision_applied_since(
            DECISION_KIND,
            path,
            now.checked_sub(grace).unwrap_or(SystemTime::UNIX_EPOCH),
        )? {
            // An answer is still landing (a delete in flight); asking
            // again would be noise.
            return Ok(());
        }
        let local_is_dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
        let trash_enabled = self
            .trash
            .as_ref()
            .is_some_and(|trash| trash.settings().enabled);
        let id = self.state_db.create_decision(
            DECISION_KIND,
            crate::state_db::DecisionScope::Path,
            Some(path),
            &question(path, local_is_dir, trash_enabled),
            &options(local_is_dir, trash_enabled),
            &serde_json::json!({
                "local": if local_is_dir { "directory" } else { "file" },
                "cloud": if local_is_dir { "file" } else { "directory" },
            }),
            now,
        )?;
        logging::warning(
            "Reconcile found a file/directory type mismatch; asking which side wins",
            &[
                ("path", path.display().to_string()),
                ("decision_id", id.to_string()),
            ],
        );
        self.announce_decisions(&[id], now);
        Ok(())
    }

    /// Applies a `type-mismatch` answer. `keep-both` and `prefer-cloud`
    /// move the local side out of the way (to a conflict name, or into
    /// the trash) and let the cloud side come down; `prefer-local`
    /// removes the cloud side and lets the local side go up. The
    /// local move is echo-suppressed so the watcher's `Removed` never
    /// turns into a delete of the cloud side.
    fn apply_type_mismatch(
        &mut self,
        decision: &crate::state_db::DecisionRecord,
        choice: &str,
        now: SystemTime,
    ) -> Result<String, String> {
        use crate::type_mismatch::{OPTION_KEEP_BOTH, OPTION_PREFER_CLOUD, OPTION_PREFER_LOCAL};
        let path = decision
            .path
            .clone()
            .ok_or_else(|| "decision has no path".to_string())?;
        let summary = match choice {
            OPTION_KEEP_BOTH => {
                let copy = crate::conflict::conflict_copy_path(
                    &path,
                    &self.device_id,
                    now.duration_since(SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0),
                    |candidate| candidate.exists(),
                );
                match std::fs::rename(&path, &copy) {
                    Ok(()) => {
                        self.local_echoes
                            .record_delete(crate::executor::path_key(&path), now);
                        self.state_db
                            .enqueue_intents_coalesced(
                                &[(copy.clone(), PendingIntentKind::Upload, now)],
                                crate::safeguards::IntentSource::Fresh,
                            )
                            .map_err(|error| error.to_string())?;
                        format!(
                            "kept both: the local side moved to {}; the cloud side comes down under the original name",
                            copy.display()
                        )
                    }
                    // The local side is not at the path any more: an
                    // earlier try moved it and failed after that, or the
                    // user moved it. The reconcile below uploads a copy
                    // that is there and brings the cloud side down.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        "kept both: the local side is not at the path any more; the cloud side comes down under the original name"
                            .to_string()
                    }
                    Err(error) => {
                        return Err(format!("cannot rename {} aside: {error}", path.display()));
                    }
                }
            }
            OPTION_PREFER_CLOUD => {
                let trash = self
                    .trash
                    .clone()
                    .ok_or_else(|| "no trash attached".to_string())?;
                match trash.discard_without_copy(&path, constants::trash::REASON_CLOUD_DELETE, now)
                {
                    Ok(_) => {}
                    // A try before this one moved it (the copy below).
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
                        // Only a copy moves it to the trash, and this
                        // thread reads no file: a thread of its own
                        // makes the copy, and the answer is applied at
                        // the next try, which finds the path free.
                        let copied = path.clone();
                        let _ = std::thread::Builder::new()
                            .name("vapor-trash-copy".to_string())
                            .spawn(move || {
                                if let Err(error) = trash.discard(
                                    &copied,
                                    constants::trash::REASON_CLOUD_DELETE,
                                    now,
                                ) {
                                    logging::warning(
                                        "Could not copy the local side of a decision to the trash",
                                        &[
                                            ("path", copied.display().to_string()),
                                            ("error", error.to_string()),
                                        ],
                                    );
                                }
                            });
                        return Err(format!(
                            "{} is being copied to the trash on another volume",
                            path.display()
                        ));
                    }
                    Err(error) => {
                        return Err(format!(
                            "cannot move {} to the trash: {error}",
                            path.display()
                        ));
                    }
                }
                self.local_echoes
                    .record_delete(crate::executor::path_key(&path), now);
                "preferred the cloud side: the local side is in the trash; the cloud side comes down"
                    .to_string()
            }
            OPTION_PREFER_LOCAL => {
                // A Delete for the path removes the cloud side whatever
                // it is (a directory expands into guarded per-entry
                // deletes); a local file then uploads through the
                // reconcile below, and a local directory through the
                // subtree reconcile the completed delete enqueues.
                // Approved: the cloud side was never synced here, so
                // no index row vouches for it; the answer does.
                self.state_db
                    .enqueue_intents_coalesced_with(
                        &[(path.clone(), PendingIntentKind::Delete, now)],
                        crate::safeguards::IntentSource::Fresh,
                        crate::state_db::Approval::NamedPath,
                    )
                    .map_err(|error| error.to_string())?;
                "preferred the local side: the cloud side is being removed; the local side goes up"
                    .to_string()
            }
            other => return Err(format!("no applier for choice {other:?}")),
        };
        // The reconcile that follows materializes the winning side, and
        // the work that failed at the clash is tried now, not at the
        // end of its backoff.
        self.state_db
            .wake_retries_under(&path, now)
            .map_err(|error| error.to_string())?;
        self.enqueue_startup_reconstruction_reconcile(now)
            .map_err(|error| format!("{error:?}"))?;
        Ok(summary)
    }

    /// Applies a `reattach` answer: the folder now at the root becomes
    /// the profile's root, the next whole-scope reconcile merges the
    /// two sides without propagating any deletion, and the hold lifts.
    fn reattach_root(
        &mut self,
        decision: &crate::state_db::DecisionRecord,
        now: SystemTime,
    ) -> Result<String, String> {
        use crate::root_identity::RootSide;
        let side = match decision.evidence["side"].as_str() {
            Some("cloud") => RootSide::Cloud,
            _ => RootSide::Local,
        };
        match side {
            RootSide::Local => {
                let root = self
                    .sync_scope
                    .local_root
                    .clone()
                    .ok_or_else(|| "no local root configured".to_string())?;
                if decision.kind == crate::root_identity::MISSING_DECISION_KIND {
                    std::fs::create_dir_all(&root)
                        .map_err(|error| format!("cannot re-create {}: {error}", root.display()))?;
                }
                if !root.is_dir() {
                    return Err(format!("{} is not there", root.display()));
                }
                crate::root_identity::reattach_local(
                    &mut self.state_db,
                    &root,
                    &self.device_id,
                    now,
                )
                .map_err(|error| error.to_string())?;
            }
            RootSide::Cloud => {
                // Rare and user-triggered: the one provider call the
                // tick thread makes outside startup.
                let cloud_root = self.sync_scope.cloud_root.clone();
                if decision.kind == crate::root_identity::MISSING_DECISION_KIND {
                    self.app
                        .provider()
                        .ensure_cloud_root(cloud_root.as_str())
                        .map_err(|error| error.message)?;
                }
                let identity = self
                    .app
                    .provider()
                    .adopt_root(cloud_root.as_str(), &self.device_id)
                    .map_err(|error| error.message)?;
                crate::root_identity::record_identity(
                    &mut self.state_db,
                    RootSide::Cloud,
                    identity.as_deref().unwrap_or(""),
                    now,
                )
                .map_err(|error| error.to_string())?;
                self.cloud_root_ready = true;
            }
        }
        self.state_db
            .set_state(constants::state::MERGE_WITHOUT_DELETIONS_KEY, "1", now)
            .map_err(|error| error.to_string())?;
        self.forget_the_outage()
            .map_err(|error| error.to_string())?;
        self.root_hold = None;
        self.local_root_changed |= side == RootSide::Local;
        self.enqueue_startup_reconstruction_reconcile(now)
            .map_err(|error| format!("{error:?}"))?;
        self.restore_run_state_after_root_recovery("sync root reattached");
        Ok(format!(
            "{} the {} root; merging without deletions",
            if decision.kind == crate::root_identity::MISSING_DECISION_KIND {
                "re-created"
            } else {
                "reattached"
            },
            side.label()
        ))
    }

    /// A sync root that went away and came back: the deletions the
    /// watchers reported while it was going describe the outage, not
    /// the user, and so does the feed's history across the gap. Both
    /// are dropped; the whole-scope reconcile the caller schedules
    /// merges the two sides instead.
    fn forget_the_outage(&mut self) -> Result<(), StateDbError> {
        let mut dropped = 0;
        for kind in [
            PendingIntentKind::Delete,
            PendingIntentKind::ApplyRemoteDelete,
        ] {
            dropped += self.scheduler.discard_pending_of_kind(kind);
            dropped += self.state_db.discard_queued_of_kind(kind)?;
        }
        if dropped > 0 {
            logging::info(
                "Dropped deletions queued while the sync root was missing",
                &[("count", dropped.to_string())],
            );
        }
        self.remote_poller.discard_history();
        Ok(())
    }

    fn flush_scheduler_to_durable_queue(&mut self) -> Result<usize, DaemonRuntimeError> {
        // Claim everything first, then persist in ONE transaction with
        // per-(path, kind) coalescing against already-pending durable
        // rows: N pending intents cost one fsync instead of N, and a
        // checkpoint-paused reconcile (tracked both in the scheduler and
        // as a requeued durable row) cannot multiply into duplicates.
        let mut claimed = Vec::new();
        while let Some(intent) = self.scheduler.claim_next() {
            claimed.push(intent);
        }
        if claimed.is_empty() {
            return Ok(0);
        }

        // Priority classes: the durable priority_rank column orders
        // leasing across batches; this in-batch stable sort additionally
        // gives key config and code paths the lower durable ids that
        // break ties inside one class, preserving arrival order.
        let windows = crate::debounce::DebounceWindows::default();
        claimed.sort_by_key(|intent| {
            crate::safeguards::intent_priority_rank(windows.classify_path(&intent.path).0)
        });

        let batch: Vec<_> = claimed
            .iter()
            .map(|intent| (intent.path.clone(), intent.kind, intent.last_observed_at))
            .collect();
        let enqueued = self
            .state_db
            .enqueue_intents_coalesced(&batch, crate::safeguards::IntentSource::Fresh)?;

        for intent in &claimed {
            let disposition = self
                .scheduler
                .complete_running(&intent.path)
                .expect("claimed scheduler intent should still exist on completion");
            if disposition == crate::scheduler::CompletionDisposition::RequeuedDirty {
                logging::debug(
                    "Scheduler intent stayed pending after durable enqueue because newer work arrived",
                    &[("path", intent.path.display().to_string())],
                );
            }
        }
        Ok(enqueued)
    }

    /// Leases ready intents and starts them on the staged executor.
    /// Returns the ids of the executions started this call so the tick
    /// can immediately advance them (no dead tick between admission and
    /// planning).
    fn process_ready_queue(
        &mut self,
        now: SystemTime,
        report: &mut RuntimeTickReport,
    ) -> Result<Vec<i64>, DaemonRuntimeError> {
        self.evaluate_startup_barrier(now);
        let mut started_intent_ids = Vec::new();

        if self.startup_reconstruction_barrier && self.running_reconcile_intent_id.is_some() {
            return Ok(started_intent_ids);
        }

        // Only `Suspended` refuses a scan. Leasing one to refuse it
        // costs two database writes each tick for as long as the state
        // holds, so a scan that cannot start is left in the queue, and
        // the barrier that waits for it holds nothing: its caps are
        // zero for every other kind of work too.
        let next_ready = self.state_db.peek_next_ready_kind(now)?;
        // Work is about to start on what the local root holds. A root
        // that was swapped since the last periodic check (a volume
        // unmounted, a folder put in its place) holds nothing the
        // profile synced: its files would read as deleted, and its own
        // files as changes. The root is read now, and a root that is
        // not the adopted one holds the profile before anything is
        // leased.
        if next_ready.is_some()
            && let Some(root) = self.sync_scope.local_root.as_deref()
            && self.root_hold.is_none()
            && !crate::root_identity::local_root_is_adopted(&self.state_db, root)
        {
            self.last_root_check_inst = None;
            self.check_roots_if_due(now)?;
            if self.root_hold.is_some() {
                return Ok(started_intent_ids);
            }
        }
        let next_is_reconcile = next_ready
            .map(|kind| kind == PendingIntentKind::ReconcileSubtree)
            .unwrap_or(false);
        // A walk that starts before the changes feed has its cursor can
        // list a folder ahead of it, and a change between the two is in
        // neither. So a scan also waits for the poll that gives the
        // cursor: the first one after a start, or the baseline after an
        // expiry. That poll is under way or due, and its answer ends the
        // wait whatever it is.
        let waits_for_cursor = self.app.reconcile_admissible() && self.walk_waits_for_cursor();
        let reconcile_may_start = self.app.reconcile_admissible() && !waits_for_cursor;
        if !reconcile_may_start {
            if next_is_reconcile || self.startup_reconstruction_barrier {
                let reason = if waits_for_cursor {
                    "the changes feed takes its cursor first".to_string()
                } else {
                    self.app
                        .throttle_decision()
                        .map(|decision| decision.reason.clone())
                        .filter(|reason| !reason.is_empty())
                        .unwrap_or_else(|| "sync is suspended".to_string())
                };
                self.reconcile_wait_reason = Some(format!("waiting: {reason}"));
            }
            if self.startup_reconstruction_barrier {
                return Ok(started_intent_ids);
            }
        }

        let batch_limit = if self.startup_reconstruction_barrier {
            1
        } else {
            let workgate = self.app.workgate_snapshot();
            self.staged_executor
                .admission_capacity(workgate)
                .min(workgate.available_planner_workers())
                + usize::from(
                    self.running_reconcile_intent_id.is_none()
                        && next_is_reconcile
                        && reconcile_may_start,
                )
        };

        // `lease_ready_batch` marks every returned row leased up front, so
        // any early `break` below must hand the unprocessed remainder back
        // to the pending state: an abandoned leased row would sit invisible
        // until the stale-lease sweep.
        let mut leased_batch = self
            .state_db
            .lease_ready_batch(now, batch_limit)?
            .into_iter();
        for intent in leased_batch.by_ref() {
            report.leased_intents += 1;

            match intent.kind {
                PendingIntentKind::ReconcileSubtree => {
                    let is_startup_root_reconcile = self.startup_reconstruction_barrier
                        && self.sync_scope.local_root.as_ref() == Some(&intent.path);
                    if waits_for_cursor {
                        self.requeue_runtime_intent(
                            intent.id,
                            now + blocked_intent_requeue_delay(),
                            "waiting for the cursor of the changes feed",
                        )?;
                        report.requeued_intents += 1;
                        continue;
                    }
                    self.scheduler.upsert_intent(
                        intent.path.clone(),
                        intent.kind,
                        intent.available_at,
                    );
                    match self.app.try_start_reconcile(&mut self.scheduler, now) {
                        Ok(Some(root)) => {
                            self.running_reconcile_intent_id = Some(intent.id);
                            self.reconcile_wait_reason = None;
                            report.started_reconcile_root = Some(root);
                            if is_startup_root_reconcile {
                                break;
                            }
                            continue;
                        }
                        Ok(None) => {
                            // Another scan holds the slot, or the state
                            // turned `Suspended` since the peek.
                            self.scheduler.discard_pending(&intent.path);
                            self.requeue_runtime_intent(
                                intent.id,
                                now + blocked_intent_requeue_delay(),
                                "waiting for the reconcile slot",
                            )?;
                            report.requeued_intents += 1;
                            if self.startup_reconstruction_barrier {
                                break;
                            }
                            continue;
                        }
                        Err(_) => {
                            self.scheduler.discard_pending(&intent.path);
                            self.requeue_runtime_intent(
                                intent.id,
                                now + blocked_intent_requeue_delay(),
                                "waiting for reconcile permit",
                            )?;
                            report.requeued_intents += 1;
                            if self.startup_reconstruction_barrier {
                                break;
                            }
                            continue;
                        }
                    }
                }
                _ => {
                    if self.startup_reconstruction_barrier {
                        self.requeue_runtime_intent(
                            intent.id,
                            now + blocked_intent_requeue_delay(),
                            "waiting for startup reconstruction reconcile",
                        )?;
                        report.requeued_intents += 1;
                        break;
                    }

                    let intent_id = intent.id;
                    match self.try_start_staged_intent(intent, now) {
                        crate::executor::StartDecision::Started => {
                            report.started_staged_intents += 1;
                            started_intent_ids.push(intent_id);
                        }
                        crate::executor::StartDecision::PathBusy => {
                            // Per-path serialization blocks only this
                            // intent (an execution is in flight for the
                            // same path); the rest of the batch keeps
                            // admitting: breaking here would pin every
                            // later-id intent behind one long transfer.
                            self.requeue_runtime_intent(
                                intent_id,
                                now + blocked_intent_requeue_delay(),
                                "waiting for in-flight work on the same path",
                            )?;
                            report.requeued_intents += 1;
                        }
                        crate::executor::StartDecision::AtCapacity => {
                            self.requeue_runtime_intent(
                                intent_id,
                                now + blocked_intent_requeue_delay(),
                                "waiting for planner permit",
                            )?;
                            report.requeued_intents += 1;
                            break;
                        }
                    }
                }
            }
        }

        for abandoned in leased_batch {
            self.requeue_runtime_intent(
                abandoned.id,
                now + blocked_intent_requeue_delay(),
                "requeued unprocessed remainder of an interrupted lease batch",
            )?;
            report.requeued_intents += 1;
        }

        Ok(started_intent_ids)
    }

    fn try_start_staged_intent(
        &mut self,
        intent: DurableIntentRecord,
        now: SystemTime,
    ) -> crate::executor::StartDecision {
        self.staged_executor.try_start(&mut self.app, intent, now)
    }

    /// One bounded chunk of the running reconcile's comparison walk.
    /// Creates (or re-targets) the walker for the currently-running
    /// root; returns whether the walk has finished.
    fn process_reconcile_walk(
        &mut self,
        now: SystemTime,
    ) -> Result<bool, crate::reconcile_walk::WalkError> {
        let Some(scope_root) = self.sync_scope.local_root.clone() else {
            return Ok(true);
        };
        let Some(running_root) = self.app.running_reconcile_root() else {
            return Ok(true);
        };
        let needs_new_walker = self
            .reconcile_walker
            .as_ref()
            .map(|walker| walker.subtree_root() != running_root.as_path())
            .unwrap_or(true);
        if needs_new_walker {
            // A whole-scope walk after a reattached or re-created root
            // merges the two sides and propagates no deletion.
            let merge = running_root == scope_root
                && self
                    .state_db
                    .state(constants::state::MERGE_WITHOUT_DELETIONS_KEY)
                    .ok()
                    .flatten()
                    .is_some();
            if merge {
                logging::info(
                    "Whole-scope reconcile merges without propagating deletions",
                    &[("root", scope_root.display().to_string())],
                );
            }
            // A new walk starts its own list of the names it meets.
            self.unsyncable_seen_by_walk.clear();
            self.reconcile_walker = Some(
                crate::reconcile_walk::ReconcileWalker::new(
                    &scope_root,
                    &running_root,
                    self.path_filter.clone(),
                )
                .with_merge_without_deletions(merge)
                .with_device_id(&self.device_id)
                .with_tags(self.tags.clone()),
            );
        }
        // Bound the chunk by a wall-clock slice so a slow provider's
        // enumerate cannot hold the tick thread for the whole directory
        // budget; the high directory budget lets a fast provider converge
        // a large tree quickly under Idle.
        let clock = self.clock.clone();
        let deadline =
            clock.now() + Duration::from_millis(constants::engine::RECONCILE_SLICE_MILLIS);
        let should_continue = move || clock.now() < deadline;
        // A provider that asked for fewer requests gets one listing at
        // a time, as it gets one transfer at a time.
        let slowed = self
            .app
            .retry_slowdown_until()
            .is_some_and(|until| until > now);
        let listings = if slowed {
            1
        } else {
            listings_in_flight(self.app.snapshot().throttle_state)
        };
        let walker = self
            .reconcile_walker
            .as_mut()
            .expect("walker was just ensured");
        walker.set_listings_in_flight(listings);
        walker.process(
            self.app.provider_handle(),
            &self.provider_call_mode,
            self.sync_scope.sync_mode,
            &mut self.state_db,
            constants::engine::RECONCILE_DIRS_PER_SLICE_IDLE,
            now,
            &should_continue,
        )
    }

    /// Forces this runtime into the blocking `Error` run state with a
    /// reason (used by the multi-profile shell to surface a profile that
    /// cannot run, e.g. an invalid provider, without performing any sync
    /// work for it).
    pub fn set_error_state(&mut self, reason: impl Into<String>) {
        self.app.set_run_state(RunState::Error, reason.into());
    }

    /// Reclaims every shared-workgate permit this runtime holds when it is
    /// being suspended (panic or repeated tick failures): the staged
    /// executor's in-flight sessions and a running reconcile would
    /// otherwise leak their permits for the process lifetime, starving all
    /// other profiles' uploads/hashing/reconciles on the shared workgate.
    pub fn abort_and_release(&mut self, now: SystemTime) {
        self.staged_executor.abort_all(&mut self.app);
        if self.running_reconcile_intent_id.take().is_some() {
            self.app.abort_reconcile(&mut self.scheduler, now);
        }
    }

    fn complete_running_reconcile(&mut self) -> Result<Option<PathBuf>, DaemonRuntimeError> {
        let Some(recorder) = &self.recorder else {
            return Ok(None);
        };

        let app = &mut self.app;
        let scheduler = &mut self.scheduler;
        let completion = recorder.with_mut_state(|maps| app.complete_reconcile(maps, scheduler));
        Ok(completion.map(|completion| completion.root))
    }

    fn requeue_runtime_intent(
        &mut self,
        intent_id: i64,
        available_at: SystemTime,
        last_error: &str,
    ) -> Result<(), DaemonRuntimeError> {
        if !self
            .state_db
            .requeue_leased(intent_id, available_at, Some(last_error))?
        {
            // The row vanished or changed state underneath us: an
            // inconsistency worth loud logging, but not worth failing the
            // tick (and, transitively, the daemon) over one intent.
            logging::warning(
                "Durable intent was not leased during runtime requeue; dropping it",
                &[
                    ("intent_id", intent_id.to_string()),
                    ("reason", last_error.to_string()),
                ],
            );
        }
        Ok(())
    }
}

/// Whether an ignore file whose rules are not in force lies under
/// `directory`, in a place the rules in force do not hide. The look is
/// bounded, because it runs on the tick thread: a tree with more
/// entries than one report covers counts as "may hold one", and waits
/// for a reload of the rules, which walks it on a worker.
fn may_hold_an_ignore_file(
    directory: &Path,
    filter: &crate::fs_events::SharedEventPathFilter,
) -> bool {
    let mut budget = constants::engine::SYNTHESIZED_SUBTREE_EVENT_CAP;
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            if budget == 0 {
                return true;
            }
            budget -= 1;
            let path = entry.path();
            if filter.should_ignore(&path) {
                continue;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(path),
                Ok(kind) if kind.is_file() && filter.is_ignore_file(&path) => {
                    let state = entry
                        .metadata()
                        .map(|metadata| (metadata.modified().ok(), metadata.len()));
                    if !state.is_ok_and(|state| filter.covers(&path, state)) {
                        return true;
                    }
                }
                _ => {}
            }
        }
    }
    false
}

/// Timeline text for a remote name that would alias a local file.
fn name_collision_message(wanted: &Path, existing: &Path) -> String {
    format!(
        "{} exists in the cloud but this filesystem cannot hold it next to {}; \
         both are left untouched until you rename one of them",
        wanted.display(),
        existing.display()
    )
}

/// Cloud listings a reconcile walk asks for at one time under `state`.
/// A throttle state makes the scan slower and never stops it.
fn listings_in_flight(state: vapor_shared::ThrottleState) -> usize {
    use vapor_shared::ThrottleState;
    match state {
        ThrottleState::Idle => constants::engine::RECONCILE_LISTINGS_IN_FLIGHT,
        ThrottleState::Light => constants::engine::RECONCILE_LISTINGS_IN_FLIGHT / 2,
        ThrottleState::Throttled | ThrottleState::Suspended => 1,
    }
}

/// Requeue delay applied when a leased intent cannot start because no
/// permit / slot is available. Coarser than the tick interval so blocked
/// intents don't churn a lease+requeue write pair on every tick.
fn blocked_intent_requeue_delay() -> Duration {
    Duration::from_millis(constants::engine::BLOCKED_INTENT_REQUEUE_DELAY_MILLIS)
}

/// Loop prevention on the local ingest path: decides whether a
/// stabilized local event is an echo of a write/delete the daemon itself
/// performed while applying remote changes.
///
/// Removals correlate by path + recency, and by the state of the path:
/// a path that is gone matches a delete record whatever kind the
/// watcher delivered last. FSEvents reports the daemon's own renames
/// and trash moves as `Renamed`, and a file written seconds before
/// ends its burst with `Modified`. Writes correlate by the size and
/// the modification time of the file as the daemon applied it.
fn is_local_self_write_echo(
    local_echoes: &mut SelfWriteCache,
    event: &crate::debounce::StabilizedEvent,
    now: SystemTime,
) -> bool {
    let key = event.path.to_string_lossy();
    // Gone, or stored under another spelling now: this name left.
    if !crate::name_collision::exactly_present(&event.path) {
        return local_echoes.matches_delete(&key, now);
    }
    let Ok(metadata) = std::fs::symlink_metadata(&event.path) else {
        return local_echoes.matches_delete(&key, now);
    };

    // The path is here. A delete the daemon made at it is over: what
    // is here now came after, and its removal will be a real one.
    local_echoes.forget_delete(&key);

    // A write echo is the file exactly as the daemon applied it: the
    // size and the modification time the apply recorded. The op-id tag
    // alone is not that evidence: it survives later writes, so a
    // tag-only match would suppress a user's edit for the record's
    // whole lifetime. Nothing is read from the file: this runs on the
    // tick thread, in every throttle state, for every downloaded file.
    if !local_echoes.has_write_record(&key, now) {
        return false;
    }
    if !metadata.is_file() {
        return false;
    }
    local_echoes.matches_write_state(&key, metadata.len(), metadata.modified().ok(), now)
}

/// Whether an event is the watcher's report of a rename that the
/// daemon made on this device.
enum OwnRenameEcho {
    No,
    /// `changed_file` is a file under the new path that is not what
    /// the sync index holds for it: the user changed it, or added it,
    /// around the moment of the rename, and the event that said so
    /// named the old path.
    Yes {
        changed_file: Option<PathBuf>,
    },
}

/// Loop prevention for a rename the daemon made (a file or a folder
/// that got another name or place in the cloud). A watcher reports the
/// folder that left and the folder that came, and some report each
/// file in it.
///
/// - An event for a path that is gone, at or under the old path of the
///   rename, is the daemon's own.
/// - An event at or under the new path is the daemon's own when it
///   names the renamed folder, a synced folder in it, or a file that
///   still has the size and the time of its index row. A file that
///   differs from its row is a change of the user, and the event goes
///   on as one. So does a folder that is new to the index.
fn own_rename_echo(
    local_echoes: &mut SelfWriteCache,
    state_db: &DurableStateDb,
    event: &crate::debounce::StabilizedEvent,
    now: SystemTime,
) -> OwnRenameEcho {
    // The common case costs nothing: no rename, no look at the volume.
    if !local_echoes.remembers_a_rename() {
        return OwnRenameEcho::No;
    }
    // A file is as the rename left it while it has the size and the
    // time of its index row.
    let file_as_synced =
        |path: &Path| {
            std::fs::symlink_metadata(path).is_ok_and(|metadata| {
                metadata.is_file()
                    && state_db.sync_index(path).ok().flatten().is_some_and(|row| {
                        row.matches_local(metadata.len(), metadata.modified().ok())
                    })
            })
        };
    if !crate::name_collision::exactly_present(&event.path) {
        let Some(new_path) = local_echoes.new_path_after_own_rename(&event.path, now) else {
            return OwnRenameEcho::No;
        };
        local_echoes.note_suppressed();
        // What the event was about is under the new path now.
        let changed_file = (std::fs::symlink_metadata(&new_path)
            .is_ok_and(|metadata| metadata.is_file())
            && !file_as_synced(&new_path))
        .then_some(new_path);
        return OwnRenameEcho::Yes { changed_file };
    }
    let Some(renamed_to) = local_echoes.own_rename_that_made(&event.path, now) else {
        return OwnRenameEcho::No;
    };
    let is_the_daemons_own = match std::fs::symlink_metadata(&event.path) {
        // The folder that was renamed, or a synced folder in it. A
        // folder that is new to the index came after the rename, and
        // its files are reported as those of any new folder.
        Ok(metadata) if metadata.is_dir() => {
            event.path == renamed_to
                || state_db
                    .sync_index_has_rows_under(&event.path)
                    .unwrap_or(false)
                || matches!(state_db.synced_folder_id(&event.path), Ok(Some(_)))
        }
        Ok(_) => file_as_synced(&event.path),
        Err(_) => false,
    };
    if !is_the_daemons_own {
        return OwnRenameEcho::No;
    }
    local_echoes.note_suppressed();
    OwnRenameEcho::Yes { changed_file: None }
}

/// Resident size as a whole-number share of device memory, rounded up so
/// a running daemon never reports 0% while it holds memory.
fn memory_share_percent(resident: Option<u64>, device: Option<u64>) -> Option<u8> {
    let (resident, device) = (resident?, device?);
    if device == 0 {
        return None;
    }
    Some((resident.saturating_mul(100)).div_ceil(device).min(100) as u8)
}

#[cfg(test)]
#[path = "runtime_cloud_rename_tests.rs"]
mod cloud_rename_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs_events::{FsEventKind, FsEventRecord, FsEventRecording};

    use crate::sync_directories::SyncScope;
    use crate::throttle::ThermalPressure;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::time::Instant;
    use tempfile::TempDir;
    use vapor_providers::inert_stub_provider;

    #[test]
    fn start_queues_whole_scope_reconcile_for_restart_reconstruction() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");

        let runtime = DaemonRuntime::start(
            test_sync_scope(&watch_root),
            state_db,
            inert_stub_provider(),
        )
        .expect("runtime");

        assert_eq!(
            runtime.has_live_watcher(),
            native_watcher_available(),
            "a live watcher exists exactly where the host has a native one"
        );
        assert_eq!(runtime.state_db().queue_depth().expect("queue depth"), 1);
        assert_eq!(
            runtime.state_db().pending_depth().expect("pending depth"),
            1
        );
    }

    #[test]
    fn start_surfaces_missing_native_watcher_instead_of_failing() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");

        let runtime = DaemonRuntime::start(
            test_sync_scope(&watch_root),
            state_db,
            inert_stub_provider(),
        )
        .expect("runtime starts on every host");

        let snapshot = runtime.app().snapshot();
        if native_watcher_available() {
            assert!(runtime.has_live_watcher());
            assert_eq!(snapshot.run_state, RunState::Running);
        } else {
            assert!(!runtime.has_live_watcher());
            assert_eq!(snapshot.run_state, RunState::Error);
            assert!(
                snapshot.reason.contains("no native filesystem watcher"),
                "reason must name the missing capability: {}",
                snapshot.reason
            );
        }
    }

    #[test]
    fn start_skips_whole_scope_reconcile_when_no_local_root_exists() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");

        let runtime = DaemonRuntime::start(
            SyncScope {
                local_root: None,
                cloud_root: "/Vapor".to_string(),
                sync_mode: vapor_shared::SyncMode::TwoWay,
            },
            state_db,
            inert_stub_provider(),
        )
        .expect("runtime");

        assert!(!runtime.has_live_watcher());
        assert_eq!(runtime.state_db().queue_depth().expect("queue depth"), 0);
    }

    #[cfg(unix)]
    #[test]
    fn start_canonicalizes_symlinked_local_sync_root() {
        let temp_dir = TempDir::new().expect("temp dir");
        let real_watch_root = temp_dir.path().join("real-watch");
        let symlink_watch_root = temp_dir.path().join("watch-link");
        std::fs::create_dir_all(&real_watch_root).expect("create real watch root");
        symlink(&real_watch_root, &symlink_watch_root).expect("create watch root symlink");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");

        let runtime = DaemonRuntime::start(
            test_sync_scope(&symlink_watch_root),
            state_db,
            inert_stub_provider(),
        )
        .expect("runtime");

        assert_eq!(
            runtime.sync_scope().local_root,
            Some(
                vapor_shared::paths::canonicalize(&real_watch_root).expect("canonical watch root"),
            )
        );
        let leased = runtime
            .state_db()
            .intent_record(1)
            .expect("load startup reconcile")
            .expect("startup reconcile record");
        assert_eq!(
            leased.path,
            runtime.sync_scope().local_root.clone().unwrap()
        );

        // The second start finds the adopted root through the link.
        drop(runtime);
        let state_db = DurableStateDb::open(&database_path).expect("reopen durable state db");
        let runtime = DaemonRuntime::start(
            test_sync_scope(&symlink_watch_root),
            state_db,
            inert_stub_provider(),
        )
        .expect("a root that is a link is present on the second start");
        assert_eq!(
            runtime.sync_scope().local_root,
            Some(
                vapor_shared::paths::canonicalize(&real_watch_root).expect("canonical watch root"),
            )
        );
    }

    #[test]
    fn start_adds_startup_reconcile_even_when_other_durable_work_exists() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut state_db = DurableStateDb::open(&database_path).expect("open durable state db");

        state_db
            .enqueue_intent(
                &watch_root.join("src/main.rs"),
                PendingIntentKind::Upload,
                timestamp_ms(100),
            )
            .expect("enqueue existing upload intent");

        let runtime = DaemonRuntime::start(
            test_sync_scope(&watch_root),
            state_db,
            inert_stub_provider(),
        )
        .expect("runtime");

        assert_eq!(runtime.state_db().queue_depth().expect("queue depth"), 2);
        assert_eq!(
            runtime.state_db().pending_depth().expect("pending depth"),
            2
        );
    }

    #[test]
    fn startup_reconstruction_reconcile_runs_before_older_durable_uploads() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut state_db = DurableStateDb::open(&database_path).expect("open durable state db");

        state_db
            .enqueue_intent(
                &watch_root.join("src/main.rs"),
                PendingIntentKind::Upload,
                timestamp_ms(100),
            )
            .expect("enqueue existing upload intent");

        let mut runtime = DaemonRuntime::start(
            test_sync_scope(&watch_root),
            state_db,
            inert_stub_provider(),
        )
        .expect("runtime");

        let first_tick = runtime
            .tick_with_inputs(timestamp_ms(1_000), ThrottleInputs::default())
            .expect("runtime tick");

        assert_eq!(
            first_tick.started_reconcile_root,
            runtime.sync_scope().local_root.clone()
        );
        assert_eq!(first_tick.completed_intents, 0);
        assert_eq!(runtime.state_db().leased_depth().expect("leased depth"), 1);
        assert_eq!(
            runtime.state_db().pending_depth().expect("pending depth"),
            1
        );
    }

    #[test]
    fn runtime_tick_moves_stabilized_event_through_scheduler_and_durable_queue() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        let cloud_root = temp_dir.path().join("cloud");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");
        let clock = Arc::new(crate::clock::ManualClock::at_now());
        // A real filesystem provider rooted in the sandbox: the scope's
        // cloud directory names the provider-side root.
        let sync_scope = SyncScope {
            local_root: Some(watch_root.clone()),
            cloud_root: cloud_root.to_string_lossy().into_owned(),
            sync_mode: vapor_shared::SyncMode::TwoWay,
        };
        let mut runtime = DaemonRuntime::build(
            sync_scope,
            EventPathFilterOptions::default(),
            state_db,
            Box::new(vapor_providers::FilesystemProvider::new()),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            false,
        )
        .expect("runtime");
        let runtime_watch_root = runtime
            .sync_scope()
            .local_root
            .clone()
            .expect("runtime watch root");
        // Real payload on disk: the pipeline hashes and uploads actual
        // bytes, no simulator.
        let local_file = runtime_watch_root.join("src/main.rs");
        std::fs::create_dir_all(local_file.parent().unwrap()).expect("dirs");
        std::fs::write(&local_file, b"fn main() {}\n").expect("seed local file");
        let recorder = runtime.recorder.as_ref().expect("runtime recorder");
        FsEventRecording::record_event(
            recorder.as_ref(),
            FsEventRecord {
                path: local_file.clone(),
                kind: FsEventKind::Modified,
                observed_at: timestamp_ms(0),
            },
        );

        // Each scripted tick advances the monotonic clock by 250 ms so the
        // debounce elapsed-time gates fire deterministically. The SystemTime
        // arg keeps documenting the durable wall-clock value recorded in the
        // state DB.
        clock.advance(Duration::from_millis(1_500));
        let first_tick = runtime
            .tick_with_inputs(timestamp_ms(1_500), ThrottleInputs::default())
            .expect("runtime tick");
        assert_eq!(first_tick.stabilized_events, 1);
        assert_eq!(first_tick.durable_enqueues, 1);
        assert_eq!(first_tick.started_staged_intents, 1);

        // Drive follow-up ticks until the pipeline completes the upload.
        // (Stage chaining can complete a small file within its lease
        // tick, so the first tick may already report the completion.)
        let mut completed = first_tick.completed_intents;
        for tick_index in 0..8 {
            clock.advance(Duration::from_millis(250));
            let report = runtime
                .tick_with_inputs(
                    timestamp_ms(1_750 + tick_index * 250),
                    ThrottleInputs::default(),
                )
                .expect("runtime tick");
            completed += report.completed_intents;
            if completed > 0 {
                break;
            }
        }

        assert_eq!(completed, 1);
        assert_eq!(runtime.state_db().queue_depth().expect("queue depth"), 0);
        // The provider holds the real bytes: local→remote propagation is
        // observable, not simulated.
        assert_eq!(
            std::fs::read(cloud_root.join("src/main.rs")).expect("uploaded payload"),
            b"fn main() {}\n"
        );
    }

    #[test]
    fn runtime_starts_multiple_upload_intents_up_to_planner_cap() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        // Durable intent paths must live under the runtime's *canonical*
        // watch root, exactly like real watcher events do.
        let watch_root =
            vapor_shared::paths::canonicalize(&watch_root).expect("canonical watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut state_db = DurableStateDb::open(&database_path).expect("open durable state db");
        let planner_cap = crate::throttle::idle_concurrency();
        for index in 0..planner_cap + 2 {
            state_db
                .enqueue_intent(
                    &watch_root.join(format!("src/file-{index}.rs")),
                    PendingIntentKind::Upload,
                    timestamp_ms(0),
                )
                .expect("enqueue upload intent");
        }
        let mut runtime = DaemonRuntime::build(
            test_sync_scope(&watch_root),
            EventPathFilterOptions::default(),
            state_db,
            inert_stub_provider(),
            Arc::new(StaticMetricsSampler::default()),
            system_clock(),
            false,
        )
        .expect("runtime");

        let first_tick = runtime
            .tick_with_inputs(timestamp_ms(250), ThrottleInputs::default())
            .expect("runtime tick");

        // Admission is capped at the planner-worker tier: two of the
        // ready intents must wait for the next tick. Stage chaining then
        // runs the admitted intents through the stub provider within the
        // same tick, so they complete rather than sit in the planner
        // stage.
        assert_eq!(first_tick.started_staged_intents, planner_cap);
        assert_eq!(first_tick.completed_intents, planner_cap);
        assert_eq!(runtime.state_db().leased_depth().expect("leased depth"), 0);
        assert_eq!(
            runtime.state_db().pending_depth().expect("pending depth"),
            2
        );
    }

    /// A timing guard-rail (`testing-strategy.md`): a flake here means a
    /// saturated host, not a logic failure, and is triaged as such.
    #[test]
    fn timing_guardrail_composed_runtime_tick_stays_under_budget() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");
        let mut runtime = DaemonRuntime::build(
            test_sync_scope(&watch_root),
            EventPathFilterOptions::default(),
            state_db,
            inert_stub_provider(),
            Arc::new(StaticMetricsSampler::default()),
            system_clock(),
            false,
        )
        .expect("runtime");
        let runtime_watch_root = runtime
            .sync_scope()
            .local_root
            .clone()
            .expect("runtime watch root");
        let recorder = runtime.recorder.as_ref().expect("runtime recorder");

        for index in 0..150 {
            FsEventRecording::record_event(
                recorder.as_ref(),
                FsEventRecord {
                    path: runtime_watch_root.join(format!("src/file-{index}.rs")),
                    kind: FsEventKind::Modified,
                    observed_at: timestamp_ms(0),
                },
            );
        }

        let start = Instant::now();
        let report = runtime
            .tick_with_inputs(timestamp_ms(1_500), ThrottleInputs::default())
            .expect("runtime tick");
        let elapsed = start.elapsed();

        assert_eq!(report.stabilized_events, 150);
        assert_eq!(report.durable_enqueues, 150);
        assert!(report.started_staged_intents >= 4);
        // Gross-regression guard-rail (Tier 1), not an SLO. One tick that
        // durably enqueues 150 intents performs 150 SQLite writes, which is
        // fsync-bound and slow on cold-cache CI runners: Windows disk I/O in
        // particular measured ~3s. The bound is deliberately generous so it
        // only trips on an order-of-magnitude regression, never on runner
        // variance; precise timing lives in the Tier-2 perf suite.
        assert!(
            elapsed < Duration::from_secs(10),
            "composed runtime tick took {:?}, expected < 10s",
            elapsed
        );
    }

    #[test]
    fn runtime_reconcile_flow_waits_out_suspension_and_completes_across_ticks() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");
        let clock = Arc::new(crate::clock::ManualClock::at_now());
        let mut runtime = DaemonRuntime::build(
            test_sync_scope(&watch_root),
            EventPathFilterOptions::default(),
            state_db,
            inert_stub_provider(),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            false,
        )
        .expect("runtime");
        let runtime_watch_root = runtime
            .sync_scope()
            .local_root
            .clone()
            .expect("runtime watch root");
        let recorder = runtime.recorder.as_ref().expect("runtime recorder");

        for index in 0..200 {
            FsEventRecording::record_event(
                recorder.as_ref(),
                FsEventRecord {
                    path: runtime_watch_root
                        .join("project/sub")
                        .join(format!("file-{index}.txt")),
                    kind: FsEventKind::Modified,
                    observed_at: timestamp_ms(0),
                },
            );
        }

        clock.advance(Duration::from_secs(31));
        let blocked = runtime
            .tick_with_inputs(
                timestamp_ms(31_000),
                ThrottleInputs {
                    thermal_pressure: crate::throttle::ThermalPressure::Critical,
                    ..ThrottleInputs::default()
                },
            )
            .expect("runtime tick");
        assert_eq!(blocked.released_deferred_reconciles, 0);
        assert!(blocked.started_reconcile_root.is_none());

        // Advance past the throttle dwell window for `Throttled`
        // (MIN_DWELL_THROTTLED_SECONDS = 5 s) before the second
        // evaluation so the controller is allowed to transition back to
        // Idle.
        clock.advance(Duration::from_secs(6));
        let started = runtime
            .tick_with_inputs(timestamp_ms(32_000), ThrottleInputs::default())
            .expect("runtime tick");
        assert_eq!(started.released_deferred_reconciles, 1);
        assert!(started.started_reconcile_root.is_some());
        assert_eq!(runtime.state_db().leased_depth().expect("leased depth"), 1);

        clock.advance(Duration::from_millis(250));
        let completed = runtime
            .tick_with_inputs(timestamp_ms(32_250), ThrottleInputs::default())
            .expect("runtime tick");
        assert!(completed.completed_reconcile_root.is_some());
        assert_eq!(runtime.state_db().queue_depth().expect("queue depth"), 0);
    }

    fn test_sync_scope(watch_root: &std::path::Path) -> SyncScope {
        SyncScope {
            local_root: Some(watch_root.to_path_buf()),
            cloud_root: "/Vapor".to_string(),
            sync_mode: vapor_shared::SyncMode::TwoWay,
        }
    }

    /// Bidirectional tick-harness fixture: a real filesystem
    /// provider with a manually-driven changes feed, composed into a
    /// full `DaemonRuntime`. Tests drive local events through the
    /// recorder and remote events through the feed handle, then tick
    /// with a manual clock.
    struct BidirectionalFixture {
        _temp: TempDir,
        watch_root: PathBuf,
        cloud_root: PathBuf,
        runtime: DaemonRuntime,
        feed: vapor_providers::filesystem::ManualFeedHandle,
        clock: Arc<crate::clock::ManualClock>,
        now_ms: u64,
        /// The local tree as the stand-in watcher last saw it; `None`
        /// while the test records every event by hand.
        watched_tree: Option<LocalTreeState>,
    }

    /// Path to (is a directory, size, modification time).
    pub(super) type LocalTreeState =
        std::collections::BTreeMap<PathBuf, (bool, u64, Option<SystemTime>)>;

    pub(super) fn local_tree_state(root: &std::path::Path) -> LocalTreeState {
        let mut state = LocalTreeState::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if metadata.is_dir() {
                    pending.push(path.clone());
                }
                state.insert(
                    path,
                    (metadata.is_dir(), metadata.len(), metadata.modified().ok()),
                );
            }
        }
        state
    }

    impl BidirectionalFixture {
        fn new() -> Self {
            Self::new_with_mode(vapor_shared::SyncMode::TwoWay)
        }

        /// Turns on the stand-in watcher: before and after each tick
        /// the local tree is compared with what was last seen, and
        /// each difference is delivered the way FSEvents delivers it.
        /// That covers the writes the daemon makes itself (a rename
        /// to a conflict copy, a move to the trash, a download put in
        /// place), which a test that records events by hand leaves
        /// out. A path that went away arrives as `Renamed`, because
        /// the daemon renames and never unlinks; a path that appeared
        /// or changed arrives as `Renamed` then `Modified`, the order
        /// a file written seconds before ends its burst in.
        fn watching_the_local_tree(mut self) -> Self {
            self.watched_tree = Some(local_tree_state(&self.watch_root));
            self
        }

        fn deliver_local_tree_changes(&mut self) {
            let Some(seen) = self.watched_tree.take() else {
                return;
            };
            let current = local_tree_state(&self.watch_root);
            // The watcher bridge drops what the ignore rules hide, and
            // that covers Vapor's own files: on a host with no extended
            // attributes the op-id tag of a file is a side-file next to
            // it.
            let filter = self.runtime.path_filter.clone();
            let hidden = |path: &std::path::Path| {
                filter
                    .as_ref()
                    .is_some_and(|filter| filter.should_ignore(path))
            };
            for path in seen
                .keys()
                .filter(|path| !current.contains_key(*path) && !hidden(path))
            {
                self.record_local_event(path, FsEventKind::Renamed, self.now_ms);
            }
            for (path, state) in &current {
                if seen.get(path) != Some(state) && !state.0 && !hidden(path) {
                    self.record_local_event(path, FsEventKind::Renamed, self.now_ms);
                    self.record_local_event(path, FsEventKind::Modified, self.now_ms);
                }
            }
            self.watched_tree = Some(current);
        }

        /// A fixture in `sync_mode`. A one-way mode is confirmed the
        /// way the user confirms it, so the profile runs.
        fn new_with_mode(sync_mode: vapor_shared::SyncMode) -> Self {
            let mut fixture = Self::unconfirmed_with_mode(sync_mode);
            if sync_mode != vapor_shared::SyncMode::TwoWay {
                fixture.runtime.acknowledge_one_way_mode(timestamp_ms(0));
            }
            fixture
        }

        /// A fixture in `sync_mode` as the daemon composes it: a
        /// one-way mode still waits for the user.
        fn unconfirmed_with_mode(sync_mode: vapor_shared::SyncMode) -> Self {
            Self::new_with_provider(sync_mode, |provider| Box::new(provider))
        }

        /// A fixture whose provider can be signed out and back in.
        fn new_signing_out(signed_out: bool) -> (Self, Arc<std::sync::atomic::AtomicBool>) {
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(signed_out));
            let handle = flag.clone();
            let fixture =
                Self::new_with_provider(vapor_shared::SyncMode::TwoWay, move |provider| {
                    Box::new(SwitchedProvider {
                        inner: provider,
                        signed_out: flag,
                        downloads_fail: Arc::default(),
                        cloud_full: Arc::default(),
                        offline: Arc::default(),
                        listings_held: Arc::default(),
                    })
                });
            (fixture, handle)
        }

        /// A fixture whose downloads fail with a transient error while
        /// the returned switch is on: a download that takes longer
        /// than the debounce window.
        /// A fixture whose provider refuses uploads, the way a full
        /// cloud does, while the returned switch is set.
        fn new_with_cloud_full_switch() -> (Self, Arc<std::sync::atomic::AtomicBool>) {
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let handle = flag.clone();
            let fixture =
                Self::new_with_provider(vapor_shared::SyncMode::TwoWay, move |provider| {
                    Box::new(SwitchedProvider {
                        inner: provider,
                        signed_out: Arc::default(),
                        downloads_fail: Arc::default(),
                        cloud_full: flag,
                        offline: Arc::default(),
                        listings_held: Arc::default(),
                    })
                });
            (fixture, handle)
        }

        /// A fixture whose provider does not answer a listing while the
        /// returned hold is set.
        fn new_with_listing_hold() -> (Self, Arc<ListingHold>) {
            let hold = Arc::new(ListingHold::default());
            let handle = hold.clone();
            let fixture =
                Self::new_with_provider(vapor_shared::SyncMode::TwoWay, move |provider| {
                    Box::new(SwitchedProvider {
                        inner: provider,
                        signed_out: Arc::default(),
                        downloads_fail: Arc::default(),
                        cloud_full: Arc::default(),
                        offline: Arc::default(),
                        listings_held: hold,
                    })
                });
            (fixture, handle)
        }

        fn new_with_download_switch() -> (Self, Arc<std::sync::atomic::AtomicBool>) {
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let handle = flag.clone();
            let fixture =
                Self::new_with_provider(vapor_shared::SyncMode::TwoWay, move |provider| {
                    Box::new(SwitchedProvider {
                        inner: provider,
                        signed_out: Arc::default(),
                        downloads_fail: flag,
                        cloud_full: Arc::default(),
                        offline: Arc::default(),
                        listings_held: Arc::default(),
                    })
                });
            (fixture, handle)
        }

        fn new_with_provider(
            sync_mode: vapor_shared::SyncMode,
            wrap: impl FnOnce(vapor_providers::FilesystemProvider) -> Box<dyn Provider>,
        ) -> Self {
            let temp = TempDir::new().expect("temp dir");
            let watch_root = temp.path().join("watch");
            let cloud_root = temp.path().join("cloud");
            std::fs::create_dir_all(&watch_root).expect("watch root");
            std::fs::create_dir_all(&cloud_root).expect("cloud root");
            let cloud_root =
                vapor_shared::paths::canonicalize(&cloud_root).expect("canonical cloud root");
            let database_path = temp.path().join("state/vapor.sqlite");
            let state_db = DurableStateDb::open(&database_path).expect("open durable state db");
            let clock = Arc::new(crate::clock::ManualClock::at_now());
            let caps: Arc<dyn vapor_platform::fs_caps::FilesystemCapabilities> =
                Arc::new(vapor_platform::fs_caps::NativeFilesystemCapabilities::for_current_host());
            let (provider, feed) =
                vapor_providers::FilesystemProvider::with_manual_feed(&cloud_root, caps)
                    .expect("manual-feed provider");
            let sync_scope = SyncScope {
                local_root: Some(watch_root.clone()),
                cloud_root: cloud_root.to_string_lossy().into_owned(),
                sync_mode,
            };
            let runtime = DaemonRuntime::build(
                sync_scope,
                EventPathFilterOptions::default(),
                state_db,
                wrap(provider),
                Arc::new(StaticMetricsSampler::default()),
                clock.clone(),
                false,
            )
            .expect("runtime");
            let watch_root = runtime
                .sync_scope()
                .local_root
                .clone()
                .expect("normalized watch root");
            Self {
                _temp: temp,
                watch_root,
                cloud_root,
                runtime,
                feed,
                clock,
                now_ms: 0,
                watched_tree: None,
            }
        }

        /// One tick, advancing both clocks by `advance_ms`.
        fn tick(&mut self, advance_ms: u64) -> RuntimeTickReport {
            // What changed since the last tick was seen back then.
            self.deliver_local_tree_changes();
            self.clock.advance(Duration::from_millis(advance_ms));
            self.now_ms += advance_ms;
            let report = self
                .runtime
                .tick_with_inputs(timestamp_ms(self.now_ms), ThrottleInputs::default())
                .expect("runtime tick");
            self.deliver_local_tree_changes();
            report
        }

        /// Ticks until the durable queue drains and the executor goes
        /// quiet (or the budget runs out). Remote polls gate on a 5s
        /// idle cadence, so ticks advance well past it.
        fn converge(&mut self, max_ticks: usize) -> usize {
            let mut completed = 0;
            for _ in 0..max_ticks {
                let report = self.tick(6_000);
                completed += report.completed_intents;
                let quiet = report.staged_executor.active_total == 0
                    && self.runtime.state_db().queue_depth().expect("depth") == 0;
                if quiet {
                    break;
                }
            }
            completed
        }

        fn record_local_event(&self, path: &std::path::Path, kind: FsEventKind, at_ms: u64) {
            let recorder = self.runtime.recorder.as_ref().expect("recorder");
            FsEventRecording::record_event(
                recorder.as_ref(),
                FsEventRecord {
                    path: path.to_path_buf(),
                    kind,
                    observed_at: timestamp_ms(at_ms),
                },
            );
        }
    }

    /// The filesystem provider behind two switches. While signed out
    /// every call fails the way an expired or revoked token does.
    /// While downloads fail, each download start fails the way a
    /// dropped connection does.
    struct SwitchedProvider {
        inner: vapor_providers::FilesystemProvider,
        signed_out: Arc<std::sync::atomic::AtomicBool>,
        downloads_fail: Arc<std::sync::atomic::AtomicBool>,
        /// While set, each upload is refused the way a full cloud does.
        cloud_full: Arc<std::sync::atomic::AtomicBool>,
        /// While set, a call that got past the sign-in fails the way a
        /// cloud with no route to it does.
        offline: Arc<std::sync::atomic::AtomicBool>,
        /// While held, a listing does not answer: a cloud that is slow.
        listings_held: Arc<ListingHold>,
    }

    #[derive(Default)]
    struct ListingHold {
        held: Mutex<bool>,
        released: Condvar,
    }

    impl ListingHold {
        fn set(&self, held: bool) {
            *self.held.lock().expect("hold") = held;
            self.released.notify_all();
        }

        fn wait(&self) {
            let mut held = self.held.lock().expect("hold");
            while *held {
                held = self.released.wait(held).expect("hold");
            }
        }
    }

    impl SwitchedProvider {
        fn gate(&self) -> Result<(), vapor_providers::ProviderError> {
            if self.signed_out.load(std::sync::atomic::Ordering::SeqCst) {
                Err(vapor_providers::ProviderError::authentication(
                    "authorization is no longer valid (invalid_grant)",
                ))
            } else if self.offline.load(std::sync::atomic::Ordering::SeqCst) {
                Err(vapor_providers::ProviderError::transient(
                    "the cloud is unreachable",
                ))
            } else {
                Ok(())
            }
        }
    }

    impl Provider for SwitchedProvider {
        fn name(&self) -> &'static str {
            self.inner.name()
        }
        fn capabilities(&self) -> vapor_providers::ProviderCapabilities {
            self.inner.capabilities()
        }
        fn content_hash_algorithm(&self) -> vapor_providers::HashAlgorithm {
            self.inner.content_hash_algorithm()
        }
        fn ensure_cloud_root(&self, dir: &str) -> Result<(), vapor_providers::ProviderError> {
            self.gate()?;
            self.inner.ensure_cloud_root(dir)
        }
        fn root_identity(
            &self,
            dir: &str,
        ) -> Result<Option<String>, vapor_providers::ProviderError> {
            self.gate()?;
            self.inner.root_identity(dir)
        }
        fn adopt_root(
            &self,
            dir: &str,
            device_id: &str,
        ) -> Result<Option<String>, vapor_providers::ProviderError> {
            self.gate()?;
            self.inner.adopt_root(dir, device_id)
        }
        fn enumerate(
            &self,
            directory: &vapor_providers::RemotePath,
        ) -> Result<Vec<vapor_providers::RemoteEntry>, vapor_providers::ProviderError> {
            self.gate()?;
            self.listings_held.wait();
            self.inner.enumerate(directory)
        }
        fn stat(
            &self,
            path: &vapor_providers::RemotePath,
        ) -> Result<Option<vapor_providers::RemoteEntry>, vapor_providers::ProviderError> {
            self.gate()?;
            self.inner.stat(path)
        }
        fn content_hash(
            &self,
            path: &vapor_providers::RemotePath,
        ) -> Result<String, vapor_providers::ProviderError> {
            self.gate()?;
            self.inner.content_hash(path)
        }
        fn begin_upload(
            &self,
            request: vapor_providers::UploadRequest,
        ) -> Result<Box<dyn vapor_providers::TransferSession>, vapor_providers::ProviderError>
        {
            self.gate()?;
            if self.cloud_full.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(vapor_providers::ProviderError::storage_full(
                    "the cloud storage is full; free space, then uploads resume",
                ));
            }
            self.inner.begin_upload(request)
        }
        fn begin_download(
            &self,
            request: vapor_providers::DownloadRequest,
        ) -> Result<Box<dyn vapor_providers::TransferSession>, vapor_providers::ProviderError>
        {
            self.gate()?;
            if self
                .downloads_fail
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(vapor_providers::ProviderError::transient(
                    "the connection dropped",
                ));
            }
            self.inner.begin_download(request)
        }
        fn delete(
            &self,
            path: &vapor_providers::RemotePath,
            op_id: &str,
        ) -> Result<(), vapor_providers::ProviderError> {
            self.gate()?;
            self.inner.delete(path, op_id)
        }
        fn move_object(
            &self,
            from: &vapor_providers::RemotePath,
            to: &vapor_providers::RemotePath,
            op_id: &str,
        ) -> Result<(), vapor_providers::ProviderError> {
            self.gate()?;
            self.inner.move_object(from, to, op_id)
        }
        fn poll_changes(
            &self,
            cursor: Option<&str>,
            max: usize,
        ) -> Result<vapor_providers::ChangesPoll, vapor_providers::ProviderError> {
            self.gate()?;
            self.inner.poll_changes(cursor, max)
        }
    }

    /// What a test sees of an [`OwingUploads`] provider: what it has
    /// to tell the engine, and what the engine handed back to it.
    #[derive(Clone, Default)]
    struct OwedWork {
        changes: Arc<Mutex<Vec<vapor_providers::FollowUpChange>>>,
        resumed: Arc<Mutex<Vec<String>>>,
    }

    impl OwedWork {
        fn report(&self, change: vapor_providers::FollowUpChange) {
            self.changes.lock().expect("lock").push(change);
        }

        fn resumed(&self) -> Vec<String> {
            self.resumed.lock().expect("lock").clone()
        }
    }

    /// The filesystem provider, owing one piece of work for each
    /// upload that lands, the way Google Drive owes a check after a
    /// guarded overwrite.
    struct OwingUploads {
        inner: vapor_providers::FilesystemProvider,
        owed: OwedWork,
    }

    struct OwingUpload {
        inner: Box<dyn vapor_providers::TransferSession>,
        record: String,
        owed: OwedWork,
    }

    impl vapor_providers::TransferSession for OwingUpload {
        fn step(
            &mut self,
            max_bytes: u64,
        ) -> Result<vapor_providers::TransferStep, vapor_providers::ProviderError> {
            let step = self.inner.step(max_bytes)?;
            // In the list before the step that ends the transfer
            // returns, as the contract asks.
            if matches!(step, vapor_providers::TransferStep::Completed(_)) {
                self.owed.report(vapor_providers::FollowUpChange::Started(
                    self.record.clone(),
                ));
            }
            Ok(step)
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

    impl Provider for OwingUploads {
        fn name(&self) -> &'static str {
            self.inner.name()
        }
        fn capabilities(&self) -> vapor_providers::ProviderCapabilities {
            self.inner.capabilities()
        }
        fn content_hash_algorithm(&self) -> vapor_providers::HashAlgorithm {
            self.inner.content_hash_algorithm()
        }
        fn ensure_cloud_root(&self, dir: &str) -> Result<(), vapor_providers::ProviderError> {
            self.inner.ensure_cloud_root(dir)
        }
        fn root_identity(
            &self,
            dir: &str,
        ) -> Result<Option<String>, vapor_providers::ProviderError> {
            self.inner.root_identity(dir)
        }
        fn adopt_root(
            &self,
            dir: &str,
            device_id: &str,
        ) -> Result<Option<String>, vapor_providers::ProviderError> {
            self.inner.adopt_root(dir, device_id)
        }
        fn enumerate(
            &self,
            directory: &vapor_providers::RemotePath,
        ) -> Result<Vec<vapor_providers::RemoteEntry>, vapor_providers::ProviderError> {
            self.inner.enumerate(directory)
        }
        fn stat(
            &self,
            path: &vapor_providers::RemotePath,
        ) -> Result<Option<vapor_providers::RemoteEntry>, vapor_providers::ProviderError> {
            self.inner.stat(path)
        }
        fn content_hash(
            &self,
            path: &vapor_providers::RemotePath,
        ) -> Result<String, vapor_providers::ProviderError> {
            self.inner.content_hash(path)
        }
        fn begin_upload(
            &self,
            request: vapor_providers::UploadRequest,
        ) -> Result<Box<dyn vapor_providers::TransferSession>, vapor_providers::ProviderError>
        {
            let record = format!("check {}", request.remote_path);
            Ok(Box::new(OwingUpload {
                inner: self.inner.begin_upload(request)?,
                record,
                owed: self.owed.clone(),
            }))
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
            path: &vapor_providers::RemotePath,
            op_id: &str,
        ) -> Result<(), vapor_providers::ProviderError> {
            self.inner.delete(path, op_id)
        }
        fn move_object(
            &self,
            from: &vapor_providers::RemotePath,
            to: &vapor_providers::RemotePath,
            op_id: &str,
        ) -> Result<(), vapor_providers::ProviderError> {
            self.inner.move_object(from, to, op_id)
        }
        fn poll_changes(
            &self,
            cursor: Option<&str>,
            max: usize,
        ) -> Result<vapor_providers::ChangesPoll, vapor_providers::ProviderError> {
            self.inner.poll_changes(cursor, max)
        }
        fn take_follow_up_changes(&self) -> Vec<vapor_providers::FollowUpChange> {
            std::mem::take(&mut self.owed.changes.lock().expect("lock"))
        }
        fn resume_follow_ups(&self, records: Vec<String>) -> Vec<String> {
            *self.owed.resumed.lock().expect("lock") = records;
            Vec::new()
        }
        fn note_unrecorded_write(
            &self,
            path: &vapor_providers::RemotePath,
            base_hash: &str,
            content_hash: &str,
            op_id: &str,
        ) {
            self.owed.report(vapor_providers::FollowUpChange::Started(
                unrecorded_write_record(path.as_str(), base_hash, content_hash, op_id),
            ));
        }
    }

    /// The record that [`OwingUploads`] owes for a write with no
    /// recorded outcome.
    fn unrecorded_write_record(
        path: &str,
        base_hash: &str,
        content_hash: &str,
        op_id: &str,
    ) -> String {
        format!("check {path} for {content_hash} over {base_hash}, tagged {op_id}")
    }

    /// A fixture whose provider owes work for each upload that lands.
    fn fixture_with_owing_uploads() -> (BidirectionalFixture, OwedWork) {
        let owed = OwedWork::default();
        let handle = owed.clone();
        let fixture =
            BidirectionalFixture::new_with_provider(vapor_shared::SyncMode::TwoWay, |provider| {
                Box::new(OwingUploads {
                    inner: provider,
                    owed,
                })
            });
        (fixture, handle)
    }

    /// The follow-up records in the state database of `runtime`.
    fn stored_follow_ups(runtime: &DaemonRuntime) -> Vec<String> {
        runtime
            .state_db()
            .states_with_prefix(constants::provider::FOLLOW_UP_STATE_KEY_PREFIX)
            .expect("stored follow-ups")
            .into_iter()
            .map(|entry| entry.value)
            .collect()
    }

    #[test]
    fn the_work_a_provider_owes_for_an_upload_is_stored_in_the_tick_of_its_index_row() {
        let (mut fixture, owed) = fixture_with_owing_uploads();
        fixture.tick(6_000);
        let file = fixture.watch_root.join("doc.txt");
        std::fs::write(&file, b"an edit").expect("seed");
        fixture.record_local_event(&file, FsEventKind::Created, fixture.now_ms);

        // At the end of each tick the database holds the index row of
        // the upload and the record of its work, or neither: a daemon
        // that stops between two ticks never has the row alone.
        let mut landed = false;
        for _ in 0..12 {
            fixture.tick(6_000);
            let indexed = fixture
                .runtime
                .state_db()
                .sync_index(&file)
                .expect("index")
                .is_some();
            let stored = stored_follow_ups(&fixture.runtime);
            assert_eq!(
                stored.len(),
                usize::from(indexed),
                "index row: {indexed}, records: {stored:?}"
            );
            if indexed {
                assert_eq!(stored, ["check doc.txt"]);
                landed = true;
                break;
            }
        }
        assert!(landed, "the upload lands");
        assert!(fixture.cloud_root.join("doc.txt").is_file());

        // The record stays while its work waits, and goes when the
        // provider reports the end.
        fixture.tick(6_000);
        assert_eq!(stored_follow_ups(&fixture.runtime), ["check doc.txt"]);
        owed.report(vapor_providers::FollowUpChange::Finished(
            "check doc.txt".to_string(),
        ));
        fixture.tick(6_000);
        assert!(stored_follow_ups(&fixture.runtime).is_empty());
    }

    #[test]
    fn the_work_for_a_write_with_no_recorded_outcome_is_stored_in_the_tick_of_its_index_row() {
        let (mut fixture, owed) = fixture_with_owing_uploads();
        fixture.tick(6_000);
        let file = fixture.watch_root.join("doc.txt");
        std::fs::write(&file, b"base").expect("seed");
        fixture.record_local_event(&file, FsEventKind::Created, fixture.now_ms);
        assert!(fixture.converge(12) >= 1, "the base is synced");
        owed.report(vapor_providers::FollowUpChange::Finished(
            "check doc.txt".to_string(),
        ));
        fixture.tick(6_000);
        assert!(stored_follow_ups(&fixture.runtime).is_empty());
        let indexed = |fixture: &BidirectionalFixture| {
            fixture
                .runtime
                .state_db()
                .sync_index(&file)
                .expect("index")
                .expect("an index row")
                .content_hash
        };
        let base = indexed(&fixture);

        // An edit went up and landed, and the daemon stopped before it
        // stored the outcome: both sides hold the edit, the object has
        // the tag of that upload, the index holds the base and the tag
        // of the upload before, and the upload runs again.
        let lost_op_id = format!(
            "{}77-a0-t1",
            crate::executor::own_op_id_prefix(
                &fixture.runtime.device_id,
                &fixture.runtime.profile_id
            )
        );
        std::fs::write(&file, b"our edit").expect("edit");
        let object = fixture.cloud_root.join("doc.txt");
        std::fs::write(&object, b"our edit").expect("the write");
        OpIdTagStore::new(Arc::new(NativeFilesystemCapabilities::for_current_host()))
            .write_op_id(&object, &lost_op_id)
            .expect("the tag of the write");
        fixture.record_local_event(&file, FsEventKind::Modified, fixture.now_ms);

        // At the end of each tick the database holds the index row of
        // the edit and the record of the work, or neither: a daemon
        // that stops between two ticks never has the row alone.
        let mut converged = false;
        for _ in 0..12 {
            fixture.tick(6_000);
            let now = indexed(&fixture);
            let stored = stored_follow_ups(&fixture.runtime);
            if now == base {
                assert!(stored.is_empty(), "{stored:?}");
                continue;
            }
            assert_eq!(
                stored,
                [unrecorded_write_record("doc.txt", &base, &now, &lost_op_id)]
            );
            converged = true;
            break;
        }
        assert!(converged, "the upload finds its content in the cloud");
    }

    #[test]
    fn a_tick_that_the_database_does_not_save_keeps_neither_the_index_row_nor_the_record() {
        // A fixture with one new file, and the event for it recorded.
        let seeded = || {
            let (mut fixture, owed) = fixture_with_owing_uploads();
            fixture.tick(6_000);
            let file = fixture.watch_root.join("doc.txt");
            std::fs::write(&file, b"an edit").expect("seed");
            fixture.record_local_event(&file, FsEventKind::Created, fixture.now_ms);
            (fixture, owed, file)
        };
        // The clock is in the hands of the test, so the upload lands
        // in the same tick of every run. This run counts to it.
        let (mut counted, _owed, _file) = seeded();
        let mut ticks_to_the_upload = 0;
        while !counted.cloud_root.join("doc.txt").exists() {
            counted.tick(250);
            ticks_to_the_upload += 1;
            assert!(ticks_to_the_upload < 100, "the upload lands");
        }

        let (mut fixture, owed, file) = seeded();
        for _ in 1..ticks_to_the_upload {
            fixture.tick(250);
        }
        assert!(!fixture.cloud_root.join("doc.txt").exists());

        // The commit of the next tick fails, as on a full drive. The
        // batch of that tick is the one that is open here.
        fixture
            .runtime
            .state_db_mut()
            .begin_batch()
            .expect("the batch of the tick");
        fixture
            .runtime
            .state_db_mut()
            .fail_the_commit_of_the_open_batch();
        fixture.clock.advance(Duration::from_millis(250));
        fixture.now_ms += 250;
        let outcome = fixture
            .runtime
            .tick_with_inputs(timestamp_ms(fixture.now_ms), ThrottleInputs::default());
        assert!(matches!(outcome, Err(DaemonRuntimeError::TickNotSaved(_))));

        // The upload landed in that tick, and the provider reported
        // its work. The index row and the record were one write, and
        // it is gone as one: the next start plans the upload again.
        assert!(fixture.cloud_root.join("doc.txt").is_file());
        assert!(owed.changes.lock().expect("lock").is_empty());
        assert!(
            fixture
                .runtime
                .state_db()
                .sync_index(&file)
                .expect("index")
                .is_none()
        );
        assert!(stored_follow_ups(&fixture.runtime).is_empty());
    }

    #[test]
    fn the_stored_work_goes_back_to_the_provider_of_the_next_start_until_it_ends() {
        let (mut fixture, owed) = fixture_with_owing_uploads();
        fixture.tick(6_000);
        let file = fixture.watch_root.join("doc.txt");
        std::fs::write(&file, b"an edit").expect("seed");
        fixture.record_local_event(&file, FsEventKind::Created, fixture.now_ms);
        assert!(fixture.converge(12) >= 1, "the upload lands");
        assert_eq!(stored_follow_ups(&fixture.runtime), ["check doc.txt"]);
        assert!(owed.resumed().is_empty(), "nothing was stored at the start");

        // The provider reports one more piece after the last tick (an
        // upload whose answer came as the daemon stops). A clean stop
        // stores it.
        owed.report(vapor_providers::FollowUpChange::Started(
            "check late.txt".to_string(),
        ));
        fixture
            .runtime
            .flush_for_shutdown(timestamp_ms(fixture.now_ms + 20))
            .expect("shutdown flush");

        // The daemon stops. The next one is built on the same
        // database, with a provider that has nothing in memory.
        let BidirectionalFixture {
            _temp: temp,
            watch_root,
            cloud_root,
            runtime,
            clock,
            now_ms,
            ..
        } = fixture;
        drop(runtime);
        let next = OwedWork::default();
        let build = |owed: OwedWork| {
            let caps: Arc<dyn vapor_platform::fs_caps::FilesystemCapabilities> =
                Arc::new(vapor_platform::fs_caps::NativeFilesystemCapabilities::for_current_host());
            let (provider, _feed) =
                vapor_providers::FilesystemProvider::with_manual_feed(&cloud_root, caps)
                    .expect("manual-feed provider");
            DaemonRuntime::build(
                SyncScope {
                    local_root: Some(watch_root.clone()),
                    cloud_root: cloud_root.to_string_lossy().into_owned(),
                    sync_mode: vapor_shared::SyncMode::TwoWay,
                },
                EventPathFilterOptions::default(),
                DurableStateDb::open(temp.path().join("state/vapor.sqlite")).expect("reopen"),
                Box::new(OwingUploads {
                    inner: provider,
                    owed,
                }),
                Arc::new(StaticMetricsSampler::default()),
                clock.clone(),
                false,
            )
            .expect("the next runtime")
        };
        let mut runtime = build(next.clone());
        assert_eq!(
            next.resumed(),
            ["check doc.txt", "check late.txt"],
            "oldest first"
        );
        assert_eq!(
            stored_follow_ups(&runtime),
            ["check doc.txt", "check late.txt"],
            "a record stays until its work ends"
        );

        // The work of one record ends in this run.
        next.report(vapor_providers::FollowUpChange::Finished(
            "check doc.txt".to_string(),
        ));
        runtime
            .tick_with_inputs(timestamp_ms(now_ms + 6_000), ThrottleInputs::default())
            .expect("tick");
        assert_eq!(stored_follow_ups(&runtime), ["check late.txt"]);

        // One more stop: the record that is left goes to the next
        // provider, and the one that ended does not.
        drop(runtime);
        let last = OwedWork::default();
        let runtime = build(last.clone());
        assert_eq!(last.resumed(), ["check late.txt"]);
        drop(runtime);
    }

    #[test]
    fn a_refused_sign_in_holds_the_queue_and_signing_in_again_resumes_it() {
        let (mut fixture, signed_out) = BidirectionalFixture::new_signing_out(false);
        fixture.tick(6_000);
        let kept = fixture.watch_root.join("kept.txt");
        std::fs::write(&kept, b"synced before the sign-in expired").expect("seed");
        fixture.record_local_event(&kept, FsEventKind::Created, fixture.now_ms);
        assert!(fixture.converge(12) >= 1, "baseline upload");
        let cloud_kept = fixture.cloud_root.join("kept.txt");
        assert!(cloud_kept.is_file());

        // The token expires. A new file and a deletion arrive while the
        // provider refuses every call.
        signed_out.store(true, std::sync::atomic::Ordering::SeqCst);
        let fresh = fixture.watch_root.join("fresh.txt");
        std::fs::write(&fresh, b"written while signed out").expect("seed");
        fixture.record_local_event(&fresh, FsEventKind::Created, fixture.now_ms);
        std::fs::remove_file(&kept).expect("local delete");
        fixture.record_local_event(&kept, FsEventKind::Removed, fixture.now_ms);
        for _ in 0..8 {
            fixture.tick(6_000);
        }

        assert!(fixture.runtime.sign_in_required());
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0,
            "a refused sign-in never fails an intent"
        );
        assert!(
            fixture.runtime.state_db().queue_depth().expect("depth") >= 2,
            "both changes wait in the queue"
        );
        let snapshot = fixture.runtime.app().snapshot();
        assert_eq!(snapshot.run_state, RunState::Error);
        assert!(
            snapshot
                .reason
                .contains("vapor auth login --profile default"),
            "the reason names the command that ends the hold: {}",
            snapshot.reason
        );
        assert!(!fixture.cloud_root.join("fresh.txt").exists());

        // The user signs in again. The next root check gets through,
        // the hold lifts, and both changes land: the deletion made
        // during the hold is not forgotten.
        signed_out.store(false, std::sync::atomic::Ordering::SeqCst);
        fixture.converge(24);
        assert!(!fixture.runtime.sign_in_required());
        assert_eq!(
            fixture.runtime.app().snapshot().run_state,
            RunState::Running
        );
        assert!(fixture.cloud_root.join("fresh.txt").is_file());
        assert!(!cloud_kept.exists(), "the held deletion propagated");
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0
        );
    }

    #[test]
    fn a_sign_in_given_while_the_cloud_is_away_stops_the_ask_for_one() {
        let offline = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signed_out = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (offline_switch, signed_out_switch) = (offline.clone(), signed_out.clone());
        let mut fixture = BidirectionalFixture::new_with_provider(
            vapor_shared::SyncMode::TwoWay,
            move |provider| {
                Box::new(SwitchedProvider {
                    inner: provider,
                    signed_out: signed_out_switch,
                    downloads_fail: Arc::default(),
                    cloud_full: Arc::default(),
                    offline: offline_switch,
                    listings_held: Arc::default(),
                })
            },
        );
        assert!(fixture.runtime.sign_in_required());

        // The user signs in on a device with no route to the cloud.
        offline.store(true, std::sync::atomic::Ordering::SeqCst);
        signed_out.store(false, std::sync::atomic::Ordering::SeqCst);
        for _ in 0..6 {
            fixture.tick(6_000);
        }
        assert!(
            !fixture.runtime.sign_in_required(),
            "the sign-in is stored: nothing asks for it any more"
        );
        let snapshot = fixture.runtime.app().snapshot();
        assert_eq!(snapshot.run_state, RunState::Error);
        assert!(
            snapshot.reason.contains("is unavailable") && !snapshot.reason.contains("sign in"),
            "the reason is the cloud now: {}",
            snapshot.reason
        );

        // The cloud is back: sync runs.
        offline.store(false, std::sync::atomic::Ordering::SeqCst);
        let local = fixture.watch_root.join("draft.txt");
        std::fs::write(&local, b"written after the sign-in").expect("seed");
        fixture.record_local_event(&local, FsEventKind::Created, fixture.now_ms);
        fixture.converge(24);
        assert_eq!(
            fixture.runtime.app().snapshot().run_state,
            RunState::Running
        );
        assert!(fixture.cloud_root.join("draft.txt").is_file());
    }

    #[test]
    fn a_daemon_that_starts_signed_out_waits_for_the_sign_in() {
        let (mut fixture, signed_out) = BidirectionalFixture::new_signing_out(true);
        fixture.runtime.set_profile_id("work");
        assert!(fixture.runtime.sign_in_required());
        let snapshot = fixture.runtime.app().snapshot();
        assert_eq!(snapshot.run_state, RunState::Error);
        assert!(
            snapshot.reason.contains("--profile work"),
            "the startup reason names the profile once it is known: {}",
            snapshot.reason
        );

        let local = fixture.watch_root.join("draft.txt");
        std::fs::write(&local, b"queued at startup").expect("seed");
        fixture.record_local_event(&local, FsEventKind::Created, fixture.now_ms);
        for _ in 0..4 {
            fixture.tick(6_000);
        }
        assert!(!fixture.cloud_root.join("draft.txt").exists());
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0
        );

        signed_out.store(false, std::sync::atomic::Ordering::SeqCst);
        fixture.converge(24);
        assert!(!fixture.runtime.sign_in_required());
        assert!(fixture.cloud_root.join("draft.txt").is_file());
    }

    #[test]
    fn cloud_deletion_of_a_previously_uploaded_file_propagates_locally() {
        // Field-testing repro: local create A (uploads), cloud create B
        // (downloads), local delete B (propagates up), cloud delete A:
        // the final removal must propagate down. Every feed event the
        // real cloud-root watcher would emit is reproduced, including
        // the echoes of Vapor's own provider writes.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);

        // 1. Local create A -> upload. The cloud watcher then sees our
        // own write and echoes Created(A).
        let local_a = fixture.watch_root.join("a.txt");
        std::fs::write(&local_a, b"file a").expect("seed a");
        fixture.record_local_event(&local_a, FsEventKind::Created, fixture.now_ms);
        assert!(fixture.converge(12) >= 1, "A must upload");
        let cloud_a = fixture.cloud_root.join("a.txt");
        assert!(cloud_a.exists());
        fixture
            .feed
            .emit_created(cloud_a.clone(), timestamp_ms(fixture.now_ms));

        // 2. Cloud create B -> download.
        let cloud_b = fixture.cloud_root.join("b.txt");
        std::fs::write(&cloud_b, b"file b").expect("seed b");
        fixture
            .feed
            .emit_created(cloud_b.clone(), timestamp_ms(fixture.now_ms));
        assert!(fixture.converge(12) >= 1, "B must download");
        let local_b = fixture.watch_root.join("b.txt");
        assert!(local_b.exists());

        // 3. Local delete B -> remote delete. The cloud watcher echoes
        // Removed(B) for our own provider delete.
        std::fs::remove_file(&local_b).expect("delete local b");
        fixture.record_local_event(&local_b, FsEventKind::Removed, fixture.now_ms);
        assert!(fixture.converge(12) >= 1, "B's deletion must propagate up");
        assert!(!cloud_b.exists(), "cloud B must be deleted");
        fixture
            .feed
            .emit_removed(cloud_b.clone(), timestamp_ms(fixture.now_ms));
        fixture.converge(12);
        assert!(
            !local_b.exists(),
            "the echo of our own remote delete must not resurrect B"
        );

        // 4. Cloud delete A -> the removal must propagate down.
        std::fs::remove_file(&cloud_a).expect("delete cloud a");
        fixture
            .feed
            .emit_removed(cloud_a.clone(), timestamp_ms(fixture.now_ms));
        fixture.converge(24);
        assert!(
            !local_a.exists(),
            "a cloud deletion of a previously-uploaded file must propagate locally"
        );
    }

    #[test]
    fn deleted_cloud_root_holds_the_profile_and_recreate_reuploads() {
        let mut fixture = BidirectionalFixture::new();
        // Baseline the feed cursor, then sync one file normally.
        fixture.tick(6_000);
        let first = fixture.watch_root.join("kept.txt");
        std::fs::write(&first, b"survives the outage").expect("seed local");
        fixture.record_local_event(&first, FsEventKind::Created, fixture.now_ms);
        assert!(fixture.converge(12) >= 1, "baseline upload must complete");
        assert!(fixture.cloud_root.join("kept.txt").exists());
        assert!(
            fixture
                .cloud_root
                .join(constants::provider::ROOT_MARKER_FILE_NAME)
                .is_file(),
            "adoption wrote the cloud root marker"
        );

        // The user deletes the whole cloud sync root out from under the
        // running daemon. A per-directory watcher (inotify) reports the
        // files inside as removed before the root itself.
        std::fs::remove_dir_all(&fixture.cloud_root).expect("delete cloud root");
        fixture.feed.emit_removed(
            fixture.cloud_root.join("kept.txt"),
            timestamp_ms(fixture.now_ms),
        );

        // New local work cannot reach the provider: the daemon must
        // block (Error state), not finalize failures or spin forever.
        let second = fixture.watch_root.join("during-outage.txt");
        std::fs::write(&second, b"written while root is gone").expect("seed local");
        fixture.record_local_event(&second, FsEventKind::Created, fixture.now_ms);
        let mut blocked = false;
        for _ in 0..12 {
            fixture.tick(1_000);
            if fixture.runtime.app().snapshot().run_state == RunState::Error {
                blocked = true;
                break;
            }
        }
        assert!(blocked, "root loss must surface as the blocked Error state");
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0,
            "root loss must never finalize intents into failed_intents"
        );

        // The root is never re-created on Vapor's own: the profile waits
        // and asks. The reason names the decision.
        for _ in 0..30 {
            fixture.tick(6_000);
        }
        assert!(
            !fixture.cloud_root.exists(),
            "an adopted root is never re-created silently"
        );
        assert!(
            first.is_file(),
            "a missing root is never mirrored as deletions on this device"
        );
        let decision = fixture
            .runtime
            .state_db()
            .open_decision(crate::root_identity::MISSING_DECISION_KIND, None)
            .expect("query")
            .expect("a root-missing decision is open");
        assert_eq!(decision.evidence["side"], "cloud");
        assert!(
            fixture
                .runtime
                .app()
                .snapshot()
                .reason
                .contains(&format!("decision #{}", decision.id)),
            "reason: {}",
            fixture.runtime.app().snapshot().reason
        );

        // The user asks for it back: the root is re-created, adopted
        // afresh, and the reconcile re-uploads local content into it.
        fixture
            .runtime
            .state_db_mut()
            .resolve_decision(decision.id, "recreate", timestamp_ms(fixture.now_ms))
            .expect("resolve");
        let mut recovered = false;
        for _ in 0..30 {
            fixture.tick(6_000);
            if fixture.runtime.app().snapshot().run_state == RunState::Running {
                recovered = true;
                break;
            }
        }
        assert!(recovered, "recreate must bring the profile back to Running");
        fixture.converge(60);
        assert!(
            first.is_file(),
            "the deletion queued while the root was missing is not applied after recreate"
        );
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("kept.txt")).expect("re-uploaded"),
            b"survives the outage",
        );
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("during-outage.txt")).expect("uploaded"),
            b"written while root is gone",
        );
        assert!(
            fixture
                .runtime
                .state_db()
                .open_decision(crate::root_identity::MISSING_DECISION_KIND, None)
                .expect("query")
                .is_none()
        );
    }

    #[test]
    fn a_cloud_root_that_returns_on_its_own_releases_the_hold_without_an_answer() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let first = fixture.watch_root.join("kept.txt");
        std::fs::write(&first, b"kept").expect("seed local");
        fixture.record_local_event(&first, FsEventKind::Created, fixture.now_ms);
        fixture.converge(12);
        // The volume goes away and comes back with the same marker. A
        // per-directory watcher reports the file under it as removed
        // on the way out.
        let parked = fixture._temp.path().join("parked");
        std::fs::rename(&fixture.cloud_root, &parked).expect("unmount");
        fixture.feed.emit_removed(
            fixture.cloud_root.join("kept.txt"),
            timestamp_ms(fixture.now_ms),
        );
        for _ in 0..12 {
            fixture.tick(6_000);
        }
        assert_eq!(fixture.runtime.app().snapshot().run_state, RunState::Error);
        assert!(
            fixture
                .runtime
                .state_db()
                .open_decision(crate::root_identity::MISSING_DECISION_KIND, None)
                .expect("query")
                .is_some()
        );
        std::fs::rename(&parked, &fixture.cloud_root).expect("mount again");
        let mut recovered = false;
        for _ in 0..30 {
            fixture.tick(6_000);
            if fixture.runtime.app().snapshot().run_state == RunState::Running {
                recovered = true;
                break;
            }
        }
        assert!(
            recovered,
            "the original root returning resumes sync on its own"
        );
        fixture.converge(30);
        assert!(
            first.is_file() && fixture.cloud_root.join("kept.txt").is_file(),
            "the removal the feed reported on the way out is the outage's, not the user's"
        );
        let closed = fixture
            .runtime
            .state_db()
            .decisions(true)
            .expect("decisions")
            .into_iter()
            .find(|decision| decision.kind == crate::root_identity::MISSING_DECISION_KIND)
            .expect("the decision is kept in history");
        assert_eq!(closed.choice.as_deref(), Some("withdrawn"));
    }

    #[test]
    fn a_cloud_root_replaced_by_an_empty_folder_asks_before_syncing() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let first = fixture.watch_root.join("kept.txt");
        std::fs::write(&first, b"kept").expect("seed local");
        fixture.record_local_event(&first, FsEventKind::Created, fixture.now_ms);
        fixture.converge(12);
        // A fresh, empty folder appears where the cloud root was.
        std::fs::remove_dir_all(&fixture.cloud_root).expect("delete");
        std::fs::create_dir_all(&fixture.cloud_root).expect("empty folder");
        for _ in 0..12 {
            fixture.tick(6_000);
        }
        assert_eq!(fixture.runtime.app().snapshot().run_state, RunState::Error);
        let decision = fixture
            .runtime
            .state_db()
            .open_decision(crate::root_identity::DECISION_KIND, None)
            .expect("query")
            .expect("a root-replaced decision is open");
        assert_eq!(decision.evidence["side"], "cloud");
        assert!(
            !fixture.cloud_root.join("kept.txt").exists(),
            "nothing is synced into a folder that was not adopted"
        );
        assert!(first.exists(), "and nothing is deleted on this device");

        fixture
            .runtime
            .state_db_mut()
            .resolve_decision(decision.id, "reattach", timestamp_ms(fixture.now_ms))
            .expect("resolve");
        for _ in 0..30 {
            fixture.tick(6_000);
            if fixture.runtime.app().snapshot().run_state == RunState::Running {
                break;
            }
        }
        fixture.converge(60);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("kept.txt")).expect("merged up"),
            b"kept"
        );
        assert!(
            fixture
                .cloud_root
                .join(constants::provider::ROOT_MARKER_FILE_NAME)
                .is_file(),
            "the reattached folder carries a marker now"
        );
    }

    #[test]
    fn remote_create_flows_through_download_pipeline_to_local_file() {
        let mut fixture = BidirectionalFixture::new();
        // First tick baselines the changes-feed cursor.
        fixture.tick(6_000);

        std::fs::create_dir_all(fixture.cloud_root.join("docs")).expect("dirs");
        std::fs::write(
            fixture.cloud_root.join("docs/from-cloud.txt"),
            b"cloud payload",
        )
        .expect("seed remote");
        fixture.feed.emit_created(
            fixture.cloud_root.join("docs/from-cloud.txt"),
            timestamp_ms(fixture.now_ms),
        );

        let completed = fixture.converge(12);
        assert!(completed >= 1, "download intent must complete");
        let local_target = fixture.watch_root.join("docs/from-cloud.txt");
        assert_eq!(
            std::fs::read(&local_target).expect("applied payload"),
            b"cloud payload"
        );
        assert_eq!(
            fixture.runtime.state_db().queue_depth().expect("depth"),
            0,
            "remote apply must complete durably"
        );
    }

    #[test]
    fn remote_delete_flows_through_apply_pipeline_and_removes_local_file() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline

        // Sync the file for real first (download establishes the sync
        // index); only a synced, unmodified file may be deleted by a
        // remote deletion (preservation guard).
        std::fs::write(fixture.cloud_root.join("stale.txt"), b"stale").expect("seed remote");
        fixture.feed.emit_created(
            fixture.cloud_root.join("stale.txt"),
            timestamp_ms(fixture.now_ms),
        );
        fixture.converge(12);
        let local_file = fixture.watch_root.join("stale.txt");
        assert!(local_file.exists(), "download must land first");

        std::fs::remove_file(fixture.cloud_root.join("stale.txt")).expect("cloud delete");
        fixture.feed.emit_removed(
            fixture.cloud_root.join("stale.txt"),
            timestamp_ms(fixture.now_ms),
        );
        let completed = fixture.converge(12);
        assert!(completed >= 1, "apply-remote-delete must complete");
        assert!(!local_file.exists(), "local replica must be removed");
    }

    #[test]
    fn upload_echo_from_remote_feed_is_suppressed_by_loop_prevention() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline

        // Local file → upload completes for real.
        let local_file = fixture.watch_root.join("mine.txt");
        std::fs::write(&local_file, b"my payload").expect("seed local");
        fixture.record_local_event(&local_file, FsEventKind::Created, fixture.now_ms);
        let completed = fixture.converge(12);
        assert!(completed >= 1, "upload must complete");
        let remote_file = fixture.cloud_root.join("mine.txt");
        assert!(remote_file.exists(), "upload must land remotely");

        // The remote watcher would now observe our own write: emit that
        // echo through the feed. Loop prevention must suppress it, no
        // new intents, no re-download.
        fixture
            .feed
            .emit_created(remote_file.clone(), timestamp_ms(fixture.now_ms));
        let report = fixture.tick(6_000);
        assert_eq!(
            report.remote_poll.suppressed_echoes, 1,
            "the upload echo must be suppressed, not re-applied"
        );
        assert_eq!(report.remote_poll.enqueued_intents, 0);
        assert_eq!(fixture.runtime.state_db().queue_depth().expect("depth"), 0);
    }

    #[test]
    fn a_cloud_deletion_is_named_in_the_timeline_with_its_cause() {
        let mut fixture = BidirectionalFixture::new();
        let timeline = crate::timeline::TimelineBuffer::new(64);
        fixture.runtime.attach_timeline(timeline.clone());
        fixture.tick(6_000); // baseline
        let local_file = fixture.watch_root.join("old-plan.txt");
        std::fs::write(&local_file, b"payload").expect("seed local");
        fixture.record_local_event(&local_file, FsEventKind::Created, fixture.now_ms);
        fixture.converge(12);
        std::fs::remove_file(&local_file).expect("local delete");
        fixture.record_local_event(&local_file, FsEventKind::Removed, fixture.now_ms);
        fixture.converge(12);
        assert!(!fixture.cloud_root.join("old-plan.txt").exists());

        let entries = timeline.snapshot(None);
        let entry = entries
            .iter()
            .find(|entry| entry.kind == constants::timeline::KIND_CLOUD_DELETE)
            .expect("the cloud deletion has a timeline entry");
        assert!(entry.message.contains("old-plan.txt"), "{}", entry.message);
        assert!(
            entry.message.contains("it was deleted on this device"),
            "{}",
            entry.message
        );
    }

    #[test]
    fn remote_delete_echo_is_suppressed_after_local_delete_propagates() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline

        // Sync a file, then delete it locally and propagate.
        let local_file = fixture.watch_root.join("shared.txt");
        std::fs::write(&local_file, b"payload").expect("seed local");
        fixture.record_local_event(&local_file, FsEventKind::Created, fixture.now_ms);
        fixture.converge(12);
        assert!(fixture.cloud_root.join("shared.txt").exists());
        std::fs::remove_file(&local_file).expect("local delete");
        fixture.record_local_event(&local_file, FsEventKind::Removed, fixture.now_ms);
        let completed = fixture.converge(12);
        assert!(completed >= 1, "remote delete must complete");
        assert!(!fixture.cloud_root.join("shared.txt").exists());

        // The feed echoes the removal we caused; suppression must catch it.
        fixture.feed.emit_removed(
            fixture.cloud_root.join("shared.txt"),
            timestamp_ms(fixture.now_ms),
        );
        let report = fixture.tick(6_000);
        assert_eq!(report.remote_poll.suppressed_echoes, 1);
        assert_eq!(report.remote_poll.enqueued_intents, 0);
    }

    #[test]
    fn a_file_made_again_after_the_daemons_delete_can_be_deleted_by_the_user() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let cloud_file = fixture.cloud_root.join("again.txt");
        let local_file = fixture.watch_root.join("again.txt");
        std::fs::write(&cloud_file, b"first").expect("seed remote");
        fixture
            .feed
            .emit_created(cloud_file.clone(), timestamp_ms(fixture.now_ms));
        fixture.converge(12);
        assert!(local_file.exists(), "download must land first");

        // The cloud deletes the file; the daemon removes the local copy
        // and remembers that delete as its own for 30 s.
        std::fs::remove_file(&cloud_file).expect("cloud delete");
        fixture
            .feed
            .emit_removed(cloud_file.clone(), timestamp_ms(fixture.now_ms));
        fixture.converge(12);
        assert!(!local_file.exists(), "the local copy is removed");
        let deleted_at_ms = fixture.now_ms;

        // Inside those 30 s the user makes the file again, and it syncs.
        std::fs::write(&local_file, b"second").expect("made again");
        fixture.record_local_event(&local_file, FsEventKind::Created, fixture.now_ms);
        fixture.converge(2);
        assert_eq!(std::fs::read(&cloud_file).expect("uploaded"), b"second");

        // Then the user deletes it. That delete is the user's, not an
        // echo of the daemon's: it must reach the cloud.
        std::fs::remove_file(&local_file).expect("user delete");
        fixture.record_local_event(&local_file, FsEventKind::Removed, fixture.now_ms);
        fixture.converge(2);
        assert!(
            fixture.now_ms - deleted_at_ms
                < vapor_shared::constants::self_write_cache::DEFAULT_TTL_MILLIS,
            "the test stays inside the life of the daemon's delete record"
        );
        assert!(
            !cloud_file.exists(),
            "the user's delete was dropped as an echo of the daemon's earlier one"
        );
    }

    #[test]
    fn download_echo_from_local_watcher_is_suppressed_by_loop_prevention() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline

        // Remote create → download applies locally.
        std::fs::write(fixture.cloud_root.join("inbound.txt"), b"inbound").expect("seed remote");
        fixture.feed.emit_created(
            fixture.cloud_root.join("inbound.txt"),
            timestamp_ms(fixture.now_ms),
        );
        let completed = fixture.converge(12);
        assert!(completed >= 1, "download must complete");
        let local_target = fixture.watch_root.join("inbound.txt");
        assert!(local_target.exists());

        // The local watcher would now observe the daemon's own apply:
        // record that echo. It must be suppressed before the scheduler,
        // no upload back to the cloud.
        fixture.record_local_event(&local_target, FsEventKind::Created, fixture.now_ms);
        let mut suppressed = 0;
        for _ in 0..4 {
            let report = fixture.tick(6_000);
            suppressed += report.suppressed_local_echoes;
            if suppressed > 0 {
                break;
            }
        }
        assert_eq!(suppressed, 1, "the download echo must be suppressed");
        assert_eq!(
            fixture.runtime.state_db().queue_depth().expect("depth"),
            0,
            "no upload intent may be born from the echo"
        );
    }

    #[test]
    fn pull_only_reverts_local_edits_and_removes_local_only_files_without_uploading() {
        // Cloud is authoritative. A local edit converges
        // back to the cloud canonical, local-only content is removed,
        // and nothing is ever uploaded.
        let mut fixture = BidirectionalFixture::new_with_mode(vapor_shared::SyncMode::PullOnly);
        std::fs::write(fixture.cloud_root.join("shared.txt"), b"canonical").expect("seed remote");
        std::fs::write(fixture.watch_root.join("shared.txt"), b"canonical").expect("seed local");
        fixture.tick(6_000); // baseline

        // Local divergence: an edit and a brand-new local-only file.
        std::fs::write(fixture.watch_root.join("shared.txt"), b"local tampering")
            .expect("local edit");
        fixture.record_local_event(
            &fixture.watch_root.join("shared.txt"),
            FsEventKind::Modified,
            fixture.now_ms,
        );
        std::fs::write(fixture.watch_root.join("local-only.txt"), b"L").expect("local only");
        fixture.record_local_event(
            &fixture.watch_root.join("local-only.txt"),
            FsEventKind::Created,
            fixture.now_ms,
        );

        fixture.converge(16);

        assert_eq!(
            std::fs::read(fixture.watch_root.join("shared.txt")).expect("restored"),
            b"canonical",
            "the local edit must be reverted to the cloud canonical"
        );
        assert!(
            !fixture.watch_root.join("local-only.txt").exists(),
            "local-only content must be removed in pull-only"
        );
        // The cloud side is untouched: same single file, same content.
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("shared.txt")).expect("cloud intact"),
            b"canonical"
        );
        assert!(
            !fixture.cloud_root.join("local-only.txt").exists(),
            "pull-only must never upload"
        );
        let (reverts, deletes) = fixture.runtime.mirror_counters();
        assert!(reverts >= 1, "the revert must be observable");
        assert!(deletes >= 1, "the removal must be observable");
    }

    #[test]
    fn pull_only_removes_a_local_only_folder_without_a_folder_download() {
        // A folder created locally in pull-only is local-only content:
        // its files are removed one by one. The folder itself must
        // never become a download (a directory has no payload, and the
        // intent would retry forever, holding the queue open).
        let mut fixture = BidirectionalFixture::new_with_mode(vapor_shared::SyncMode::PullOnly);
        fixture.tick(6_000); // baseline
        let folder = fixture.watch_root.join("scratch");
        std::fs::create_dir_all(&folder).expect("local folder");
        std::fs::write(folder.join("note.txt"), b"local only").expect("local file");
        fixture.record_local_event(&folder, FsEventKind::Created, fixture.now_ms);
        // The folder's event stabilizes on one tick and reports its
        // file as a fresh watcher event, which stabilizes on the next.
        for _ in 0..3 {
            fixture.tick(6_000);
        }
        fixture.converge(16);
        assert!(
            !folder.join("note.txt").exists(),
            "local-only content is removed in pull-only"
        );
        assert_eq!(
            fixture.runtime.state_db().queue_depth().expect("depth"),
            0,
            "no intent may be left retrying"
        );
        assert!(
            !fixture.cloud_root.join("scratch").exists(),
            "pull-only never uploads"
        );
    }

    #[test]
    fn pull_only_restores_a_folder_this_device_removed() {
        // Removing a synced folder locally is divergence from the cloud
        // source of truth: the files come back. The folder's own
        // vanish event reaches the queue as a download of a directory,
        // which resolves into a walk instead of a transfer that can
        // never complete.
        let mut fixture = BidirectionalFixture::new_with_mode(vapor_shared::SyncMode::PullOnly);
        std::fs::create_dir_all(fixture.cloud_root.join("keep")).expect("cloud folder");
        std::fs::write(fixture.cloud_root.join("keep/a.txt"), b"canonical").expect("cloud file");
        std::fs::create_dir_all(fixture.watch_root.join("keep")).expect("local folder");
        std::fs::write(fixture.watch_root.join("keep/a.txt"), b"canonical").expect("local file");
        fixture.tick(6_000); // baseline
        fixture.converge(8);

        std::fs::remove_dir_all(fixture.watch_root.join("keep")).expect("local removal");
        fixture.record_local_event(
            &fixture.watch_root.join("keep/a.txt"),
            FsEventKind::Removed,
            fixture.now_ms,
        );
        fixture.record_local_event(
            &fixture.watch_root.join("keep"),
            FsEventKind::Removed,
            fixture.now_ms,
        );
        fixture.converge(16);
        assert_eq!(
            std::fs::read(fixture.watch_root.join("keep/a.txt")).expect("restored"),
            b"canonical"
        );
        assert_eq!(
            fixture.runtime.state_db().queue_depth().expect("depth"),
            0,
            "the folder's own event must not leave a download retrying"
        );
    }

    #[test]
    fn an_in_place_cloud_append_is_not_taken_for_the_daemons_own_write() {
        // The op-id tag survives an append made in place on the cloud
        // object, so the feed reports a change carrying Vapor's own
        // op-id. The size no longer matches what the upload recorded:
        // this is another writer's edit and it must reach this device.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let local = fixture.watch_root.join("log.txt");
        std::fs::write(&local, b"line one\n").expect("seed local");
        fixture.record_local_event(&local, FsEventKind::Created, fixture.now_ms);
        fixture.converge(16);
        let cloud = fixture.cloud_root.join("log.txt");
        assert_eq!(std::fs::read(&cloud).expect("uploaded"), b"line one\n");

        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&cloud)
                .expect("open the cloud object");
            file.write_all(b"line two\n").expect("append in place");
        }
        fixture
            .feed
            .emit_modified(cloud.clone(), timestamp_ms(fixture.now_ms));
        fixture.converge(16);
        assert_eq!(
            std::fs::read(&local).expect("local copy"),
            b"line one\nline two\n",
            "the cloud append must reach this device"
        );
    }

    #[test]
    fn pull_only_applies_cloud_deletions_locally() {
        let mut fixture = BidirectionalFixture::new_with_mode(vapor_shared::SyncMode::PullOnly);
        std::fs::write(fixture.cloud_root.join("doomed.txt"), b"x").expect("seed remote");
        std::fs::write(fixture.watch_root.join("doomed.txt"), b"x").expect("seed local");
        fixture.tick(6_000); // baseline

        std::fs::remove_file(fixture.cloud_root.join("doomed.txt")).expect("cloud delete");
        fixture.feed.emit_removed(
            fixture.cloud_root.join("doomed.txt"),
            timestamp_ms(fixture.now_ms),
        );
        fixture.converge(12);
        assert!(
            !fixture.watch_root.join("doomed.txt").exists(),
            "a cloud deletion removes the local replica"
        );
    }

    #[test]
    fn push_only_overwrites_remote_divergence_and_removes_cloud_only_files_without_downloading() {
        // Local is authoritative. Remote edits are
        // overwritten with the local canonical, cloud-only content is
        // removed, and nothing is ever downloaded or deleted locally.
        let mut fixture = BidirectionalFixture::new_with_mode(vapor_shared::SyncMode::PushOnly);
        std::fs::write(fixture.cloud_root.join("shared.txt"), b"canonical").expect("seed remote");
        std::fs::write(fixture.watch_root.join("shared.txt"), b"canonical").expect("seed local");
        fixture.tick(6_000); // baseline

        // Remote divergence: a tampered edit and a cloud-only file.
        std::fs::write(fixture.cloud_root.join("shared.txt"), b"remote tampering!")
            .expect("remote edit");
        fixture.feed.emit_modified(
            fixture.cloud_root.join("shared.txt"),
            timestamp_ms(fixture.now_ms),
        );
        std::fs::write(fixture.cloud_root.join("cloud-only.txt"), b"C").expect("cloud only");
        fixture.feed.emit_created(
            fixture.cloud_root.join("cloud-only.txt"),
            timestamp_ms(fixture.now_ms),
        );

        fixture.converge(16);

        assert_eq!(
            std::fs::read(fixture.cloud_root.join("shared.txt")).expect("restored"),
            b"canonical",
            "the remote edit must be overwritten with the local canonical"
        );
        assert!(
            !fixture.cloud_root.join("cloud-only.txt").exists(),
            "cloud-only content must be removed in push-only"
        );
        assert!(
            !fixture.watch_root.join("cloud-only.txt").exists(),
            "push-only must never download"
        );
        assert_eq!(
            std::fs::read(fixture.watch_root.join("shared.txt")).expect("local intact"),
            b"canonical"
        );
        let (reverts, deletes) = fixture.runtime.mirror_counters();
        assert!(reverts >= 1);
        assert!(deletes >= 1);
    }

    #[test]
    fn push_only_puts_back_a_spelling_that_the_cloud_changed() {
        // The cloud renames `report.txt` to `Report.txt`. On a volume
        // that folds case the two are one name, so a restore through
        // the cloud's spelling finds nothing to send, and the upload of
        // the local spelling is refused while the other one is there.
        let mut fixture = BidirectionalFixture::new_with_mode(vapor_shared::SyncMode::PushOnly);
        // Past the startup scan: from here on only the watcher and the
        // feed report changes.
        for _ in 0..20 {
            fixture.tick(6_000);
        }
        std::fs::write(fixture.watch_root.join("report.txt"), b"canonical").expect("local file");
        fixture.record_local_event(
            &fixture.watch_root.join("report.txt"),
            FsEventKind::Created,
            fixture.now_ms,
        );
        fixture.converge(16);
        assert_eq!(names_in(&fixture.cloud_root), vec!["report.txt"]);
        assert_eq!(fixture.runtime.state_db().queue_depth().expect("depth"), 0);

        std::fs::rename(
            fixture.cloud_root.join("report.txt"),
            fixture.cloud_root.join("Report.txt"),
        )
        .expect("cloud rename");
        // What a watcher on a volume that folds case reports: both
        // spellings, and each resolves to the one file that is there.
        fixture.feed.emit_modified(
            fixture.cloud_root.join("report.txt"),
            timestamp_ms(fixture.now_ms),
        );
        fixture.feed.emit_created(
            fixture.cloud_root.join("Report.txt"),
            timestamp_ms(fixture.now_ms),
        );
        for _ in 0..30 {
            fixture.tick(1_000);
        }
        fixture.converge(24);

        assert_eq!(names_in(&fixture.cloud_root), vec!["report.txt"]);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("report.txt")).expect("cloud"),
            b"canonical"
        );
        assert_eq!(names_in(&fixture.watch_root), vec!["report.txt"]);
        assert_eq!(fixture.runtime.state_db().queue_depth().expect("depth"), 0);
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0
        );
    }

    #[test]
    fn push_only_propagates_local_deletions_to_the_cloud() {
        let mut fixture = BidirectionalFixture::new_with_mode(vapor_shared::SyncMode::PushOnly);
        std::fs::write(fixture.cloud_root.join("gone.txt"), b"x").expect("seed remote");
        std::fs::write(fixture.watch_root.join("gone.txt"), b"x").expect("seed local");
        fixture.tick(6_000); // baseline

        std::fs::remove_file(fixture.watch_root.join("gone.txt")).expect("local delete");
        fixture.record_local_event(
            &fixture.watch_root.join("gone.txt"),
            FsEventKind::Removed,
            fixture.now_ms,
        );
        fixture.converge(12);
        assert!(
            !fixture.cloud_root.join("gone.txt").exists(),
            "a local deletion removes the cloud copy"
        );
    }

    #[test]
    fn two_way_mode_keeps_the_one_way_gates_inert() {
        // The default mode still moves both directions and never
        // records a mirror revert/delete.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline

        std::fs::write(fixture.watch_root.join("up.txt"), b"up").expect("seed local");
        fixture.record_local_event(
            &fixture.watch_root.join("up.txt"),
            FsEventKind::Created,
            fixture.now_ms,
        );
        std::fs::write(fixture.cloud_root.join("down.txt"), b"down").expect("seed remote");
        fixture.feed.emit_created(
            fixture.cloud_root.join("down.txt"),
            timestamp_ms(fixture.now_ms),
        );

        fixture.converge(16);
        assert!(fixture.cloud_root.join("up.txt").exists());
        assert!(fixture.watch_root.join("down.txt").exists());
        assert_eq!(
            fixture.runtime.mirror_counters(),
            (0, 0),
            "two-way must never take a strict-mirror action"
        );
    }

    #[test]
    fn mode_change_mid_run_converges_by_dropping_stale_direction_intents() {
        // Intents durably enqueued under the previous mode must
        // not fire after a mode change (restart with new config). Stale
        // Upload intents complete as gated no-ops in pull-only.
        let temp = TempDir::new().expect("temp dir");
        let watch_root = temp.path().join("watch");
        let cloud_root = temp.path().join("cloud");
        std::fs::create_dir_all(&watch_root).expect("watch root");
        std::fs::create_dir_all(&cloud_root).expect("cloud root");
        let watch_root =
            vapor_shared::paths::canonicalize(&watch_root).expect("canonical watch root");
        let database_path = temp.path().join("state/vapor.sqlite");
        let local_file = watch_root.join("pending-upload.txt");
        std::fs::write(&local_file, b"was queued in two-way").expect("seed local");

        // Durably enqueue an Upload intent as a two-way daemon would
        // have, then "restart" into pull-only.
        {
            let mut state_db = DurableStateDb::open(&database_path).expect("open durable state db");
            state_db
                .enqueue_intent(&local_file, PendingIntentKind::Upload, timestamp_ms(0))
                .expect("enqueue");
        }
        let state_db = DurableStateDb::open(&database_path).expect("reopen durable state db");
        let clock = Arc::new(crate::clock::ManualClock::at_now());
        let mut runtime = DaemonRuntime::build(
            SyncScope {
                local_root: Some(watch_root.clone()),
                cloud_root: cloud_root.to_string_lossy().into_owned(),
                sync_mode: vapor_shared::SyncMode::PullOnly,
            },
            EventPathFilterOptions::default(),
            state_db,
            Box::new(vapor_providers::FilesystemProvider::new()),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            false,
        )
        .expect("runtime");
        runtime.acknowledge_one_way_mode(timestamp_ms(0));

        let mut completed = 0;
        for tick_index in 0..8 {
            clock.advance(Duration::from_millis(250));
            let report = runtime
                .tick_with_inputs(
                    timestamp_ms(250 + tick_index * 250),
                    ThrottleInputs::default(),
                )
                .expect("tick");
            completed += report.completed_intents;
            if completed > 0 {
                break;
            }
        }
        assert!(completed >= 1, "the stale intent must complete as a no-op");
        assert!(
            !cloud_root.join("pending-upload.txt").exists(),
            "the gated direction must not fire after the mode change"
        );
    }

    /// Simulates a foreign device editing a remote file: new content,
    /// and no Vapor op-id tag (foreign writers do not tag).
    fn foreign_remote_edit(fixture: &BidirectionalFixture, name: &str, content: &[u8]) {
        let remote_file = fixture.cloud_root.join(name);
        std::fs::write(&remote_file, content).expect("foreign remote edit");
        let tags = vapor_providers::tags::OpIdTagStore::new(Arc::new(
            vapor_platform::fs_caps::NativeFilesystemCapabilities::for_current_host(),
        ));
        let _ = tags.remove(&remote_file);
    }

    fn files_in(directory: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.file_type().map(|t| t.is_file()).unwrap_or(false))
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| !name.starts_with(".vapor-tmp-"))
                    .filter(|name| !name.ends_with(".vapor-meta.json"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Both roots hold a different `alex.txt` written while no daemon
    /// watched; the startup scan is queued the way the composed daemon
    /// queues it at start.
    fn fixture_with_offline_divergence() -> BidirectionalFixture {
        let mut fixture = BidirectionalFixture::new();
        std::fs::write(fixture.watch_root.join("alex.txt"), b"local").expect("seed local");
        std::fs::write(fixture.cloud_root.join("alex.txt"), b"cloud").expect("seed cloud");
        fixture
            .runtime
            .schedule_startup_reconcile(timestamp_ms(fixture.now_ms))
            .expect("startup scan");
        fixture
    }

    /// Ticks under `inputs` until the divergence is a keep-both copy
    /// on both sides and the queue is empty.
    fn keeps_both_under(fixture: &mut BidirectionalFixture, inputs: ThrottleInputs) -> bool {
        for _ in 0..40 {
            fixture.clock.advance(Duration::from_secs(6));
            fixture.now_ms += 6_000;
            fixture
                .runtime
                .tick_with_inputs(timestamp_ms(fixture.now_ms), inputs)
                .expect("tick");
            let local = files_in(&fixture.watch_root);
            let cloud = files_in(&fixture.cloud_root);
            if fixture.runtime.state_db().queue_depth().expect("depth") == 0
                && local.iter().any(|name| name.starts_with("alex~conflict-"))
                && cloud.iter().any(|name| name.starts_with("alex~conflict-"))
            {
                return true;
            }
        }
        false
    }

    #[test]
    fn the_startup_scan_runs_under_each_condition_that_can_last() {
        // A device on battery, a user at the keyboard, Low Power Mode:
        // each can hold for hours. Sync gets slower. It does not stop.
        let conditions: [(&str, ThrottleInputs, vapor_shared::ThrottleState); 3] = [
            (
                "a user at the keyboard",
                ThrottleInputs {
                    user_active: true,
                    ..ThrottleInputs::default()
                },
                vapor_shared::ThrottleState::Throttled,
            ),
            (
                "battery power",
                ThrottleInputs {
                    on_battery: true,
                    ..ThrottleInputs::default()
                },
                vapor_shared::ThrottleState::Light,
            ),
            (
                "Low Power Mode",
                ThrottleInputs {
                    low_power_mode: true,
                    on_battery: true,
                    ..ThrottleInputs::default()
                },
                vapor_shared::ThrottleState::Throttled,
            ),
        ];
        for (name, inputs, state) in conditions {
            let mut fixture = fixture_with_offline_divergence();
            assert!(
                keeps_both_under(&mut fixture, inputs),
                "the scan must run under {name}; local {:?} cloud {:?}",
                files_in(&fixture.watch_root),
                files_in(&fixture.cloud_root)
            );
            assert_eq!(
                fixture.runtime.app.snapshot().throttle_state,
                state,
                "{name}"
            );
            assert_eq!(fixture.runtime.reconcile_status(), ReconcileStatus::Idle);
        }
    }

    #[test]
    fn the_memory_ceiling_reacts_to_measured_memory_and_not_to_cache_population() {
        let mut fixture = BidirectionalFixture::new();
        let timeline = crate::timeline::TimelineBuffer::new(256);
        fixture.runtime.attach_timeline(timeline.clone());
        let device = 16_u64 * 1024 * 1024 * 1024;
        let sample = |fixture: &mut BidirectionalFixture, resident: Option<u64>| {
            fixture.clock.advance(Duration::from_secs(2));
            fixture.now_ms += 2_000;
            fixture
                .runtime
                .tick_with_inputs(
                    timestamp_ms(fixture.now_ms),
                    ThrottleInputs {
                        vapor_memory_bytes: resident,
                        device_memory_bytes: resident.map(|_| device),
                        ..ThrottleInputs::default()
                    },
                )
                .expect("tick");
        };

        // A bulk of synced files fills the echo caches. With no memory
        // reading nothing is trimmed, whatever the caches hold.
        seed_synced_files(&mut fixture, 40);
        sample(&mut fixture, None);
        assert_eq!(timeline.max_entries(), 256);

        // Far over any share the ceiling can be: the caches trim.
        sample(&mut fixture, Some(device / 2));
        assert_eq!(timeline.max_entries(), 64);
        // Clearly under: restored.
        sample(&mut fixture, Some(device / 1_000));
        assert_eq!(timeline.max_entries(), 256);
    }

    #[cfg(unix)]
    #[test]
    fn a_local_root_that_cannot_be_read_is_a_run_reason_until_it_can() {
        use std::os::unix::fs::PermissionsExt;
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let root = fixture.watch_root.clone();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).expect("lock");
        if std::fs::read_dir(&root).is_ok() {
            // Running as a user that permissions do not stop.
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).ok();
            return;
        }
        fixture
            .runtime
            .schedule_startup_reconcile(timestamp_ms(fixture.now_ms))
            .expect("scan");
        for _ in 0..4 {
            fixture.tick(6_000);
        }
        let snapshot = fixture.runtime.app.snapshot().clone();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).expect("unlock");
        assert_eq!(snapshot.run_state, RunState::Error);
        assert!(
            snapshot.reason.contains("permission denied")
                && snapshot.reason.contains("Privacy & Security"),
            "the reason names the cause and where to allow it: {:?}",
            snapshot.reason
        );

        // The folder can be read again: the walk that the runtime queued
        // for it clears the reason.
        for _ in 0..200 {
            fixture.tick(60_000);
            if fixture.runtime.app.snapshot().run_state == RunState::Running {
                break;
            }
        }
        assert_eq!(fixture.runtime.app.snapshot().run_state, RunState::Running);
    }

    #[test]
    fn a_reconcile_requested_while_one_walks_gets_a_walk_of_its_own() {
        let mut fixture = BidirectionalFixture::new();
        // More folders than one slice of the walk compares.
        let folders = constants::engine::RECONCILE_DIRS_PER_SLICE_IDLE + 16;
        for index in 0..folders {
            std::fs::create_dir(fixture.watch_root.join(format!("folder-{index:03}")))
                .expect("folder");
        }
        fixture
            .runtime
            .schedule_startup_reconcile(timestamp_ms(fixture.now_ms))
            .expect("startup scan");
        // A walker that outlives a tick has folders left to compare.
        for _ in 0..10 {
            if fixture.runtime.reconcile_walker.is_some() {
                break;
            }
            fixture.tick(1_000);
        }
        assert!(
            fixture.runtime.reconcile_walker.is_some(),
            "the walk is under way"
        );

        // A file appears in the root, which the walk compared first,
        // and no watcher event tells of it. The user asks for a
        // reconcile.
        std::fs::write(
            fixture.watch_root.join("late.txt"),
            b"after the root was compared",
        )
        .expect("late file");
        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        fixture.runtime.attach_control(control.clone());
        control.request_reconcile();

        fixture.converge(40);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("late.txt")).expect("uploaded"),
            b"after the root was compared",
            "the walk that was under way had passed the root; the request got a walk of its own"
        );
    }

    #[test]
    fn a_running_daemon_scans_the_whole_scope_once_a_day() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline: the day starts here
        // A file put in the cloud with no feed event, the way a lost
        // event leaves it: nothing tells the daemon about it.
        std::fs::write(fixture.cloud_root.join("missed.txt"), b"nobody said").expect("seed");
        for _ in 0..20 {
            fixture.tick(60 * 60 * 1_000);
        }
        assert!(
            !fixture.watch_root.join("missed.txt").exists(),
            "inside the day nothing looks"
        );

        for _ in 0..5 {
            fixture.tick(60 * 60 * 1_000);
        }
        fixture.converge(12);
        assert_eq!(
            std::fs::read(fixture.watch_root.join("missed.txt")).expect("found by the daily scan"),
            b"nobody said"
        );

        // One scan a day, not one a tick: the next one is a day away.
        std::fs::write(fixture.cloud_root.join("later.txt"), b"later").expect("seed");
        for _ in 0..6 {
            fixture.tick(60 * 60 * 1_000);
        }
        assert!(!fixture.watch_root.join("later.txt").exists());
    }

    #[test]
    fn critical_thermal_pressure_holds_the_startup_scan_and_says_why() {
        let mut fixture = fixture_with_offline_divergence();
        let critical = ThrottleInputs {
            thermal_pressure: crate::throttle::ThermalPressure::Critical,
            ..ThrottleInputs::default()
        };
        let mut database_writes = 0;
        for _ in 0..5 {
            fixture.clock.advance(Duration::from_secs(6));
            fixture.now_ms += 6_000;
            let report = fixture
                .runtime
                .tick_with_inputs(timestamp_ms(fixture.now_ms), critical)
                .expect("tick");
            assert!(report.started_reconcile_root.is_none());
            database_writes += report.leased_intents + report.requeued_intents;
        }
        assert_eq!(
            database_writes, 0,
            "a scan that cannot start is not leased and handed back on every tick"
        );
        match fixture.runtime.reconcile_status() {
            ReconcileStatus::Waiting { reason } => assert_eq!(
                reason, "waiting: thermal pressure is critical",
                "status must name what the scan waits for"
            ),
            other => panic!("the startup scan must report itself as waiting: {other:?}"),
        }
        assert_eq!(
            std::fs::read(fixture.watch_root.join("alex.txt")).expect("local"),
            b"local",
            "nothing may move while sync is suspended"
        );

        // The device cools down: the scan runs with no request.
        assert!(keeps_both_under(&mut fixture, ThrottleInputs::default()));
    }

    #[test]
    fn concurrent_edit_conflict_keeps_both_versions_on_both_sides() {
        // Simultaneous local + foreign remote edits of a synced
        // file resolve as keep-both: the remote canonical lands at the
        // original path, the local edit survives as a conflict copy, and
        // the copy propagates to the cloud.
        let mut fixture = BidirectionalFixture::new();
        let timeline = crate::timeline::TimelineBuffer::new(64);
        fixture.runtime.attach_timeline(timeline.clone());
        fixture.tick(6_000); // baseline

        // Establish a synced file (upload → sync index).
        let local_file = fixture.watch_root.join("doc.txt");
        std::fs::write(&local_file, b"v1 shared").expect("seed local");
        fixture.record_local_event(&local_file, FsEventKind::Created, fixture.now_ms);
        fixture.converge(12);
        assert!(fixture.cloud_root.join("doc.txt").exists());

        // Concurrent divergence.
        foreign_remote_edit(&fixture, "doc.txt", b"v2 from another device");
        std::fs::write(&local_file, b"v2 local edit").expect("local edit");
        fixture.record_local_event(&local_file, FsEventKind::Modified, fixture.now_ms);

        fixture.converge(24);

        // Local side: canonical carries the remote version; the local
        // edit survives in a conflict copy named per the conflict-suffix template.
        assert_eq!(
            std::fs::read(&local_file).expect("canonical"),
            b"v2 from another device"
        );
        let local_files = files_in(&fixture.watch_root);
        let conflict_name = local_files
            .iter()
            .find(|name| name.contains("~conflict-"))
            .expect("a conflict copy must exist locally");
        assert!(
            conflict_name.starts_with("doc~conflict-") && conflict_name.ends_with(".txt"),
            "conflict name must follow {{stem}}~conflict-{{device}}-{{ts}}{{ext}}: {conflict_name}"
        );
        assert_eq!(
            std::fs::read(fixture.watch_root.join(conflict_name)).expect("conflict copy"),
            b"v2 local edit",
            "the losing local edit must survive in the conflict copy"
        );
        // Cloud side: both versions present after the copy uploads.
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("doc.txt")).expect("cloud canonical"),
            b"v2 from another device"
        );
        assert_eq!(
            std::fs::read(fixture.cloud_root.join(conflict_name)).expect("cloud conflict copy"),
            b"v2 local edit"
        );
        assert!(fixture.runtime.conflict_count() >= 1);
        // The timeline names the file, so a notification can too.
        let entries = timeline.snapshot(None);
        let entry = entries
            .iter()
            .find(|entry| entry.kind == constants::timeline::KIND_CONFLICT)
            .expect("the conflict has a timeline entry");
        assert!(
            entry.message.ends_with("doc.txt"),
            "the conflicted file, not its copy: {}",
            entry.message
        );
    }

    #[test]
    fn a_slow_download_after_a_conflict_keeps_the_winning_version_in_the_cloud() {
        // The daemon renames the local edit to a conflict copy and
        // queues the download of the cloud version. While that
        // download is still failing, the watcher reports the rename;
        // the original path is gone at stabilization. Nothing may read
        // that as a deletion of the cloud file.
        let (fixture, downloads_fail) = BidirectionalFixture::new_with_download_switch();
        let mut fixture = fixture.watching_the_local_tree();
        fixture.tick(6_000); // baseline

        let local_file = fixture.watch_root.join("doc.txt");
        std::fs::write(&local_file, b"v1 shared").expect("seed local");
        fixture.converge(12);
        assert!(fixture.cloud_root.join("doc.txt").exists());

        downloads_fail.store(true, std::sync::atomic::Ordering::SeqCst);
        foreign_remote_edit(&fixture, "doc.txt", b"v2 from another device");
        std::fs::write(&local_file, b"v2 local edit").expect("local edit");
        // Longer than the debounce window, the settle window, and a
        // few retries of the download.
        for _ in 0..40 {
            fixture.tick(1_000);
        }
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("doc.txt")).expect("cloud canonical"),
            b"v2 from another device",
            "the version that won the conflict stays in the cloud"
        );

        downloads_fail.store(false, std::sync::atomic::Ordering::SeqCst);
        for _ in 0..12 {
            fixture.tick(60_000);
        }
        assert_eq!(
            std::fs::read(&local_file).expect("canonical"),
            b"v2 from another device"
        );
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("doc.txt")).expect("cloud canonical"),
            b"v2 from another device"
        );
        let conflict_name = files_in(&fixture.watch_root)
            .into_iter()
            .find(|name| name.contains("~conflict-"))
            .expect("a conflict copy must exist locally");
        assert_eq!(
            std::fs::read(fixture.cloud_root.join(&conflict_name)).expect("cloud conflict copy"),
            b"v2 local edit"
        );
        assert_eq!(fixture.runtime.state_db().queue_depth().expect("depth"), 0);
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0
        );
    }

    #[test]
    fn a_file_removed_and_put_back_within_seconds_stays_in_the_cloud() {
        let mut fixture = BidirectionalFixture::new().watching_the_local_tree();
        fixture.tick(6_000); // baseline

        let local_file = fixture.watch_root.join("notes.txt");
        std::fs::write(&local_file, b"keep me").expect("seed local");
        fixture.converge(12);
        assert!(fixture.cloud_root.join("notes.txt").exists());

        // Out of the folder for longer than the debounce window, back
        // before the settle window of the delete ends.
        let outside = fixture._temp.path().join("notes.txt.elsewhere");
        std::fs::rename(&local_file, &outside).expect("move out");
        for _ in 0..3 {
            fixture.tick(1_000);
        }
        std::fs::rename(&outside, &local_file).expect("move back");
        for _ in 0..30 {
            fixture.tick(1_000);
        }
        fixture.converge(12);

        assert_eq!(std::fs::read(&local_file).expect("local"), b"keep me");
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("notes.txt")).expect("cloud copy stays"),
            b"keep me"
        );
        assert_eq!(fixture.runtime.state_db().queue_depth().expect("depth"), 0);
    }

    fn conflict_questions(fixture: &BidirectionalFixture) -> Vec<crate::state_db::DecisionRecord> {
        fixture
            .runtime
            .state_db()
            .decisions(false)
            .expect("decisions")
            .into_iter()
            .filter(|decision| decision.kind == constants::decision::KIND_CONFLICT)
            .collect()
    }

    /// `report.md` and one copy of it, indexed the way a synced copy is.
    fn seed_conflict(fixture: &mut BidirectionalFixture) -> (PathBuf, PathBuf) {
        let original = fixture.watch_root.join("report.md");
        let copy = fixture
            .watch_root
            .join("report~conflict-other-mac-1750000000123.md");
        std::fs::write(&original, b"the cloud's version").expect("seed original");
        std::fs::write(&copy, b"the other mac's version").expect("seed copy");
        fixture
            .runtime
            .state_db
            .set_sync_index(
                &copy,
                "hash",
                23,
                None,
                None,
                "op",
                timestamp_ms(fixture.now_ms),
            )
            .expect("index the copy");
        (original, copy)
    }

    #[test]
    fn a_file_that_only_looks_like_a_conflict_copy_is_not_asked_about() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        // No `report.txt`, and no conflict that this device resolved:
        // the name came from somewhere else.
        let lookalike = fixture
            .watch_root
            .join("report~conflict-otherdevice-1700000000000.txt");
        std::fs::write(&lookalike, b"an export of something").expect("seed");
        fixture.record_local_event(&lookalike, FsEventKind::Created, fixture.now_ms);
        fixture.converge(12);
        for _ in 0..6 {
            fixture.tick(6_000);
        }

        assert!(
            fixture
                .cloud_root
                .join("report~conflict-otherdevice-1700000000000.txt")
                .is_file(),
            "the file syncs like any file"
        );
        assert!(
            conflict_questions(&fixture).is_empty(),
            "{:?}",
            conflict_questions(&fixture)
        );
    }

    #[test]
    fn a_burst_shows_as_waiting_work_and_is_scanned_a_moment_after_it_ends() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let project = fixture.watch_root.join("project");
        std::fs::create_dir_all(&project).expect("dir");
        // One burst: more paths in two seconds than a folder takes
        // one by one.
        let files = constants::engine::STORM_DIRECTORY_UNIQUE_PATHS_THRESHOLD + 50;
        for index in 0..files {
            let path = project.join(format!("f{index}.txt"));
            std::fs::write(&path, format!("payload {index}")).expect("seed");
            fixture.record_local_event(&path, FsEventKind::Created, fixture.now_ms);
        }
        fixture.tick(250);

        // The changes are in no queue yet, and the status says so.
        match fixture.runtime.reconcile_status() {
            ReconcileStatus::Waiting { reason } => {
                assert!(
                    reason.contains("change(s) in project wait for one scan"),
                    "{reason}"
                );
            }
            other => panic!("a folder that waits for its scan must show: {other:?}"),
        }
        assert!(
            fixture
                .runtime
                .has_pending_work(&RuntimeTickReport::default())
        );

        // The device is idle: two seconds after the burst the scan is
        // released, long before the delay of a device in use.
        fixture.tick(2_000);
        fixture.tick(250);
        assert!(
            fixture.runtime.state_db().queue_depth().expect("depth") > 0
                || fixture.runtime.reconcile_status() != ReconcileStatus::Idle,
            "the scan of the folder started"
        );
        fixture.converge(60);
        assert_eq!(names_in(&fixture.cloud_root.join("project")).len(), files);
        assert_eq!(fixture.runtime.reconcile_status(), ReconcileStatus::Idle);
    }

    #[test]
    fn a_walk_that_waits_for_a_listing_when_its_slice_ends_goes_on_in_the_same_tick() {
        let (mut fixture, hold) = BidirectionalFixture::new_with_listing_hold();
        fixture.runtime.enable_transfer_workers(None);
        std::fs::write(fixture.watch_root.join("a.txt"), b"a").expect("seed");
        hold.set(true);
        fixture
            .runtime
            .schedule_startup_reconcile(timestamp_ms(fixture.now_ms))
            .expect("schedule");
        // Bounded by iteration count: the walk starts when the threads
        // that it waits for have answered.
        for _ in 0..100_000 {
            if fixture.runtime.running_reconcile_intent_id.is_some() {
                break;
            }
            fixture.tick(250);
            std::thread::yield_now();
        }
        assert!(fixture.runtime.running_reconcile_intent_id.is_some());

        // The cloud does not answer the listing, and the slice of the
        // walk ends. The walk used none of it on this thread, so it is
        // the running reconcile again when the tick ends: the answer
        // wakes the loop, and no timer stands between the two.
        let mut yields = 0;
        for _ in 0..8 {
            let report = fixture.tick(250);
            yields += report.requeued_intents;
            assert!(
                fixture.runtime.running_reconcile_intent_id.is_some(),
                "the walk waits for the cloud as the running reconcile"
            );
        }
        assert!(yields > 0, "the slice ended while the listing was out");

        // On battery the gate has one read token, and a scan holds one.
        // A walk that took it back in the same tick would keep every
        // hash waiting until the walk ends. So there it lets go for a
        // tick, as a walk that used its slice does.
        let on_battery = |fixture: &mut BidirectionalFixture| {
            fixture.clock.advance(Duration::from_millis(250));
            fixture.now_ms += 250;
            fixture
                .runtime
                .tick_with_inputs(
                    timestamp_ms(fixture.now_ms),
                    ThrottleInputs {
                        on_battery: true,
                        ..ThrottleInputs::default()
                    },
                )
                .expect("runtime tick");
        };
        for _ in 0..40 {
            if fixture.runtime.app.snapshot().throttle_state == vapor_shared::ThrottleState::Light {
                break;
            }
            on_battery(&mut fixture);
        }
        assert_eq!(
            fixture.runtime.app.snapshot().throttle_state,
            vapor_shared::ThrottleState::Light
        );
        // Four slices pass: the first can still end on the caps of
        // the state before, and each later one ends on those of now.
        let mut let_go = false;
        for _ in 0..8 {
            on_battery(&mut fixture);
            let_go |= fixture.runtime.running_reconcile_intent_id.is_none();
        }
        assert!(let_go, "the permit is free for a tick between two slices");

        hold.set(false);
        for _ in 0..100_000 {
            fixture.tick(250);
            if fixture.runtime.reconcile_walker.is_none()
                && fixture.runtime.running_reconcile_intent_id.is_none()
            {
                break;
            }
            std::thread::yield_now();
        }
        assert!(fixture.runtime.reconcile_walker.is_none(), "the walk ended");
    }

    #[test]
    fn a_conflicted_file_is_one_decision_until_no_copy_is_left() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline
        let (original, copy) = seed_conflict(&mut fixture);

        fixture.tick(6_000);
        let asked = conflict_questions(&fixture);
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert_eq!(asked[0].path.as_deref(), Some(original.as_path()));
        let keys = |decision: &crate::state_db::DecisionRecord| {
            decision
                .options
                .iter()
                .map(|option| option.key.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            keys(&asked[0]),
            vec!["report.md", "report~conflict-other-mac-1750000000123.md"]
        );

        // A third device's edit lands as a second copy: the same
        // question now offers three versions.
        let third = fixture
            .watch_root
            .join("report~conflict-third-mac-1750000000456.md");
        std::fs::write(&third, b"the third mac's version").expect("seed copy");
        fixture.tick(6_000);
        let grown = conflict_questions(&fixture);
        assert_eq!(grown.len(), 1, "one question per file: {grown:?}");
        assert_eq!(grown[0].id, asked[0].id, "the id the user saw stays valid");
        assert_eq!(keys(&grown[0]).len(), 3);
        assert!(
            grown[0].question.contains("all 3 versions"),
            "{}",
            grown[0].question
        );

        // Resolved by hand (the Finder, another device): the question
        // closes on its own.
        std::fs::remove_file(&copy).expect("resolve by hand");
        std::fs::remove_file(&third).expect("resolve by hand");
        fixture.tick(6_000);
        assert!(conflict_questions(&fixture).is_empty());
    }

    #[test]
    fn a_conflict_answer_never_applied_is_asked_again_and_the_files_stay() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline
        let (original, copy) = seed_conflict(&mut fixture);
        fixture.tick(6_000);
        let id = conflict_questions(&fixture)[0].id;
        let choice = copy.file_name().unwrap().to_str().unwrap().to_string();

        // A command that recorded the answer and died before applying it.
        fixture
            .runtime
            .state_db
            .resolve_decision(id, &choice, timestamp_ms(fixture.now_ms))
            .expect("answer");
        fixture.tick(6_000);
        let pending = fixture
            .runtime
            .state_db()
            .decision(id)
            .expect("read")
            .expect("decision");
        assert!(
            pending.resolved_at.is_some(),
            "left alone inside the grace period"
        );
        assert!(
            original.exists() && copy.exists(),
            "the daemon never moves conflict files"
        );

        fixture.tick(constants::conflict::ANSWER_GRACE_SECONDS * 1_000);
        fixture.tick(6_000);
        let reopened = fixture
            .runtime
            .state_db()
            .decision(id)
            .expect("read")
            .expect("decision");
        assert!(reopened.is_open(), "asked again: {reopened:?}");
        assert!(reopened.choice.is_none());
        assert_eq!(
            std::fs::read(&original).expect("untouched"),
            b"the cloud's version"
        );
        assert!(copy.exists());
        assert_eq!(
            conflict_questions(&fixture).len(),
            1,
            "one question, not two"
        );
    }

    #[test]
    fn delete_modify_race_resolves_for_the_modification() {
        // A remote deletion racing a local modification loses:
        // data preservation wins over deletion, deterministically, in
        // both intent orderings.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline

        let local_file = fixture.watch_root.join("contested.txt");
        std::fs::write(&local_file, b"v1").expect("seed local");
        fixture.record_local_event(&local_file, FsEventKind::Created, fixture.now_ms);
        fixture.converge(12);
        assert!(fixture.cloud_root.join("contested.txt").exists());

        // Remote deletes while local modifies.
        std::fs::remove_file(fixture.cloud_root.join("contested.txt")).expect("cloud delete");
        fixture.feed.emit_removed(
            fixture.cloud_root.join("contested.txt"),
            timestamp_ms(fixture.now_ms),
        );
        std::fs::write(&local_file, b"v2 modified during delete").expect("local modify");
        fixture.record_local_event(&local_file, FsEventKind::Modified, fixture.now_ms);

        fixture.converge(24);

        assert_eq!(
            std::fs::read(&local_file).expect("local survives"),
            b"v2 modified during delete",
            "the modification must survive the racing deletion"
        );
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("contested.txt"))
                .expect("modification restored remotely"),
            b"v2 modified during delete"
        );
    }

    #[test]
    fn concurrent_write_race_smoke_converges_across_runs() {
        // Happy-path race smoke: five runs alternating which side
        // wins the enqueue race; every run converges with no version
        // lost (canonical matches on both sides; the other version, when
        // divergent, survives as a conflict copy).
        for run in 0..5 {
            let mut fixture = BidirectionalFixture::new();
            fixture.tick(6_000); // baseline

            let local_file = fixture.watch_root.join("raced.txt");
            std::fs::write(&local_file, b"base").expect("seed local");
            fixture.record_local_event(&local_file, FsEventKind::Created, fixture.now_ms);
            fixture.converge(12);

            let local_content = format!("local-{run}");
            let remote_content = format!("remote-{run}");
            if run % 2 == 0 {
                std::fs::write(&local_file, &local_content).expect("local edit");
                fixture.record_local_event(&local_file, FsEventKind::Modified, fixture.now_ms);
                foreign_remote_edit(&fixture, "raced.txt", remote_content.as_bytes());
                fixture.feed.emit_modified(
                    fixture.cloud_root.join("raced.txt"),
                    timestamp_ms(fixture.now_ms),
                );
            } else {
                foreign_remote_edit(&fixture, "raced.txt", remote_content.as_bytes());
                fixture.feed.emit_modified(
                    fixture.cloud_root.join("raced.txt"),
                    timestamp_ms(fixture.now_ms),
                );
                std::fs::write(&local_file, &local_content).expect("local edit");
                fixture.record_local_event(&local_file, FsEventKind::Modified, fixture.now_ms);
            }

            fixture.converge(24);

            // Convergence: local and cloud canonicals agree.
            let local_canonical = std::fs::read(&local_file).expect("local canonical");
            let cloud_canonical =
                std::fs::read(fixture.cloud_root.join("raced.txt")).expect("cloud canonical");
            assert_eq!(
                local_canonical, cloud_canonical,
                "run {run}: both sides must converge on one canonical"
            );
            // No version lost: every written content exists somewhere.
            let mut all_contents: Vec<Vec<u8>> = files_in(&fixture.watch_root)
                .iter()
                .map(|name| std::fs::read(fixture.watch_root.join(name)).expect("read"))
                .collect();
            all_contents.extend(
                files_in(&fixture.cloud_root)
                    .iter()
                    .map(|name| std::fs::read(fixture.cloud_root.join(name)).expect("read")),
            );
            assert!(
                all_contents.iter().any(|c| c == local_content.as_bytes()),
                "run {run}: the local edit must survive somewhere"
            );
            assert!(
                all_contents.iter().any(|c| c == remote_content.as_bytes()),
                "run {run}: the remote edit must survive somewhere"
            );
        }
    }

    #[test]
    fn loop_prevention_survives_a_provider_without_xattr_support() {
        // Constraint compatibility: on filesystems without xattr
        // (FAT, network mounts), op-id tags fall back to side-files.
        // The full echo-suppression flow must still hold.
        let temp = TempDir::new().expect("temp dir");
        let watch_root = temp.path().join("watch");
        let cloud_root = temp.path().join("cloud");
        std::fs::create_dir_all(&watch_root).expect("watch root");
        std::fs::create_dir_all(&cloud_root).expect("cloud root");
        let cloud_root = vapor_shared::paths::canonicalize(&cloud_root).expect("canonical cloud");
        let state_db = DurableStateDb::open(temp.path().join("state/vapor.sqlite"))
            .expect("open durable state db");
        let clock = Arc::new(crate::clock::ManualClock::at_now());
        let no_xattr: Arc<dyn vapor_platform::fs_caps::FilesystemCapabilities> = Arc::new(
            vapor_platform::fs_caps::InMemoryFilesystemCapabilities::new(
                false,
                vapor_platform::fs_caps::CaseSensitivity::Sensitive,
            ),
        );
        let (provider, feed) =
            vapor_providers::FilesystemProvider::with_manual_feed(&cloud_root, no_xattr)
                .expect("no-xattr provider");
        let mut runtime = DaemonRuntime::build(
            SyncScope {
                local_root: Some(watch_root.clone()),
                cloud_root: cloud_root.to_string_lossy().into_owned(),
                sync_mode: vapor_shared::SyncMode::TwoWay,
            },
            EventPathFilterOptions::default(),
            state_db,
            Box::new(provider),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            false,
        )
        .expect("runtime");
        let watch_root = runtime
            .sync_scope()
            .local_root
            .clone()
            .expect("normalized root");

        fn tick_once(
            runtime: &mut DaemonRuntime,
            clock: &Arc<crate::clock::ManualClock>,
            now_ms: &mut u64,
            advance: u64,
        ) -> RuntimeTickReport {
            clock.advance(Duration::from_millis(advance));
            *now_ms += advance;
            runtime
                .tick_with_inputs(timestamp_ms(*now_ms), ThrottleInputs::default())
                .expect("tick")
        }
        let mut now_ms: u64 = 0;
        tick_once(&mut runtime, &clock, &mut now_ms, 6_000); // baseline

        // Upload a local file: the remote copy gets a SIDE-FILE tag.
        let local_file = watch_root.join("side-file-mode.txt");
        std::fs::write(&local_file, b"payload").expect("seed");
        {
            let recorder = runtime.recorder.as_ref().expect("recorder");
            FsEventRecording::record_event(
                recorder.as_ref(),
                FsEventRecord {
                    path: local_file.clone(),
                    kind: FsEventKind::Created,
                    observed_at: timestamp_ms(now_ms),
                },
            );
        }
        let mut completed = 0;
        for _ in 0..12 {
            let report = tick_once(&mut runtime, &clock, &mut now_ms, 6_000);
            completed += report.completed_intents;
            if completed > 0 && runtime.state_db().queue_depth().expect("depth") == 0 {
                break;
            }
        }
        assert!(completed >= 1, "upload must complete");
        assert!(
            cloud_root
                .join("side-file-mode.txt.vapor-meta.json")
                .exists(),
            "op-id must land in a side-file when xattr is unsupported"
        );

        // The feed echo of our own upload must still be suppressed:
        // correlated through the side-file, not xattr.
        feed.emit_created(cloud_root.join("side-file-mode.txt"), timestamp_ms(now_ms));
        let report = tick_once(&mut runtime, &clock, &mut now_ms, 6_000);
        assert_eq!(
            report.remote_poll.suppressed_echoes, 1,
            "side-file op-id must still suppress the echo"
        );
        assert_eq!(runtime.state_db().queue_depth().expect("depth"), 0);
    }

    #[test]
    fn burst_of_real_uploads_drains_without_admission_serialization() {
        // Guard-rail: a burst of provider-backed uploads must
        // drain with parallel admission (planner cap 4 in Idle),
        // not one-intent-per-tick serialization. 40 files with 4-wide
        // stages should finish comfortably under 60 ticks; a regression
        // to serialized admission would need 120+.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline

        const BURST: usize = 40;
        for index in 0..BURST {
            let path = fixture.watch_root.join(format!("burst-{index}.txt"));
            std::fs::write(&path, format!("payload {index}")).expect("seed");
            fixture.record_local_event(&path, FsEventKind::Created, fixture.now_ms);
        }

        let mut completed = 0;
        let mut ticks = 0;
        for _ in 0..60 {
            ticks += 1;
            let report = fixture.tick(6_000);
            completed += report.completed_intents;
            if completed >= BURST && fixture.runtime.state_db().queue_depth().expect("depth") == 0 {
                break;
            }
        }
        assert_eq!(completed, BURST, "every upload must complete");
        assert!(
            ticks < 60,
            "burst did not drain within the tick budget ({ticks} ticks)"
        );
        for index in 0..BURST {
            assert!(
                fixture
                    .cloud_root
                    .join(format!("burst-{index}.txt"))
                    .exists(),
                "burst-{index} must land remotely"
            );
        }
    }

    #[test]
    fn bandwidth_ceiling_holds_transfers_until_tokens_refill() {
        // With a tiny link capacity the bucket grants almost nothing
        // each second, so an upload holds at its checkpoint; a link
        // that carries more lets it complete. The capacity is pinned,
        // so no probe step runs past the bucket.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline (default inputs)

        // About 31 bytes each second at the default 25%.
        fixture.runtime.pin_link_capacity_kbps(1);
        let starved_inputs = ThrottleInputs::default();
        // Sample once so the bucket takes the new rate.
        fixture.clock.advance(Duration::from_millis(6_000));
        fixture.now_ms += 6_000;
        fixture
            .runtime
            .tick_with_inputs(timestamp_ms(fixture.now_ms), starved_inputs)
            .expect("tick");

        let local_file = fixture.watch_root.join("starved.bin");
        std::fs::write(&local_file, vec![9_u8; 64 * 1024]).expect("seed 64KiB");
        fixture.record_local_event(&local_file, FsEventKind::Created, fixture.now_ms);

        // Several ticks under starvation: the payload must NOT complete
        // (a few stray bytes may trickle, the file cannot finish).
        let mut completed = 0;
        for _ in 0..6 {
            fixture.clock.advance(Duration::from_millis(1_500));
            fixture.now_ms += 1_500;
            let report = fixture
                .runtime
                .tick_with_inputs(timestamp_ms(fixture.now_ms), starved_inputs)
                .expect("tick");
            completed += report.completed_intents;
        }
        assert_eq!(completed, 0, "starved bandwidth must hold the upload");
        assert!(!fixture.cloud_root.join("starved.bin").exists());

        // A link that carries more: the transfer completes.
        fixture.runtime.pin_link_capacity_kbps(100_000);
        let restored = ThrottleInputs::default();
        for _ in 0..12 {
            fixture.clock.advance(Duration::from_millis(6_000));
            fixture.now_ms += 6_000;
            let report = fixture
                .runtime
                .tick_with_inputs(timestamp_ms(fixture.now_ms), restored)
                .expect("tick");
            completed += report.completed_intents;
            if completed > 0 {
                break;
            }
        }
        assert!(
            completed >= 1,
            "restored bandwidth must complete the upload"
        );
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("starved.bin"))
                .expect("uploaded")
                .len(),
            64 * 1024
        );
    }

    #[test]
    fn idle_boost_scales_workgate_caps_and_snaps_back_on_throttle_exit() {
        // The AlwaysIdle notifier + Idle inputs engage
        // boost; after the ramp the workgate caps exceed their base, and
        // a throttle exit snaps them back in the same sample.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);

        // Ride out the min-idle + ramp (AlwaysIdle reports a day).
        for _ in 0..8 {
            fixture.tick(6_000);
        }
        let boosted = fixture.runtime.app().workgate_snapshot();
        assert!(
            boosted.caps.planner_workers > crate::throttle::idle_concurrency(),
            "boost must raise caps above the base ({} <= {})",
            boosted.caps.planner_workers,
            crate::throttle::idle_concurrency()
        );
        let status = fixture
            .runtime
            .resources_status()
            .expect("published after first sample");
        assert_eq!(status.idle_boost.state, "active");
        assert!(status.cpu.limit_percent > 15);

        // Heavy foreign load drives the throttle out of Idle: caps
        // snap to (at most) their throttled base values immediately.
        let busy = ThrottleInputs {
            system_cpu_load_percent: 70,
            ..ThrottleInputs::default()
        };
        fixture.clock.advance(Duration::from_millis(6_000));
        fixture.now_ms += 6_000;
        fixture
            .runtime
            .tick_with_inputs(timestamp_ms(fixture.now_ms), busy)
            .expect("tick");
        let snapped = fixture.runtime.app().workgate_snapshot();
        assert!(
            snapped.caps.planner_workers <= crate::throttle::idle_concurrency(),
            "post-Idle states never run against boosted caps"
        );
        let status = fixture.runtime.resources_status().expect("still published");
        assert_eq!(status.idle_boost.state, "off");
        assert_eq!(status.cpu.limit_percent, 15);
    }

    #[test]
    fn restart_recovers_in_flight_upload_and_completes_it() {
        let temp = TempDir::new().expect("temp dir");
        let watch_root = temp.path().join("watch");
        let cloud_root = temp.path().join("cloud");
        std::fs::create_dir_all(&watch_root).expect("watch root");
        std::fs::create_dir_all(&cloud_root).expect("cloud root");
        // Durable intent paths must live under the runtime's *canonical*
        // watch root, exactly like real watcher events do.
        let watch_root =
            vapor_shared::paths::canonicalize(&watch_root).expect("canonical watch root");
        let database_path = temp.path().join("state/vapor.sqlite");
        let local_file = watch_root.join("durable.txt");
        std::fs::write(&local_file, b"survives restarts").expect("seed local");

        let sync_scope = || SyncScope {
            local_root: Some(watch_root.clone()),
            cloud_root: cloud_root.to_string_lossy().into_owned(),
            sync_mode: vapor_shared::SyncMode::TwoWay,
        };

        // First daemon: lease the intent into flight, then "crash"
        // (drop) before completing it. The lease is taken directly on
        // the durable DB: exactly the state a daemon that died between
        // leasing and completing leaves behind. (A ticking runtime
        // cannot model this window: stage chaining completes a small
        // upload within its lease tick.)
        {
            let mut state_db = DurableStateDb::open(&database_path).expect("open durable state db");
            state_db
                .enqueue_intent(&local_file, PendingIntentKind::Upload, timestamp_ms(0))
                .expect("enqueue");
            let leased = state_db
                .lease_next_ready(timestamp_ms(250))
                .expect("lease")
                .expect("intent leased");
            assert_eq!(leased.path, local_file);
            assert_eq!(state_db.leased_depth().expect("leased"), 1);
            // Dropped here with the lease still open: simulated crash.
        }

        // Second daemon on the same durable state: startup recovery
        // re-pends the lease and the pipeline completes it for real.
        let state_db = DurableStateDb::open(&database_path).expect("reopen durable state db");
        let clock = Arc::new(crate::clock::ManualClock::at_now());
        let mut runtime = DaemonRuntime::build(
            sync_scope(),
            EventPathFilterOptions::default(),
            state_db,
            Box::new(vapor_providers::FilesystemProvider::new()),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            false,
        )
        .expect("second runtime");
        assert_eq!(
            runtime.state_db().leased_depth().expect("leased"),
            0,
            "startup recovery must re-pend the orphaned lease"
        );

        // Recovery re-pends the intent at the *recovery* wall time, so
        // ticks must use the fixture clock's wall axis, not synthetic
        // 1970-based stamps.
        use crate::clock::Clock as _;
        let mut completed = 0;
        for _ in 0..12 {
            clock.advance(Duration::from_millis(250));
            clock.advance_system(Duration::from_millis(250));
            let report = runtime
                .tick_with_inputs(clock.now_system(), ThrottleInputs::default())
                .expect("tick");
            completed += report.completed_intents;
            if completed > 0 && runtime.state_db().queue_depth().expect("depth") == 0 {
                break;
            }
        }
        assert!(completed >= 1);
        assert_eq!(
            std::fs::read(cloud_root.join("durable.txt")).expect("uploaded after restart"),
            b"survives restarts"
        );
    }

    #[test]
    fn offline_same_size_edit_is_uploaded_by_the_startup_reconcile() {
        // A file synced by one daemon run is edited while no daemon is
        // running, keeping its byte count. The watcher never sees the
        // edit; only the startup reconcile can, through the mtime the
        // sync index recorded.
        let temp = TempDir::new().expect("temp dir");
        let watch_root = temp.path().join("watch");
        let cloud_root = temp.path().join("cloud");
        std::fs::create_dir_all(&watch_root).expect("watch root");
        std::fs::create_dir_all(&cloud_root).expect("cloud root");
        let watch_root =
            vapor_shared::paths::canonicalize(&watch_root).expect("canonical watch root");
        let database_path = temp.path().join("state/vapor.sqlite");
        let local_file = watch_root.join("notes.txt");
        std::fs::write(&local_file, b"version-A").expect("seed local");

        let sync_scope = || SyncScope {
            local_root: Some(watch_root.clone()),
            cloud_root: cloud_root.to_string_lossy().into_owned(),
            sync_mode: vapor_shared::SyncMode::TwoWay,
        };
        use crate::clock::Clock as _;
        let drive = |runtime: &mut DaemonRuntime, clock: &Arc<crate::clock::ManualClock>| {
            for _ in 0..400 {
                clock.advance(Duration::from_millis(250));
                clock.advance_system(Duration::from_millis(250));
                runtime
                    .tick_with_inputs(clock.now_system(), ThrottleInputs::default())
                    .expect("tick");
                if runtime.state_db().queue_depth().expect("depth") == 0
                    && runtime.state_db().leased_depth().expect("leased") == 0
                {
                    break;
                }
            }
        };

        // First run: upload the file so the sync index records its
        // size and mtime.
        {
            let mut state_db = DurableStateDb::open(&database_path).expect("open state db");
            state_db
                .enqueue_intent(&local_file, PendingIntentKind::Upload, timestamp_ms(0))
                .expect("enqueue");
            let clock = Arc::new(crate::clock::ManualClock::at_now());
            let mut runtime = DaemonRuntime::build(
                sync_scope(),
                EventPathFilterOptions::default(),
                state_db,
                Box::new(vapor_providers::FilesystemProvider::new()),
                Arc::new(StaticMetricsSampler::default()),
                clock.clone(),
                false,
            )
            .expect("first runtime");
            drive(&mut runtime, &clock);
            assert_eq!(
                std::fs::read(cloud_root.join("notes.txt")).expect("uploaded"),
                b"version-A"
            );
            let index = runtime
                .state_db()
                .sync_index(&local_file)
                .expect("index")
                .expect("indexed after upload");
            assert!(index.local_modified_at.is_some());
        }

        // Offline edit: same length, new content, mtime clearly later
        // than what the index recorded.
        std::fs::write(&local_file, b"version-B").expect("edit offline");
        let later = std::fs::metadata(&local_file)
            .expect("metadata")
            .modified()
            .expect("mtime")
            + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&local_file)
            .expect("open for mtime")
            .set_modified(later)
            .expect("set mtime");
        assert_eq!(
            std::fs::metadata(cloud_root.join("notes.txt"))
                .expect("cloud metadata")
                .len(),
            9,
            "the edit must keep the byte count for this test to mean anything"
        );

        // Second run: the startup whole-scope reconcile must notice the
        // touched file and upload it.
        let state_db = DurableStateDb::open(&database_path).expect("reopen state db");
        let clock = Arc::new(crate::clock::ManualClock::at_now());
        let mut runtime = DaemonRuntime::build(
            sync_scope(),
            EventPathFilterOptions::default(),
            state_db,
            Box::new(vapor_providers::FilesystemProvider::new()),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            false,
        )
        .expect("second runtime");
        runtime
            .enqueue_startup_reconstruction_reconcile(clock.now_system())
            .expect("queue startup reconcile");
        drive(&mut runtime, &clock);
        assert_eq!(
            std::fs::read(cloud_root.join("notes.txt")).expect("cloud copy"),
            b"version-B",
            "the offline same-size edit must reach the cloud"
        );
    }

    /// Two daemon runs over one state DB with a gap between them where
    /// no daemon watches either side.
    pub(super) struct OfflineFixture {
        temp: TempDir,
        pub(super) watch_root: PathBuf,
        pub(super) cloud_root: PathBuf,
        database_path: PathBuf,
    }

    impl OfflineFixture {
        pub(super) fn new() -> Self {
            let temp = TempDir::new().expect("temp dir");
            let watch_root = temp.path().join("watch");
            let cloud_root = temp.path().join("cloud");
            std::fs::create_dir_all(&watch_root).expect("watch root");
            std::fs::create_dir_all(&cloud_root).expect("cloud root");
            let watch_root =
                vapor_shared::paths::canonicalize(&watch_root).expect("canonical watch root");
            let database_path = temp.path().join("state/vapor.sqlite");
            Self {
                temp,
                watch_root,
                cloud_root,
                database_path,
            }
        }

        fn scope(&self) -> SyncScope {
            SyncScope {
                local_root: Some(self.watch_root.clone()),
                cloud_root: self.cloud_root.to_string_lossy().into_owned(),
                sync_mode: vapor_shared::SyncMode::TwoWay,
            }
        }

        /// Builds a runtime on the shared DB, schedules the startup
        /// reconcile, and drives it to quiescence.
        pub(super) fn run(&self, seed_uploads: &[&Path]) -> DaemonRuntime {
            self.run_as(vapor_shared::SyncMode::TwoWay, seed_uploads)
        }

        /// [`Self::run`] in a one-way mode that the user confirmed.
        pub(super) fn run_in_mode(&self, sync_mode: vapor_shared::SyncMode) -> DaemonRuntime {
            self.run_as(sync_mode, &[])
        }

        fn run_as(
            &self,
            sync_mode: vapor_shared::SyncMode,
            seed_uploads: &[&Path],
        ) -> DaemonRuntime {
            use crate::clock::Clock as _;
            let mut state_db = DurableStateDb::open(&self.database_path).expect("open state db");
            for path in seed_uploads {
                state_db
                    .enqueue_intent(path, PendingIntentKind::Upload, timestamp_ms(0))
                    .expect("enqueue");
            }
            let clock = Arc::new(crate::clock::ManualClock::at_now());
            let mut runtime = DaemonRuntime::build(
                SyncScope {
                    sync_mode,
                    ..self.scope()
                },
                EventPathFilterOptions::default(),
                state_db,
                Box::new(vapor_providers::FilesystemProvider::new()),
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
            if sync_mode != vapor_shared::SyncMode::TwoWay {
                runtime.acknowledge_one_way_mode(clock.now_system());
            }
            runtime
                .enqueue_startup_reconstruction_reconcile(clock.now_system())
                .expect("queue startup reconcile");
            for _ in 0..400 {
                clock.advance(Duration::from_millis(250));
                clock.advance_system(Duration::from_millis(250));
                runtime
                    .tick_with_inputs(clock.now_system(), ThrottleInputs::default())
                    .expect("tick");
                if runtime.state_db().queue_depth().expect("depth") == 0
                    && runtime.state_db().leased_depth().expect("leased") == 0
                {
                    break;
                }
            }
            runtime
        }

        /// Pushes a file's mtime clearly past whatever the index saw.
        fn touch_later(path: &Path) {
            let later = std::fs::metadata(path)
                .expect("metadata")
                .modified()
                .expect("mtime")
                + Duration::from_secs(5);
            std::fs::File::options()
                .write(true)
                .open(path)
                .expect("open for mtime")
                .set_modified(later)
                .expect("set mtime");
        }
    }

    #[test]
    fn a_file_deleted_here_while_no_daemon_ran_is_deleted_in_the_cloud() {
        let fixture = OfflineFixture::new();
        let local = fixture.watch_root.join("gone.txt");
        std::fs::write(&local, b"deleted offline").expect("seed");
        let first = fixture.run(&[&local]);
        assert!(fixture.cloud_root.join("gone.txt").exists());
        drop(first);

        std::fs::remove_file(&local).expect("delete while away");
        let second = fixture.run(&[]);
        assert!(
            !fixture.cloud_root.join("gone.txt").exists(),
            "the offline deletion propagates to an unchanged cloud copy"
        );
        assert!(!local.exists(), "and is not resurrected");
        assert_eq!(second.state_db().failed_depth().expect("failed"), 0);
    }

    #[test]
    fn a_file_deleted_here_while_away_is_restored_when_the_cloud_copy_changed() {
        let fixture = OfflineFixture::new();
        let local = fixture.watch_root.join("gone.txt");
        std::fs::write(&local, b"deleted offline").expect("seed");
        drop(fixture.run(&[&local]));

        std::fs::remove_file(&local).expect("delete while away");
        let cloud = fixture.cloud_root.join("gone.txt");
        std::fs::write(&cloud, b"but edited in the cloud meanwhile").expect("cloud edit");
        OfflineFixture::touch_later(&cloud);
        drop(fixture.run(&[]));
        assert_eq!(
            std::fs::read(&local).expect("restored"),
            b"but edited in the cloud meanwhile",
            "data preservation wins over a deletion the other side outran"
        );
        assert!(cloud.exists());
    }

    #[test]
    fn a_file_deleted_in_the_cloud_while_no_daemon_ran_is_removed_here_into_the_trash() {
        let fixture = OfflineFixture::new();
        let local = fixture.watch_root.join("gone.txt");
        std::fs::write(&local, b"deleted in the cloud offline").expect("seed");
        drop(fixture.run(&[&local]));

        std::fs::remove_file(fixture.cloud_root.join("gone.txt")).expect("cloud delete");
        let second = fixture.run(&[]);
        assert!(!local.exists(), "the offline cloud deletion applies here");
        assert!(
            !fixture.cloud_root.join("gone.txt").exists(),
            "and the file is not re-uploaded"
        );
        let trashed = second.trash().expect("trash").list();
        assert_eq!(trashed.len(), 1, "kept in the trash");
        assert_eq!(trashed[0].original_path, local);
    }

    #[test]
    fn a_file_deleted_in_the_cloud_while_away_is_reuploaded_when_the_local_copy_changed() {
        let fixture = OfflineFixture::new();
        let local = fixture.watch_root.join("gone.txt");
        std::fs::write(&local, b"deleted in the cloud offline").expect("seed");
        drop(fixture.run(&[&local]));

        std::fs::remove_file(fixture.cloud_root.join("gone.txt")).expect("cloud delete");
        std::fs::write(&local, b"edited here meanwhile, longer").expect("local edit");
        OfflineFixture::touch_later(&local);
        drop(fixture.run(&[]));
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("gone.txt")).expect("re-uploaded"),
            b"edited here meanwhile, longer"
        );
        assert!(local.exists());
    }

    #[test]
    fn a_merge_after_reattach_restores_instead_of_deleting() {
        let fixture = OfflineFixture::new();
        let local = fixture.watch_root.join("kept.txt");
        std::fs::write(&local, b"kept").expect("seed");
        drop(fixture.run(&[&local]));

        std::fs::remove_file(&local).expect("delete while away");
        {
            let mut db = DurableStateDb::open(&fixture.database_path).expect("open");
            db.set_state(
                constants::state::MERGE_WITHOUT_DELETIONS_KEY,
                "1",
                timestamp_ms(0),
            )
            .expect("flag");
        }
        let second = fixture.run(&[]);
        assert_eq!(
            std::fs::read(&local).expect("restored by the merge"),
            b"kept"
        );
        assert!(fixture.cloud_root.join("kept.txt").exists());
        assert!(
            second
                .state_db()
                .state(constants::state::MERGE_WITHOUT_DELETIONS_KEY)
                .expect("read")
                .is_none(),
            "the flag covers one whole-scope walk"
        );
    }

    /// A local file and a cloud directory under one name, or the
    /// reverse: the pair the walk cannot decide on its own.
    fn mismatched_pair(fixture: &OfflineFixture, local_is_dir: bool) -> PathBuf {
        let local = fixture.watch_root.join("notes");
        let cloud = fixture.cloud_root.join("notes");
        if local_is_dir {
            std::fs::create_dir_all(&local).expect("local dir");
            std::fs::write(local.join("inner.txt"), b"inside the local folder").expect("inner");
            std::fs::write(&cloud, b"the cloud file").expect("cloud file");
        } else {
            std::fs::write(&local, b"the local file").expect("local file");
            std::fs::create_dir_all(&cloud).expect("cloud dir");
            std::fs::write(cloud.join("inner.txt"), b"inside the cloud folder").expect("inner");
        }
        local
    }

    fn open_mismatch(runtime: &DaemonRuntime) -> crate::state_db::DecisionRecord {
        runtime
            .state_db()
            .decisions(false)
            .expect("decisions")
            .into_iter()
            .find(|decision| decision.kind == crate::type_mismatch::DECISION_KIND)
            .expect("a type-mismatch decision is open")
    }

    /// A profile with `name` synced as a file, or as a folder with two
    /// files, through one offline run.
    fn offline_fixture_with_synced_entry(name: &str, as_folder: bool) -> OfflineFixture {
        let fixture = OfflineFixture::new();
        let entry = fixture.watch_root.join(name);
        if as_folder {
            std::fs::create_dir_all(&entry).expect("dir");
            std::fs::write(entry.join("one.txt"), b"one").expect("seed");
            std::fs::write(entry.join("two.txt"), b"two").expect("seed");
        } else {
            std::fs::write(&entry, b"a synced file").expect("seed");
        }
        drop(fixture.run(&[]));
        assert_eq!(names_in(&fixture.cloud_root), vec![name.to_string()]);
        fixture
    }

    fn assert_no_question_and_nothing_failed(runtime: &DaemonRuntime) {
        assert!(
            runtime
                .state_db()
                .decisions(false)
                .expect("decisions")
                .is_empty(),
            "one side made the swap: nothing to ask"
        );
        assert_eq!(runtime.state_db().failed_depth().expect("failed"), 0);
    }

    #[test]
    fn a_file_replaced_by_a_folder_on_this_device_reaches_the_cloud_with_no_question() {
        let fixture = offline_fixture_with_synced_entry("thing", false);
        let local = fixture.watch_root.join("thing");
        std::fs::remove_file(&local).expect("remove file");
        std::fs::create_dir(&local).expect("folder");
        std::fs::write(local.join("inner.txt"), b"inside").expect("inner");

        let runtime = fixture.run(&[]);

        assert_eq!(
            std::fs::read(fixture.cloud_root.join("thing/inner.txt")).expect("cloud"),
            b"inside"
        );
        assert_no_question_and_nothing_failed(&runtime);
    }

    #[test]
    fn a_folder_replaced_by_a_file_on_this_device_reaches_the_cloud_with_no_question() {
        let fixture = offline_fixture_with_synced_entry("thing", true);
        let local = fixture.watch_root.join("thing");
        std::fs::remove_dir_all(&local).expect("remove folder");
        std::fs::write(&local, b"now a file").expect("file");

        let runtime = fixture.run(&[]);

        assert_eq!(
            std::fs::read(fixture.cloud_root.join("thing")).expect("cloud"),
            b"now a file"
        );
        assert_no_question_and_nothing_failed(&runtime);
    }

    #[test]
    fn a_folder_replaced_by_a_file_in_the_cloud_reaches_this_device_with_no_question() {
        let fixture = offline_fixture_with_synced_entry("thing", true);
        let cloud = fixture.cloud_root.join("thing");
        std::fs::remove_dir_all(&cloud).expect("remove folder");
        std::fs::write(&cloud, b"now a file").expect("file");

        let runtime = fixture.run(&[]);

        assert_eq!(
            std::fs::read(fixture.watch_root.join("thing")).expect("local"),
            b"now a file"
        );
        // The two files of the folder are in the trash, not lost.
        assert_eq!(runtime.trash().expect("trash").list().len(), 2);
        assert_no_question_and_nothing_failed(&runtime);
    }

    #[cfg(unix)]
    #[test]
    fn a_download_that_waits_for_a_replaced_folder_to_empty_stays_one_queue_row() {
        use std::os::unix::fs::PermissionsExt;
        // The files of the folder cannot leave: the folder refuses a
        // change, so each removal fails and waits for its next try. The
        // download of the file that took the folder's place waits with
        // them. It must wait as the same queue row: completed and
        // queued again by a walk that it asked for itself, it is a
        // loop with no pause.
        let fixture = OfflineFixture::new();
        let folder = fixture.watch_root.join("thing");
        std::fs::create_dir_all(&folder).expect("dir");
        for index in 0..12 {
            std::fs::write(folder.join(format!("{index}.txt")), b"synced").expect("seed");
        }
        drop(fixture.run(&[]));
        let cloud = fixture.cloud_root.join("thing");
        std::fs::remove_dir_all(&cloud).expect("remove folder");
        std::fs::write(&cloud, b"now a file").expect("file");
        let allow_changes = |allowed: bool| {
            let mode = if allowed { 0o755 } else { 0o555 };
            std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(mode))
                .expect("permissions");
        };
        allow_changes(false);
        if std::fs::write(folder.join("probe"), b"").is_ok() {
            // Running as a user that permissions do not stop.
            allow_changes(true);
            return;
        }

        let waiting_download = |runtime: &DaemonRuntime| {
            runtime
                .state_db()
                .list_queue_intents(100)
                .expect("queue")
                .into_iter()
                .find(|intent| intent.kind == PendingIntentKind::Download && intent.path == folder)
                .map(|intent| intent.id)
        };
        let first = fixture.run(&[]);
        let files_left = names_in(&folder).len();
        let row = waiting_download(&first);
        drop(first);
        let second = fixture.run(&[]);
        let row_after_a_restart = waiting_download(&second);
        allow_changes(true);
        assert_eq!(files_left, 12, "the files cannot leave");
        let row = row.expect("the download waits");
        assert_eq!(
            row_after_a_restart,
            Some(row),
            "the same row waits; it was not completed and queued again"
        );
    }

    #[test]
    fn a_file_replaced_by_a_folder_in_the_cloud_reaches_this_device_with_no_question() {
        let fixture = offline_fixture_with_synced_entry("thing", false);
        let cloud = fixture.cloud_root.join("thing");
        std::fs::remove_file(&cloud).expect("remove file");
        std::fs::create_dir(&cloud).expect("folder");
        std::fs::write(cloud.join("inner.txt"), b"inside").expect("inner");

        let runtime = fixture.run(&[]);

        assert_eq!(
            std::fs::read(fixture.watch_root.join("thing/inner.txt")).expect("local"),
            b"inside"
        );
        assert_eq!(runtime.trash().expect("trash").list().len(), 1);
        assert_no_question_and_nothing_failed(&runtime);
    }

    #[test]
    fn a_swap_on_one_side_with_an_edit_on_the_other_is_a_question() {
        let fixture = offline_fixture_with_synced_entry("thing", false);
        // This device swaps the file for a folder, and the cloud file
        // changes: both sides moved.
        let local = fixture.watch_root.join("thing");
        std::fs::remove_file(&local).expect("remove file");
        std::fs::create_dir(&local).expect("folder");
        std::fs::write(local.join("inner.txt"), b"inside").expect("inner");
        std::fs::write(
            fixture.cloud_root.join("thing"),
            b"edited in the cloud, longer",
        )
        .expect("cloud edit");

        let runtime = fixture.run(&[]);

        let decision = open_mismatch(&runtime);
        assert_eq!(decision.path.as_deref(), Some(local.as_path()));
        assert!(local.join("inner.txt").is_file(), "nothing is touched");
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("thing")).expect("cloud"),
            b"edited in the cloud, longer"
        );
    }

    #[test]
    fn a_folder_tree_removed_while_away_leaves_no_empty_folders_in_the_cloud() {
        let fixture = OfflineFixture::new();
        for name in [
            "proj/a/b/deep.txt",
            "proj/a/mid.txt",
            "proj/top.txt",
            "keep.txt",
        ] {
            let path = fixture.watch_root.join(name);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            std::fs::write(&path, name).expect("seed");
        }
        drop(fixture.run(&[]));
        assert!(fixture.cloud_root.join("proj/a/b/deep.txt").is_file());
        // Another device added a file to the tree; this one never saw it.
        std::fs::write(
            fixture.cloud_root.join("proj/a/theirs.txt"),
            b"not mine to delete",
        )
        .expect("cloud-only file");

        std::fs::remove_dir_all(fixture.watch_root.join("proj")).expect("rm -rf");
        let runtime = fixture.run(&[]);

        // The files this device synced are gone, with the folders they
        // left empty. The file it never compared is kept, on both
        // sides, and so is the folder chain that holds it.
        assert!(!fixture.cloud_root.join("proj/a/b").exists());
        assert!(!fixture.cloud_root.join("proj/top.txt").exists());
        assert_eq!(names_in(&fixture.cloud_root.join("proj")), vec!["a"]);
        assert_eq!(
            names_in(&fixture.cloud_root.join("proj/a")),
            vec!["theirs.txt"]
        );
        assert_eq!(
            std::fs::read(fixture.watch_root.join("proj/a/theirs.txt")).expect("kept here too"),
            b"not mine to delete"
        );
        assert!(fixture.cloud_root.join("keep.txt").is_file());
        assert_eq!(runtime.state_db().failed_depth().expect("failed"), 0);
    }

    #[test]
    fn a_type_mismatch_asks_once_and_touches_nothing() {
        let fixture = OfflineFixture::new();
        let local = mismatched_pair(&fixture, false);
        let runtime = fixture.run(&[]);
        let decision = open_mismatch(&runtime);
        assert_eq!(decision.path.as_deref(), Some(local.as_path()));
        assert_eq!(decision.evidence["local"], "file");
        assert_eq!(decision.evidence["cloud"], "directory");
        assert_eq!(std::fs::read(&local).expect("local"), b"the local file");
        assert!(fixture.cloud_root.join("notes/inner.txt").is_file());
        drop(runtime);
        // A second reconcile finds the question already open.
        let runtime = fixture.run(&[]);
        let open: Vec<_> = runtime
            .state_db()
            .decisions(false)
            .expect("decisions")
            .into_iter()
            .filter(|decision| decision.kind == crate::type_mismatch::DECISION_KIND)
            .collect();
        assert_eq!(open.len(), 1, "asked once");
    }

    #[test]
    fn keep_both_moves_the_local_side_aside_and_brings_the_cloud_side_down() {
        let fixture = OfflineFixture::new();
        let local = mismatched_pair(&fixture, false);
        let runtime = fixture.run(&[]);
        let decision = open_mismatch(&runtime);
        drop(runtime);
        {
            let mut db = DurableStateDb::open(&fixture.database_path).expect("open");
            db.resolve_decision(decision.id, "keep-both", timestamp_ms(1))
                .expect("resolve");
        }
        let runtime = fixture.run(&[]);
        // The local file lives on under a conflict name, on both sides.
        let copies: Vec<PathBuf> = std::fs::read_dir(&fixture.watch_root)
            .expect("list")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("notes~conflict-"))
            })
            .collect();
        assert_eq!(copies.len(), 1, "one conflict copy: {copies:?}");
        assert_eq!(std::fs::read(&copies[0]).expect("copy"), b"the local file");
        let copy_name = copies[0].file_name().unwrap().to_owned();
        assert!(
            fixture.cloud_root.join(&copy_name).is_file(),
            "the copy uploaded"
        );
        // The cloud folder came down under the original name.
        assert!(local.is_dir(), "the cloud directory materialized locally");
        assert_eq!(
            std::fs::read(local.join("inner.txt")).expect("inner"),
            b"inside the cloud folder"
        );
        assert!(fixture.cloud_root.join("notes/inner.txt").is_file());
        let open = runtime.state_db().decisions(false).expect("open");
        assert!(open.is_empty(), "{open:?}");
    }

    #[test]
    fn a_daemon_profile_starts_before_the_provider_answers_and_keeps_its_queue() {
        let temp = TempDir::new().expect("temp dir");
        let watch_root = temp.path().join("watch");
        let cloud_root = temp.path().join("cloud");
        std::fs::create_dir_all(&watch_root).expect("watch root");
        std::fs::create_dir_all(&cloud_root).expect("cloud root");
        let cloud_root = vapor_shared::paths::canonicalize(&cloud_root).expect("canonical");
        // What the last run left in the queue.
        let mut state_db =
            DurableStateDb::open(temp.path().join("state/vapor.sqlite")).expect("state db");
        state_db
            .enqueue_intents_coalesced(
                &[(
                    watch_root.join("removed-last-run.txt"),
                    PendingIntentKind::Delete,
                    timestamp_ms(1),
                )],
                crate::safeguards::IntentSource::Fresh,
            )
            .expect("enqueue");
        // A provider that refuses every call: a probe made while the
        // runtime is built would leave it held for a sign-in.
        let signed_out = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let caps: Arc<dyn vapor_platform::fs_caps::FilesystemCapabilities> =
            Arc::new(vapor_platform::fs_caps::NativeFilesystemCapabilities::for_current_host());
        let (provider, _feed) =
            vapor_providers::FilesystemProvider::with_manual_feed(&cloud_root, caps)
                .expect("provider");
        let clock = Arc::new(crate::clock::ManualClock::at_now());
        let app = DaemonApp::new_with_clock(
            Box::new(SwitchedProvider {
                inner: provider,
                signed_out: signed_out.clone(),
                downloads_fail: Arc::default(),
                cloud_full: Arc::default(),
                offline: Arc::default(),
                listings_held: Arc::default(),
            }),
            clock.clone(),
        );
        let mut runtime = DaemonRuntime::build_with_app(
            SyncScope {
                local_root: Some(watch_root.clone()),
                cloud_root: cloud_root.to_string_lossy().into_owned(),
                sync_mode: vapor_shared::SyncMode::TwoWay,
            },
            EventPathFilterOptions::default(),
            state_db,
            app,
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            Startup::DaemonProfile,
        )
        .expect("runtime");
        let snapshot = runtime.app.snapshot();
        assert_eq!(snapshot.run_state, RunState::Starting, "{snapshot:?}");
        assert!(
            snapshot.reason.contains("checking the cloud folder"),
            "no provider call was made while building: {snapshot:?}"
        );
        assert!(!runtime.sign_in_hold);

        // The first tick probes. Nothing is leased under a critical
        // thermal state, so the queue shows what the probe did to it.
        signed_out.store(false, std::sync::atomic::Ordering::SeqCst);
        runtime
            .tick_with_inputs(
                timestamp_ms(1_000),
                ThrottleInputs {
                    thermal_pressure: crate::throttle::ThermalPressure::Critical,
                    ..ThrottleInputs::default()
                },
            )
            .expect("tick");
        assert!(runtime.cloud_root_ready, "the first tick adopted the root");
        assert!(
            !runtime
                .state_db
                .queued_paths_of_kind(PendingIntentKind::Delete)
                .expect("queue")
                .is_empty(),
            "the first answer is the profile's start, not the end of an outage: \
             the deletion from the last run stays queued"
        );
        runtime.tick(timestamp_ms(2_000)).expect("tick");
        assert_eq!(runtime.app.snapshot().run_state, RunState::Running);
    }

    #[test]
    fn keep_both_still_applies_when_the_local_side_already_moved() {
        let fixture = OfflineFixture::new();
        let local = mismatched_pair(&fixture, false);
        let runtime = fixture.run(&[]);
        let decision = open_mismatch(&runtime);
        drop(runtime);
        {
            let mut db = DurableStateDb::open(&fixture.database_path).expect("open");
            db.resolve_decision(decision.id, "keep-both", timestamp_ms(1))
                .expect("resolve");
        }
        // What an earlier try leaves when it fails after its rename: the
        // local side under another name, and nothing at the path.
        let moved = fixture.watch_root.join("notes-moved");
        std::fs::rename(&local, &moved).expect("move aside");

        let runtime = fixture.run(&[]);
        assert!(
            runtime
                .state_db()
                .decisions(false)
                .expect("open")
                .is_empty(),
            "the answer applied instead of failing on the rename for ever"
        );
        assert_eq!(std::fs::read(&moved).expect("moved"), b"the local file");
        assert!(fixture.cloud_root.join("notes-moved").is_file());
        assert!(local.is_dir(), "the cloud directory came down");
    }

    #[test]
    fn an_answer_that_cannot_be_applied_is_tried_again_after_a_wait_and_logged_once() {
        let fixture = OfflineFixture::new();
        let mut runtime = fixture.run(&[]);
        // A question of a kind this build has no applier for, answered.
        let id = runtime
            .state_db
            .create_decision(
                "a-kind-from-a-newer-build",
                crate::state_db::DecisionScope::Profile,
                None,
                "A question a newer build asked",
                &[crate::state_db::DecisionOption {
                    key: "go".to_string(),
                    label: "Go".to_string(),
                }],
                &serde_json::json!({}),
                timestamp_ms(1),
            )
            .expect("create");
        runtime
            .state_db
            .resolve_decision(id, "go", timestamp_ms(2))
            .expect("resolve");
        let start = timestamp_ms(10_000);
        let tries = |runtime: &DaemonRuntime| {
            runtime
                .decision_retries
                .get(&id)
                .map(|retry| retry.failures)
                .unwrap_or(0)
        };

        runtime.apply_resolved_decisions(start).expect("apply");
        assert_eq!(tries(&runtime), 1);
        // The ticks inside the wait do not try, so they log nothing.
        for offset_ms in [100, 1_000, 4_900] {
            runtime
                .apply_resolved_decisions(start + Duration::from_millis(offset_ms))
                .expect("apply");
        }
        assert_eq!(tries(&runtime), 1, "one try per wait, not one per tick");
        let waits = constants::decision::APPLY_RETRY_SECONDS;
        runtime
            .apply_resolved_decisions(start + Duration::from_secs(waits[0]))
            .expect("apply");
        assert_eq!(tries(&runtime), 2);
        runtime
            .apply_resolved_decisions(start + Duration::from_secs(waits[0] + waits[1] - 1))
            .expect("apply");
        assert_eq!(tries(&runtime), 2, "the second wait is longer");
        assert!(
            runtime
                .state_db()
                .decision(id)
                .expect("read")
                .expect("decision")
                .applied_at
                .is_none(),
            "the answer stays recorded and unapplied"
        );

        // The question is asked again: its retry state goes with the answer.
        runtime.state_db.reopen_decision(id).expect("reopen");
        runtime
            .apply_resolved_decisions(start + Duration::from_secs(3_600))
            .expect("apply");
        assert!(runtime.decision_retries.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_name_that_cannot_be_synced_is_asked_about_once_and_withdrawn_when_renamed() {
        use std::os::unix::ffi::OsStrExt;
        let fixture = OfflineFixture::new();
        let bad = fixture
            .watch_root
            .join(std::ffi::OsStr::from_bytes(b"caf\xe9.txt"));
        if std::fs::write(&bad, b"latin-1 name").is_err() {
            // This filesystem refuses a name that is not UTF-8 (APFS
            // does), so the case cannot arise here.
            return;
        }
        std::fs::write(fixture.watch_root.join("fine.txt"), b"fine").expect("seed");

        let runtime = fixture.run(&[]);
        let open: Vec<_> = runtime
            .state_db()
            .decisions(false)
            .expect("decisions")
            .into_iter()
            .filter(|decision| decision.kind == crate::unsyncable::DECISION_KIND)
            .collect();
        assert_eq!(open.len(), 1, "asked once: {open:?}");
        assert!(
            open[0].question.contains("not valid UTF-8"),
            "{}",
            open[0].question
        );
        assert!(fixture.cloud_root.join("fine.txt").is_file());
        assert_eq!(
            std::fs::read_dir(&fixture.cloud_root)
                .expect("cloud")
                .flatten()
                .filter(|entry| entry.file_name().to_str().is_none())
                .count(),
            0,
            "the name never reaches the cloud"
        );
        drop(runtime);

        // Another walk asks nothing new.
        let runtime = fixture.run(&[]);
        assert_eq!(
            runtime
                .state_db()
                .decisions(true)
                .expect("history")
                .iter()
                .filter(|decision| decision.kind == crate::unsyncable::DECISION_KIND)
                .count(),
            1
        );
        drop(runtime);

        // Renamed to something the model carries: the question goes
        // and the file syncs.
        std::fs::rename(&bad, fixture.watch_root.join("cafe.txt")).expect("rename");
        let runtime = fixture.run(&[]);
        let history: Vec<_> = runtime
            .state_db()
            .decisions(true)
            .expect("history")
            .into_iter()
            .filter(|decision| decision.kind == crate::unsyncable::DECISION_KIND)
            .collect();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].choice.as_deref(), Some("withdrawn"));
        assert!(fixture.cloud_root.join("cafe.txt").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_name_with_a_backslash_is_asked_about_one_time_over_a_walk_of_many_slices() {
        let fixture = OfflineFixture::new();
        // A tree that one slice of the walk does not cover, with the
        // name in the first folder the walk reads.
        std::fs::write(fixture.watch_root.join("back\\slash.txt"), b"kept here").expect("seed");
        for index in 0..150 {
            let folder = fixture.watch_root.join(format!("d{index:03}"));
            std::fs::create_dir_all(&folder).expect("dir");
            std::fs::write(folder.join("f.txt"), b"x").expect("seed");
        }
        let questions = |runtime: &DaemonRuntime, closed_too: bool| {
            runtime
                .state_db()
                .decisions(closed_too)
                .expect("decisions")
                .into_iter()
                .filter(|decision| decision.kind == crate::unsyncable::DECISION_KIND)
                .count()
        };

        let runtime = fixture.run(&[]);
        assert_eq!(questions(&runtime, false), 1, "the question stays open");
        assert_eq!(questions(&runtime, true), 1, "and it was asked one time");
        drop(runtime);

        // The next walk finds the same name: the same question.
        let runtime = fixture.run(&[]);
        assert_eq!(questions(&runtime, false), 1);
        assert_eq!(questions(&runtime, true), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_name_in_the_cloud_folder_that_cannot_be_synced_is_asked_about_too() {
        let fixture = OfflineFixture::new();
        std::fs::write(
            fixture.cloud_root.join("back\\slash.txt"),
            b"stays in the cloud",
        )
        .expect("seed");
        std::fs::write(fixture.cloud_root.join("fine.txt"), b"fine").expect("seed");
        let questions = |runtime: &DaemonRuntime, closed_too: bool| -> Vec<_> {
            runtime
                .state_db()
                .decisions(closed_too)
                .expect("decisions")
                .into_iter()
                .filter(|decision| decision.kind == crate::unsyncable::DECISION_KIND)
                .collect()
        };

        let runtime = fixture.run(&[]);
        let open = questions(&runtime, false);
        assert_eq!(open.len(), 1, "asked once: {open:?}");
        assert!(
            open[0]
                .question
                .starts_with("back\\slash.txt in the cloud folder cannot be synced"),
            "{}",
            open[0].question
        );
        assert_eq!(open[0].evidence["side"], "cloud");
        assert!(fixture.watch_root.join("fine.txt").is_file());
        assert!(
            !fixture.watch_root.join("back\\slash.txt").exists(),
            "the name never reaches this device"
        );
        drop(runtime);

        // The next walk finds the same name: the same question.
        let runtime = fixture.run(&[]);
        assert_eq!(questions(&runtime, true).len(), 1);
        drop(runtime);

        // Renamed in the cloud: the question goes and the file syncs.
        std::fs::rename(
            fixture.cloud_root.join("back\\slash.txt"),
            fixture.cloud_root.join("slash.txt"),
        )
        .expect("rename");
        let runtime = fixture.run(&[]);
        let history = questions(&runtime, true);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].choice.as_deref(), Some("withdrawn"));
        assert!(fixture.watch_root.join("slash.txt").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn skipping_an_unsyncable_name_stops_the_asking() {
        use std::os::unix::ffi::OsStrExt;
        let fixture = OfflineFixture::new();
        let bad = fixture
            .watch_root
            .join(std::ffi::OsStr::from_bytes(b"bad\xff.bin"));
        if std::fs::write(&bad, b"x").is_err() {
            return;
        }
        let runtime = fixture.run(&[]);
        let decision = runtime
            .state_db()
            .decisions(false)
            .expect("decisions")
            .into_iter()
            .find(|decision| decision.kind == crate::unsyncable::DECISION_KIND)
            .expect("asked");
        drop(runtime);
        {
            let mut db = DurableStateDb::open(&fixture.database_path).expect("open");
            db.resolve_decision(decision.id, "skip", timestamp_ms(1))
                .expect("resolve");
        }
        let runtime = fixture.run(&[]);
        let all: Vec<_> = runtime
            .state_db()
            .decisions(true)
            .expect("history")
            .into_iter()
            .filter(|decision| decision.kind == crate::unsyncable::DECISION_KIND)
            .collect();
        assert_eq!(all.len(), 1, "skip is honoured on the next walk: {all:?}");
        assert!(all[0].applied_at.is_some(), "the skip answer is applied");
    }

    #[test]
    fn prefer_cloud_trashes_the_local_side() {
        let fixture = OfflineFixture::new();
        let local = mismatched_pair(&fixture, true);
        let runtime = fixture.run(&[]);
        let decision = open_mismatch(&runtime);
        assert_eq!(decision.evidence["local"], "directory");
        drop(runtime);
        {
            let mut db = DurableStateDb::open(&fixture.database_path).expect("open");
            db.resolve_decision(decision.id, "prefer-cloud", timestamp_ms(1))
                .expect("resolve");
        }
        let runtime = fixture.run(&[]);
        assert!(local.is_file(), "the cloud file took the name");
        assert_eq!(std::fs::read(&local).expect("file"), b"the cloud file");
        let trashed = runtime.trash().expect("trash").list();
        assert_eq!(trashed.len(), 1);
        assert_eq!(trashed[0].kind, "directory");
        assert!(
            runtime
                .trash()
                .expect("trash")
                .root()
                .join(&trashed[0].id)
                .join("notes/inner.txt")
                .is_file(),
            "the local folder is whole in the trash"
        );
        assert!(
            !fixture.cloud_root.join("notes/inner.txt").exists(),
            "nothing of the local folder went up"
        );
    }

    #[test]
    fn prefer_local_removes_the_cloud_side_and_uploads_the_local_folder() {
        let fixture = OfflineFixture::new();
        let local = mismatched_pair(&fixture, true);
        let runtime = fixture.run(&[]);
        let decision = open_mismatch(&runtime);
        drop(runtime);
        {
            let mut db = DurableStateDb::open(&fixture.database_path).expect("open");
            db.resolve_decision(decision.id, "prefer-local", timestamp_ms(1))
                .expect("resolve");
        }
        let runtime = fixture.run(&[]);
        assert!(local.is_dir(), "the local folder stays");
        assert!(
            fixture.cloud_root.join("notes").is_dir(),
            "the cloud file is gone and the folder went up"
        );
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("notes/inner.txt")).expect("uploaded"),
            b"inside the local folder"
        );
        assert!(
            runtime
                .state_db()
                .decisions(false)
                .expect("open")
                .is_empty(),
            "no second question while the answer lands"
        );
    }

    #[test]
    fn offline_same_size_cloud_edit_is_downloaded_by_the_startup_reconcile() {
        // The mirror image: the cloud copy is rewritten with the same
        // byte count while no daemon runs (the filesystem provider's
        // changes cursor is process-local, so the feed never reports
        // it). Only the remote mtime the index recorded can reveal it,
        // and since the local copy is untouched the answer is a
        // download, never a conflict copy.
        let temp = TempDir::new().expect("temp dir");
        let watch_root = temp.path().join("watch");
        let cloud_root = temp.path().join("cloud");
        std::fs::create_dir_all(&watch_root).expect("watch root");
        std::fs::create_dir_all(&cloud_root).expect("cloud root");
        let watch_root =
            vapor_shared::paths::canonicalize(&watch_root).expect("canonical watch root");
        let database_path = temp.path().join("state/vapor.sqlite");
        let local_file = watch_root.join("notes.txt");
        std::fs::write(&local_file, b"version-A").expect("seed local");

        let sync_scope = || SyncScope {
            local_root: Some(watch_root.clone()),
            cloud_root: cloud_root.to_string_lossy().into_owned(),
            sync_mode: vapor_shared::SyncMode::TwoWay,
        };
        use crate::clock::Clock as _;
        let drive = |runtime: &mut DaemonRuntime, clock: &Arc<crate::clock::ManualClock>| {
            for _ in 0..400 {
                clock.advance(Duration::from_millis(250));
                clock.advance_system(Duration::from_millis(250));
                runtime
                    .tick_with_inputs(clock.now_system(), ThrottleInputs::default())
                    .expect("tick");
                if runtime.state_db().queue_depth().expect("depth") == 0
                    && runtime.state_db().leased_depth().expect("leased") == 0
                {
                    break;
                }
            }
        };
        {
            let mut state_db = DurableStateDb::open(&database_path).expect("open state db");
            state_db
                .enqueue_intent(&local_file, PendingIntentKind::Upload, timestamp_ms(0))
                .expect("enqueue");
            let clock = Arc::new(crate::clock::ManualClock::at_now());
            let mut runtime = DaemonRuntime::build(
                sync_scope(),
                EventPathFilterOptions::default(),
                state_db,
                Box::new(vapor_providers::FilesystemProvider::new()),
                Arc::new(StaticMetricsSampler::default()),
                clock.clone(),
                false,
            )
            .expect("first runtime");
            drive(&mut runtime, &clock);
            let index = runtime
                .state_db()
                .sync_index(&local_file)
                .expect("index")
                .expect("indexed after upload");
            assert!(
                index.remote_modified_at.is_some(),
                "an upload must record the remote mtime"
            );
        }

        // The cloud side rewrites the file with the same length. The
        // op-id tag survives an in-place write on this filesystem, so
        // the tag alone would call the remote unchanged.
        let cloud_file = cloud_root.join("notes.txt");
        std::fs::write(&cloud_file, b"version-C").expect("edit cloud offline");
        let later = std::fs::metadata(&cloud_file)
            .expect("metadata")
            .modified()
            .expect("mtime")
            + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&cloud_file)
            .expect("open for mtime")
            .set_modified(later)
            .expect("set mtime");

        let state_db = DurableStateDb::open(&database_path).expect("reopen state db");
        let clock = Arc::new(crate::clock::ManualClock::at_now());
        let mut runtime = DaemonRuntime::build(
            sync_scope(),
            EventPathFilterOptions::default(),
            state_db,
            Box::new(vapor_providers::FilesystemProvider::new()),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            false,
        )
        .expect("second runtime");
        runtime
            .enqueue_startup_reconstruction_reconcile(clock.now_system())
            .expect("queue startup reconcile");
        drive(&mut runtime, &clock);
        assert_eq!(
            std::fs::read(&local_file).expect("local copy"),
            b"version-C",
            "the offline same-size cloud edit must reach this device"
        );
        assert_eq!(
            std::fs::read(&cloud_file).expect("cloud copy"),
            b"version-C",
            "the cloud copy must not be overwritten by the stale local one"
        );
        let entries: Vec<String> = std::fs::read_dir(&watch_root)
            .expect("list")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| !vapor_providers::filesystem::is_internal_file_name(name))
            .collect();
        assert_eq!(entries, vec!["notes.txt".to_string()], "no conflict copy");
    }

    #[test]
    fn paused_daemon_stops_admitting_new_work_and_resume_restores_it() {
        // A pause is more than the status string: ingest keeps
        // capturing intents, and nothing new is leased until resume.
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut state_db = DurableStateDb::open(&database_path).expect("open durable state db");
        state_db
            .enqueue_intent(
                &watch_root.join("src/main.rs"),
                PendingIntentKind::Upload,
                timestamp_ms(0),
            )
            .expect("enqueue upload intent");

        let clock = Arc::new(crate::clock::ManualClock::at_now());
        let mut runtime = DaemonRuntime::build(
            test_sync_scope(&watch_root),
            EventPathFilterOptions::default(),
            state_db,
            inert_stub_provider(),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            false,
        )
        .expect("runtime");
        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        runtime.attach_control(control.clone());

        control.request_pause();
        clock.advance(Duration::from_millis(250));
        let paused_tick = runtime
            .tick_with_inputs(timestamp_ms(250), ThrottleInputs::default())
            .expect("paused tick");

        assert_eq!(runtime.app().snapshot().run_state, RunState::Paused);
        assert_eq!(paused_tick.leased_intents, 0, "paused must not lease work");
        assert_eq!(paused_tick.started_staged_intents, 0);
        assert_eq!(runtime.state_db().pending_depth().expect("pending"), 1);

        // Ingest keeps capturing intent state while paused.
        let recorder = runtime.recorder.as_ref().expect("recorder").clone();
        let runtime_watch_root = runtime.sync_scope().local_root.clone().expect("watch root");
        FsEventRecording::record_event(
            recorder.as_ref(),
            FsEventRecord {
                path: runtime_watch_root.join("src/other.rs"),
                kind: FsEventKind::Modified,
                observed_at: timestamp_ms(300),
            },
        );
        clock.advance(Duration::from_millis(1_500));
        let capture_tick = runtime
            .tick_with_inputs(timestamp_ms(1_750), ThrottleInputs::default())
            .expect("capture tick");
        assert_eq!(capture_tick.stabilized_events, 1);
        assert_eq!(capture_tick.durable_enqueues, 1);
        assert_eq!(capture_tick.leased_intents, 0, "still paused");
        assert_eq!(runtime.state_db().pending_depth().expect("pending"), 2);

        control.request_resume();
        clock.advance(Duration::from_millis(250));
        let resumed_tick = runtime
            .tick_with_inputs(timestamp_ms(2_000), ThrottleInputs::default())
            .expect("resumed tick");
        assert_eq!(runtime.app().snapshot().run_state, RunState::Running);
        assert_eq!(resumed_tick.leased_intents, 2, "resume admits the backlog");
    }

    #[test]
    fn checkpoint_paused_reconcile_does_not_duplicate_durable_rows() {
        // A checkpoint-paused reconcile is tracked both as a requeued
        // durable row and as a pending scheduler intent. The flush must
        // coalesce the two: a fresh durable row for the scheduler copy
        // on every pause would multiply whole-subtree reconciles.
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        // Canonical paths throughout: the walker maps local paths under
        // the runtime's canonicalized scope root, so the enqueued
        // subtree intent must live under the same canonical form.
        let watch_root =
            vapor_shared::paths::canonicalize(&watch_root).expect("canonical watch root");
        let cloud_root = temp_dir.path().join("cloud");
        std::fs::create_dir_all(&cloud_root).expect("create cloud root");
        let cloud_root =
            vapor_shared::paths::canonicalize(&cloud_root).expect("canonical cloud root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut state_db = DurableStateDb::open(&database_path).expect("open durable state db");
        let subtree_root = watch_root.join("project");
        // Enough matched directories on BOTH sides that one walk chunk
        // (RECONCILE_DIRS_PER_SLICE_IDLE) cannot finish the
        // comparison: a finished walk completes instead of pausing, so
        // observing pause cycles requires a genuinely in-progress walk.
        for index in 0..(constants::engine::RECONCILE_DIRS_PER_SLICE_IDLE * 2) {
            std::fs::create_dir_all(subtree_root.join(format!("dir-{index}")))
                .expect("create local walk fodder");
            std::fs::create_dir_all(cloud_root.join(format!("project/dir-{index}")))
                .expect("create cloud walk fodder");
        }
        state_db
            .enqueue_intent(
                &subtree_root,
                PendingIntentKind::ReconcileSubtree,
                timestamp_ms(0),
            )
            .expect("enqueue reconcile intent");

        let clock = Arc::new(crate::clock::ManualClock::at_now());
        let caps: Arc<dyn vapor_platform::fs_caps::FilesystemCapabilities> =
            Arc::new(vapor_platform::fs_caps::NativeFilesystemCapabilities::for_current_host());
        let (provider, _feed) =
            vapor_providers::FilesystemProvider::with_manual_feed(&cloud_root, caps)
                .expect("manual-feed provider");
        let sync_scope = SyncScope {
            local_root: Some(watch_root.clone()),
            cloud_root: cloud_root.to_string_lossy().into_owned(),
            sync_mode: vapor_shared::SyncMode::TwoWay,
        };
        let mut runtime = DaemonRuntime::build(
            sync_scope,
            EventPathFilterOptions::default(),
            state_db,
            Box::new(provider),
            Arc::new(StaticMetricsSampler::default()),
            clock.clone(),
            false,
        )
        .expect("runtime");

        // Tick 1 (idle): the reconcile is leased and starts running.
        // Ticks step 1 s so each one re-samples the throttle inputs.
        clock.advance(Duration::from_secs(1));
        let started = runtime
            .tick_with_inputs(timestamp_ms(1_000), ThrottleInputs::default())
            .expect("start tick");
        assert!(started.started_reconcile_root.is_some());
        assert_eq!(runtime.state_db().queue_depth().expect("depth"), 1);

        // Tick 2 (the device is about to overheat): the reconcile checkpoint-pauses.
        clock.advance(Duration::from_secs(1));
        let paused = runtime
            .tick_with_inputs(
                timestamp_ms(2_000),
                ThrottleInputs {
                    thermal_pressure: crate::throttle::ThermalPressure::Critical,
                    ..ThrottleInputs::default()
                },
            )
            .expect("pause tick");
        assert_eq!(paused.requeued_intents, 1, "report: {paused:?}");
        assert_eq!(runtime.state_db().queue_depth().expect("depth"), 1);

        // Ticks 3..6 (still suspended): coalescing keeps the depth at 1
        // through every pause cycle.
        for step in 1..=4u64 {
            clock.advance(Duration::from_secs(1));
            runtime
                .tick_with_inputs(
                    timestamp_ms(2_000 + step * 1_000),
                    ThrottleInputs {
                        thermal_pressure: crate::throttle::ThermalPressure::Critical,
                        ..ThrottleInputs::default()
                    },
                )
                .expect("busy tick");
            assert_eq!(
                runtime.state_db().queue_depth().expect("depth"),
                1,
                "paused reconcile must never multiply durable rows"
            );
        }
    }

    #[test]
    fn tick_consults_metrics_sampler_and_propagates_decision_to_workgate() {
        // Invariant: the runtime's public `tick(now)` path must obtain
        // its ThrottleInputs from the injected `MetricsSampler` and feed
        // them through to the throttle controller / workgate. We inject a
        // sampler that reports user-active + warm thermal state; the
        // resulting throttle decision must downshift away from `Idle`.
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");

        let active_user_inputs = ThrottleInputs {
            user_active: true,
            thermal_pressure: ThermalPressure::Fair,
            ..ThrottleInputs::default()
        };
        let sampler: Arc<dyn MetricsSampler> =
            Arc::new(StaticMetricsSampler::new(active_user_inputs));

        let mut runtime = DaemonRuntime::start_with_sampler(
            test_sync_scope(&watch_root),
            state_db,
            inert_stub_provider(),
            sampler,
        )
        .expect("runtime");

        // The sampler's first sample drives the first tick's throttle
        // decision. We assert via the live decision exposed on the app.
        runtime.tick(SystemTime::now()).expect("runtime tick");

        let decision = runtime
            .app()
            .throttle_decision()
            .expect("throttle decision recorded after first tick");
        assert_eq!(decision.state, vapor_shared::ThrottleState::Throttled);
    }

    #[test]
    fn tick_with_default_static_sampler_picks_idle() {
        // Sanity check: with no overrides, the default static sampler
        // mirrors `ThrottleInputs::default()` so the engine starts in
        // Idle: the existing behavior the clock plumbing must preserve.
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");

        let mut runtime = DaemonRuntime::start(
            test_sync_scope(&watch_root),
            state_db,
            inert_stub_provider(),
        )
        .expect("runtime");
        runtime.tick(SystemTime::now()).expect("runtime tick");

        let decision = runtime
            .app()
            .throttle_decision()
            .expect("throttle decision recorded after first tick");
        assert_eq!(decision.state, vapor_shared::ThrottleState::Idle);
    }

    pub(super) fn timestamp_ms(milliseconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(milliseconds)
    }

    #[test]
    fn attached_status_publisher_receives_fresh_snapshot_each_tick() {
        // Without the publisher hook, `vapor status` would return the
        // boot snapshot for ever. A recording publisher shows the run
        // state flip through it after a pause request.
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");

        let mut runtime = DaemonRuntime::build(
            test_sync_scope(&watch_root),
            EventPathFilterOptions::default(),
            state_db,
            inert_stub_provider(),
            Arc::new(StaticMetricsSampler::default()),
            system_clock(),
            false,
        )
        .expect("runtime");

        let publisher = Arc::new(RecordingStatusPublisher::default());
        runtime.attach_status_publisher(publisher.clone());

        // Attaching the publisher must immediately flush an initial
        // snapshot so consumers don't wait for the first tick.
        let initial = publisher.last().expect("attach publishes initial snapshot");
        assert_eq!(initial.run_state, "running");

        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        runtime.attach_control(control.clone());
        control.request_pause();

        runtime
            .tick_with_inputs(timestamp_ms(1_500), ThrottleInputs::default())
            .expect("runtime tick");

        let after = publisher
            .last()
            .expect("publisher saw a post-tick snapshot");
        assert_eq!(after.run_state, "paused");
    }

    #[derive(Debug, Default)]
    struct RecordingStatusPublisher {
        last: std::sync::Mutex<Option<crate::ipc_service::DaemonStatusSnapshot>>,
    }

    impl RecordingStatusPublisher {
        fn last(&self) -> Option<crate::ipc_service::DaemonStatusSnapshot> {
            self.last
                .lock()
                .expect("RecordingStatusPublisher mutex poisoned")
                .clone()
        }
    }

    impl crate::ipc_service::StatusPublisher for RecordingStatusPublisher {
        fn publish(&self, snapshot: crate::ipc_service::DaemonStatusSnapshot) {
            *self
                .last
                .lock()
                .expect("RecordingStatusPublisher mutex poisoned") = Some(snapshot);
        }
    }

    #[test]
    fn repeated_download_intent_for_an_already_synced_path_completes() {
        // Overlapping whole-scope reconciles (startup + cursor-expiry +
        // user-requested) can schedule a second download for a path the
        // first pass already converged. The second intent must complete
        // (as a no-op or harmless re-download), never wedge leased.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline

        std::fs::write(fixture.cloud_root.join("twice.txt"), b"payload").expect("seed remote");
        fixture.feed.emit_created(
            fixture.cloud_root.join("twice.txt"),
            timestamp_ms(fixture.now_ms),
        );
        fixture.converge(12);
        assert!(fixture.watch_root.join("twice.txt").is_file());

        fixture
            .runtime
            .state_db
            .enqueue_intent(
                &fixture.watch_root.join("twice.txt"),
                PendingIntentKind::Download,
                timestamp_ms(fixture.now_ms),
            )
            .expect("second download enqueues");
        fixture.converge(12);
        assert_eq!(
            fixture.runtime.state_db().queue_depth().expect("depth"),
            0,
            "duplicate download must complete, not wedge leased"
        );
    }

    #[test]
    fn requested_reconcile_downloads_external_cloud_file_and_drains() {
        // The e2e S10 shape: a file appears in the cloud root without a
        // feed event (external write) and the user runs `vapor
        // reconcile`. The scheduled download must complete and the queue
        // drain: nothing may wedge leased.
        let mut fixture = BidirectionalFixture::new();
        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        fixture.runtime.attach_control(control.clone());
        fixture.tick(6_000); // baseline

        std::fs::write(fixture.cloud_root.join("external.txt"), b"external payload")
            .expect("seed cloud file without a feed event");
        control.request_reconcile();

        let mut drained = false;
        for _ in 0..30 {
            fixture.tick(1_000);
            if fixture.runtime.state_db().queue_depth().expect("depth") == 0
                && fixture.watch_root.join("external.txt").is_file()
            {
                drained = true;
                break;
            }
        }
        let leftovers = fixture
            .runtime
            .state_db()
            .list_queue_intents(16)
            .expect("list");
        assert!(
            drained,
            "queue must drain and the external file must download; leftovers: {leftovers:?}"
        );
    }

    #[test]
    fn ignored_names_never_cross_sides_or_manufacture_conflicts() {
        // The `.DS_Store` shape from manual testing: Finder writes
        // divergent metadata into both the watched root and the cloud
        // mirror. Ignore rules must hold in *both* directions, through
        // the changes feed and through a requested reconcile, so the
        // divergence never downloads, never uploads, and never produces
        // a keep-both conflict copy.
        let mut fixture = BidirectionalFixture::new();
        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        fixture.runtime.attach_control(control.clone());
        fixture.tick(6_000); // baseline

        std::fs::write(fixture.watch_root.join(".DS_Store"), b"local finder state")
            .expect("seed local");
        std::fs::write(
            fixture.cloud_root.join(".DS_Store"),
            b"divergent cloud bytes",
        )
        .expect("seed cloud");
        fixture.feed.emit_created(
            fixture.cloud_root.join(".DS_Store"),
            timestamp_ms(fixture.now_ms),
        );
        // Control file proving the pipeline still moves real content.
        std::fs::write(fixture.cloud_root.join("real.txt"), b"real payload").expect("seed cloud");

        control.request_reconcile();
        fixture.converge(20);

        assert_eq!(
            std::fs::read(fixture.watch_root.join("real.txt")).expect("control file downloads"),
            b"real payload"
        );
        assert_eq!(
            std::fs::read(fixture.watch_root.join(".DS_Store")).expect("local bytes"),
            b"local finder state",
            "local ignored file must never be overwritten from the cloud side"
        );
        assert_eq!(
            std::fs::read(fixture.cloud_root.join(".DS_Store")).expect("cloud bytes"),
            b"divergent cloud bytes",
            "cloud ignored file must never be overwritten from the local side"
        );
        for root in [&fixture.watch_root, &fixture.cloud_root] {
            let conflicts: Vec<_> = std::fs::read_dir(root)
                .expect("read root")
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().contains("~conflict-"))
                .collect();
            assert!(
                conflicts.is_empty(),
                "ignored divergence must not manufacture conflict copies: {conflicts:?}"
            );
        }
    }

    #[test]
    fn local_deletion_propagates_even_when_a_write_fragment_arrives_last() {
        // Real fs-watch backends split one unlink into several
        // fragments and the write-kind one can land last (FSEvents flag
        // coalescing). The delete must still reach the cloud: read as
        // an upload, it would end as "vanished before upload" and leave
        // the remote copy there for ever.
        let mut fixture = BidirectionalFixture::new();
        std::fs::write(fixture.watch_root.join("doomed.txt"), b"payload").expect("seed");
        fixture.record_local_event(
            &fixture.watch_root.join("doomed.txt"),
            FsEventKind::Created,
            fixture.now_ms,
        );
        fixture.converge(12);
        assert!(
            fixture.cloud_root.join("doomed.txt").is_file(),
            "seed file must upload first"
        );

        std::fs::remove_file(fixture.watch_root.join("doomed.txt")).expect("unlink");
        fixture.record_local_event(
            &fixture.watch_root.join("doomed.txt"),
            FsEventKind::Removed,
            fixture.now_ms,
        );
        // The trailing write-kind fragment notify delivers after the
        // unlink: the part that must not flip the intent to Upload.
        fixture.record_local_event(
            &fixture.watch_root.join("doomed.txt"),
            FsEventKind::Modified,
            fixture.now_ms,
        );

        fixture.converge(12);
        assert!(
            !fixture.cloud_root.join("doomed.txt").exists(),
            "the local deletion must propagate to the cloud"
        );
    }

    #[test]
    fn a_renamed_directory_uploads_its_children_and_removes_the_old_tree_remotely() {
        // A directory rename reaches the daemon as Removed(old) +
        // Created(new) with no events for the children. The new
        // directory's files must upload (reported as synthesized
        // events) and the old remote tree must go, file by file, through
        // the guarded delete path.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let old_dir = fixture.watch_root.join("folder");
        std::fs::create_dir_all(old_dir.join("sub")).expect("dirs");
        std::fs::write(old_dir.join("a.txt"), b"child a").expect("a");
        std::fs::write(old_dir.join("sub/b.txt"), b"child b").expect("b");
        for name in ["a.txt", "sub/b.txt"] {
            fixture.record_local_event(&old_dir.join(name), FsEventKind::Created, fixture.now_ms);
        }
        fixture.converge(12);
        assert!(
            fixture.cloud_root.join("folder/sub/b.txt").is_file(),
            "seed must upload"
        );

        let new_dir = fixture.watch_root.join("moved");
        std::fs::rename(&old_dir, &new_dir).expect("rename directory");
        fixture.record_local_event(&old_dir, FsEventKind::Removed, fixture.now_ms);
        fixture.record_local_event(&new_dir, FsEventKind::Created, fixture.now_ms);
        // Directory events debounce like any other; the synthesized
        // child events then need their own window.
        fixture.converge(24);

        assert_eq!(
            std::fs::read(fixture.cloud_root.join("moved/a.txt")).expect("moved a"),
            b"child a"
        );
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("moved/sub/b.txt")).expect("moved b"),
            b"child b"
        );
        assert!(
            !fixture.cloud_root.join("folder").exists(),
            "the old remote tree must be removed: {:?}",
            files_in(&fixture.cloud_root)
        );
        assert_eq!(fixture.runtime.state_db().queue_depth().expect("depth"), 0);
    }

    #[test]
    fn a_folder_that_moves_in_with_an_ignore_file_syncs_by_the_rules_of_that_file() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        // A project made elsewhere, then moved in: the watcher reports
        // the folder, and nothing for the files under it.
        let staged = fixture
            .watch_root
            .parent()
            .expect("parent")
            .join("staged-project");
        std::fs::create_dir_all(staged.join("src")).expect("dirs");
        std::fs::write(staged.join(".gitignore"), "secret.env\nbuild-output/\n").expect("rules");
        std::fs::write(staged.join("secret.env"), b"TOKEN=not-for-the-cloud").expect("secret");
        std::fs::write(staged.join("src/main.rs"), b"fn main() {}").expect("source");
        std::fs::create_dir_all(staged.join("build-output")).expect("dirs");
        std::fs::write(staged.join("build-output/app.bin"), b"binary").expect("output");
        let project = fixture.watch_root.join("project");
        std::fs::rename(&staged, &project).expect("move in");
        fixture.record_local_event(&project, FsEventKind::Created, fixture.now_ms);
        // The folder's event, the reload of the rules, then the events
        // of its files, each with a quiet window of its own.
        for _ in 0..4 {
            fixture.tick(6_000);
        }
        fixture.converge(24);

        assert_eq!(
            std::fs::read(fixture.cloud_root.join("project/src/main.rs")).expect("source"),
            b"fn main() {}"
        );
        assert!(fixture.cloud_root.join("project/.gitignore").is_file());
        assert!(
            !fixture.cloud_root.join("project/secret.env").exists()
                && !fixture.cloud_root.join("project/build-output").exists(),
            "what the folder's own rules hide never reaches the cloud: {:?}",
            files_in(&fixture.cloud_root)
        );
    }

    #[test]
    fn shutdown_flush_persists_events_still_inside_their_debounce_window() {
        // A write reported by the watcher moments before SIGTERM has not
        // stabilized yet. The shutdown flush must turn it into a durable
        // intent instead of leaving it in memory for the process to take
        // with it.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let file = fixture.watch_root.join("late.txt");
        std::fs::write(&file, b"written just before shutdown").expect("seed");
        fixture.record_local_event(&file, FsEventKind::Created, fixture.now_ms);
        // One immediate tick: the event is inside its quiet window, so
        // nothing reaches the durable queue yet.
        let report = fixture.tick(10);
        assert_eq!(
            report.durable_enqueues, 0,
            "the event must still be debouncing"
        );
        assert_eq!(fixture.runtime.state_db().queue_depth().expect("depth"), 0);

        let flushed = fixture
            .runtime
            .flush_for_shutdown(timestamp_ms(fixture.now_ms + 20))
            .expect("flush");
        assert_eq!(flushed, 1);
        assert_eq!(
            fixture.runtime.state_db().pending_depth().expect("pending"),
            1,
            "the write is durable"
        );
        let diagnostics =
            fixture
                .runtime
                .work_items("default", 10, timestamp_ms(fixture.now_ms + 20));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].path, file.display().to_string());
        assert_eq!(diagnostics[0].action, "upload");
    }

    #[test]
    fn editing_a_file_last_synced_from_an_external_write_uploads_without_conflict() {
        // Real-provider everyday shape: an external writer changes the
        // cloud file (no op-id tag), we download it, then the user edits
        // locally. The upload must overwrite: the index hash proves the
        // remote is exactly what we synced from. This used to
        // manufacture a keep-both copy on every such round-trip.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline

        std::fs::write(fixture.cloud_root.join("shared.txt"), b"external v1")
            .expect("external cloud write");
        fixture.feed.emit_created(
            fixture.cloud_root.join("shared.txt"),
            timestamp_ms(fixture.now_ms),
        );
        fixture.converge(12);
        assert_eq!(
            std::fs::read(fixture.watch_root.join("shared.txt")).expect("downloaded"),
            b"external v1"
        );

        std::fs::write(fixture.watch_root.join("shared.txt"), b"local edit v2")
            .expect("local edit");
        fixture.record_local_event(
            &fixture.watch_root.join("shared.txt"),
            FsEventKind::Modified,
            fixture.now_ms,
        );
        fixture.converge(12);

        assert_eq!(
            std::fs::read(fixture.cloud_root.join("shared.txt")).expect("uploaded"),
            b"local edit v2"
        );
        for root in [&fixture.watch_root, &fixture.cloud_root] {
            let conflicts: Vec<_> = std::fs::read_dir(root)
                .expect("read root")
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().contains("~conflict-"))
                .collect();
            assert!(
                conflicts.is_empty(),
                "editing after syncing from an external write must not conflict: {conflicts:?}"
            );
        }
    }

    // ---- A one-way mode asks before it runs ----

    #[test]
    fn a_one_way_profile_syncs_nothing_until_the_user_confirms_the_mode() {
        let mut fixture =
            BidirectionalFixture::unconfirmed_with_mode(vapor_shared::SyncMode::PullOnly);
        let local_only = fixture.watch_root.join("only-here.txt");
        std::fs::write(&local_only, b"a pull-only mirror removes this").expect("seed local");
        std::fs::write(fixture.cloud_root.join("from-cloud.txt"), b"cloud").expect("seed cloud");
        fixture
            .runtime
            .schedule_startup_reconcile(timestamp_ms(fixture.now_ms))
            .expect("startup scan");
        for _ in 0..10 {
            fixture.tick(6_000);
        }
        assert!(local_only.exists(), "nothing is removed before the answer");
        assert!(
            !fixture.watch_root.join("from-cloud.txt").exists(),
            "nothing syncs before the answer, the safe direction included"
        );
        let snapshot = fixture.runtime.app.snapshot().clone();
        assert_eq!(snapshot.run_state, RunState::Error);
        assert!(
            snapshot
                .reason
                .contains("pull-only needs your confirmation")
                && snapshot.reason.contains("vapor decisions list"),
            "{:?}",
            snapshot.reason
        );

        // A pause and a resume do not hide the question behind "running".
        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        fixture.runtime.attach_control(control.clone());
        control.request_pause();
        fixture.tick(1_000);
        assert_eq!(fixture.runtime.app.snapshot().run_state, RunState::Paused);
        control.request_resume();
        fixture.tick(1_000);
        let resumed = fixture.runtime.app.snapshot().clone();
        assert_eq!(resumed.run_state, RunState::Error);
        assert!(
            resumed.reason.contains("pull-only needs your confirmation"),
            "{:?}",
            resumed.reason
        );
        assert!(local_only.exists());
        let decision = fixture
            .runtime
            .state_db()
            .decisions(false)
            .expect("decisions")
            .into_iter()
            .find(|decision| decision.kind == constants::decision::KIND_ONE_WAY_MODE)
            .expect("the question is a decision");
        assert!(
            decision
                .question
                .contains("removes each file that is only on this device")
        );
        assert_eq!(decision.options.len(), 1);
        assert_eq!(decision.options[0].key, constants::decision::OPTION_ENABLE);

        fixture
            .runtime
            .acknowledge_one_way_mode(timestamp_ms(fixture.now_ms));
        fixture.converge(20);
        assert!(!local_only.exists(), "confirmed: the mirror removes it");
        assert_eq!(
            std::fs::read(fixture.watch_root.join("from-cloud.txt")).expect("mirrored"),
            b"cloud"
        );
        assert_eq!(fixture.runtime.app.snapshot().run_state, RunState::Running);
    }

    #[test]
    fn a_confirmation_holds_for_one_mode_and_one_pair_of_roots() {
        let temp = TempDir::new().expect("temp dir");
        let watch_root = temp.path().join("watch");
        let cloud_root = temp.path().join("cloud");
        let other_cloud = temp.path().join("other-cloud");
        for directory in [&watch_root, &cloud_root, &other_cloud] {
            std::fs::create_dir_all(directory).expect("dir");
        }
        let database_path = temp.path().join("state/vapor.sqlite");
        let start = |mode: vapor_shared::SyncMode, cloud: &std::path::Path| {
            DaemonRuntime::build(
                SyncScope {
                    local_root: Some(watch_root.clone()),
                    cloud_root: cloud.to_string_lossy().into_owned(),
                    sync_mode: mode,
                },
                EventPathFilterOptions::default(),
                DurableStateDb::open(&database_path).expect("open durable state db"),
                Box::new(vapor_providers::FilesystemProvider::new()),
                Arc::new(StaticMetricsSampler::default()),
                Arc::new(crate::clock::ManualClock::at_now()),
                false,
            )
            .expect("runtime")
        };
        let open_questions = |runtime: &DaemonRuntime| {
            runtime
                .state_db()
                .decisions(false)
                .expect("decisions")
                .into_iter()
                .filter(|decision| {
                    decision.kind == constants::decision::KIND_ONE_WAY_MODE && decision.is_open()
                })
                .count()
        };

        let mut first = start(vapor_shared::SyncMode::PushOnly, &cloud_root);
        assert_eq!(open_questions(&first), 1);
        first.acknowledge_one_way_mode(timestamp_ms(1));
        drop(first);

        // The same mirror on the next start: no question.
        let again = start(vapor_shared::SyncMode::PushOnly, &cloud_root);
        assert!(again.one_way_hold.is_none());
        assert_eq!(open_questions(&again), 0);
        drop(again);

        // Another cloud folder is another mirror, and so is the other
        // mode: each one asks.
        let moved = start(vapor_shared::SyncMode::PushOnly, &other_cloud);
        assert!(moved.one_way_hold.is_some());
        assert_eq!(open_questions(&moved), 1);
        drop(moved);
        let flipped = start(vapor_shared::SyncMode::PullOnly, &cloud_root);
        assert!(flipped.one_way_hold.is_some());
        assert_eq!(
            open_questions(&flipped),
            1,
            "the question about the other mirror is withdrawn, not left open"
        );
        drop(flipped);

        // Back to two-way: nothing to confirm, and no question stays.
        let two_way = start(vapor_shared::SyncMode::TwoWay, &cloud_root);
        assert!(two_way.one_way_hold.is_none());
        assert_eq!(open_questions(&two_way), 0);
        drop(two_way);

        // The first mirror again, after the profile ran both ways: the
        // old confirmation does not cover what changed since.
        let back = start(vapor_shared::SyncMode::PushOnly, &cloud_root);
        assert!(back.one_way_hold.is_some());
        assert_eq!(open_questions(&back), 1);
    }

    #[test]
    fn a_confirmation_from_another_surface_releases_a_held_mirror() {
        let temp = TempDir::new().expect("temp dir");
        let watch_root = temp.path().join("watch");
        let cloud_root = temp.path().join("cloud");
        for directory in [&watch_root, &cloud_root] {
            std::fs::create_dir_all(directory).expect("dir");
        }
        let database_path = temp.path().join("state/vapor.sqlite");
        let start = || {
            DaemonRuntime::build(
                SyncScope {
                    local_root: Some(watch_root.clone()),
                    cloud_root: cloud_root.to_string_lossy().into_owned(),
                    sync_mode: vapor_shared::SyncMode::PullOnly,
                },
                EventPathFilterOptions::default(),
                DurableStateDb::open(&database_path).expect("open durable state db"),
                Box::new(vapor_providers::FilesystemProvider::new()),
                Arc::new(StaticMetricsSampler::default()),
                Arc::new(crate::clock::ManualClock::at_now()),
                false,
            )
            .expect("runtime")
        };
        let open_questions = |runtime: &DaemonRuntime| {
            runtime
                .state_db()
                .decisions(false)
                .expect("decisions")
                .into_iter()
                .filter(|decision| {
                    decision.kind == constants::decision::KIND_ONE_WAY_MODE && decision.is_open()
                })
                .count()
        };
        // What `vapor profiles add` does on a terminal after a yes: it
        // writes the confirmation through a connection of its own.
        let confirm_elsewhere = |runtime: &DaemonRuntime| {
            let scope = runtime.sync_scope.clone();
            let mut other = DurableStateDb::open(&database_path).expect("second connection");
            crate::one_way::acknowledge(
                &mut other,
                scope.sync_mode,
                scope.local_root.as_deref().expect("local root"),
                &scope.cloud_root,
                timestamp_ms(1),
            )
            .expect("confirm");
        };

        // The daemon composed the profile before the confirmation
        // landed: it asks, then finds the confirmation and runs.
        let mut held = start();
        assert!(held.one_way_hold.is_some());
        assert_eq!(open_questions(&held), 1);
        confirm_elsewhere(&held);
        held.tick(timestamp_ms(2)).expect("tick");
        assert!(held.one_way_hold.is_none());
        assert_eq!(open_questions(&held), 0);
        assert_eq!(held.app.snapshot().run_state, RunState::Running);
        drop(held);

        // A question left open by a daemon that stopped before the
        // confirmation landed is withdrawn at the next start.
        let mut other = DurableStateDb::open(&database_path).expect("second connection");
        other
            .delete_state(constants::state::ONE_WAY_ACKNOWLEDGED_KEY)
            .expect("forget");
        drop(other);
        let asks = start();
        assert_eq!(open_questions(&asks), 1);
        confirm_elsewhere(&asks);
        drop(asks);
        let runs = start();
        assert!(runs.one_way_hold.is_none());
        assert_eq!(open_questions(&runs), 0);
    }

    // ---- A change in only the spelling of a name ----

    /// The user-visible names in a folder, as the filesystem spells them.
    pub(super) fn names_in(directory: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .expect("list")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with(".vapor"))
            .filter(|name| !name.ends_with(".vapor-meta.json"))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_local_rename_that_changes_only_the_case_moves_the_cloud_object() {
        let mut fixture = BidirectionalFixture::new().watching_the_local_tree();
        fixture.tick(6_000); // baseline
        std::fs::write(fixture.watch_root.join("report.txt"), b"quarterly").expect("seed");
        fixture.converge(12);
        assert_eq!(names_in(&fixture.cloud_root), vec!["report.txt"]);

        std::fs::rename(
            fixture.watch_root.join("report.txt"),
            fixture.watch_root.join("Report.txt"),
        )
        .expect("rename");
        for _ in 0..30 {
            fixture.tick(1_000);
        }
        fixture.converge(12);

        assert_eq!(names_in(&fixture.watch_root), vec!["Report.txt"]);
        assert_eq!(names_in(&fixture.cloud_root), vec!["Report.txt"]);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("Report.txt")).expect("cloud"),
            b"quarterly"
        );
        assert_eq!(fixture.runtime.state_db().queue_depth().expect("depth"), 0);
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0
        );
        assert_eq!(fixture.runtime.conflict_count(), 0);
    }

    #[test]
    fn a_cloud_rename_that_changes_only_the_case_renames_the_local_file() {
        let mut fixture = BidirectionalFixture::new().watching_the_local_tree();
        fixture.tick(6_000); // baseline
        std::fs::write(fixture.watch_root.join("notes.txt"), b"minutes").expect("seed");
        fixture.converge(12);
        assert_eq!(names_in(&fixture.cloud_root), vec!["notes.txt"]);

        std::fs::rename(
            fixture.cloud_root.join("notes.txt"),
            fixture.cloud_root.join("Notes.txt"),
        )
        .expect("cloud rename");
        fixture.feed.emit_removed(
            fixture.cloud_root.join("notes.txt"),
            timestamp_ms(fixture.now_ms),
        );
        fixture.feed.emit_created(
            fixture.cloud_root.join("Notes.txt"),
            timestamp_ms(fixture.now_ms),
        );
        for _ in 0..30 {
            fixture.tick(1_000);
        }
        fixture.converge(12);

        assert_eq!(names_in(&fixture.cloud_root), vec!["Notes.txt"]);
        assert_eq!(names_in(&fixture.watch_root), vec!["Notes.txt"]);
        assert_eq!(
            std::fs::read(fixture.watch_root.join("Notes.txt")).expect("local"),
            b"minutes"
        );
        assert_eq!(
            fixture.runtime.state_db().queue_depth().expect("depth"),
            0,
            "{:?}",
            fixture.runtime.state_db().queued_summaries()
        );
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0,
            "{:?}",
            fixture.runtime.state_db().failed_summaries()
        );
        assert_eq!(fixture.runtime.conflict_count(), 0);
    }

    #[test]
    fn a_local_folder_rename_that_changes_only_the_case_reaches_the_cloud() {
        let mut fixture = BidirectionalFixture::new().watching_the_local_tree();
        fixture.tick(6_000); // baseline
        std::fs::create_dir(fixture.watch_root.join("photos")).expect("dir");
        std::fs::write(fixture.watch_root.join("photos/a.jpg"), b"first").expect("seed");
        std::fs::write(fixture.watch_root.join("photos/b.jpg"), b"second").expect("seed");
        fixture.converge(12);
        assert_eq!(names_in(&fixture.cloud_root), vec!["photos"]);

        std::fs::rename(
            fixture.watch_root.join("photos"),
            fixture.watch_root.join("Photos"),
        )
        .expect("rename");
        for _ in 0..40 {
            fixture.tick(1_000);
        }
        fixture.converge(24);

        assert_eq!(names_in(&fixture.watch_root), vec!["Photos"]);
        assert_eq!(names_in(&fixture.cloud_root), vec!["Photos"]);
        assert_eq!(
            names_in(&fixture.cloud_root.join("Photos")),
            vec!["a.jpg", "b.jpg"]
        );
        assert_eq!(fixture.runtime.state_db().queue_depth().expect("depth"), 0);
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_spelling_changed_while_the_cloud_folder_cannot_be_read_is_carried_when_it_can() {
        use std::os::unix::fs::PermissionsExt;
        let mut fixture = BidirectionalFixture::new().watching_the_local_tree();
        fixture.tick(6_000); // baseline
        let root = fixture.watch_root.clone();
        std::fs::write(root.join("report.txt"), "quarterly").expect("seed");
        fixture.converge(12);
        assert_eq!(names_in(&fixture.cloud_root), vec!["report.txt"]);

        // The cloud folder cannot be read for a while (a share that
        // lost its permissions), and the name changes spelling on this
        // device in that time. "Not there" is not what the cloud says:
        // nothing can be asked.
        let readable = std::fs::metadata(&fixture.cloud_root)
            .expect("stat")
            .permissions();
        std::fs::set_permissions(&fixture.cloud_root, std::fs::Permissions::from_mode(0o000))
            .expect("lock the cloud folder");
        std::fs::rename(root.join("report.txt"), root.join("Report.txt")).expect("rename");
        for _ in 0..12 {
            fixture.tick(1_000);
        }
        std::fs::set_permissions(&fixture.cloud_root, readable).expect("unlock");

        // The rename is still this device's change: it goes to the
        // cloud, and it is not undone here.
        for _ in 0..60 {
            fixture.tick(1_000);
        }
        fixture.converge(24);
        assert_eq!(names_in(&root), vec!["Report.txt"]);
        assert_eq!(names_in(&fixture.cloud_root), vec!["Report.txt"]);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("Report.txt")).expect("cloud"),
            b"quarterly"
        );
    }

    #[test]
    fn work_inside_a_folder_just_renamed_by_case_is_synced_as_what_it_is() {
        let mut fixture = BidirectionalFixture::new().watching_the_local_tree();
        fixture.tick(6_000); // baseline
        let root = fixture.watch_root.clone();
        std::fs::create_dir_all(root.join("Folder/inner")).expect("dirs");
        for (name, body) in [
            ("Folder/edit.txt", "the synced bytes"),
            ("Folder/gone.txt", "to be deleted"),
            ("Folder/keep.txt", "untouched"),
            ("Folder/inner/1.txt", "one"),
            ("Folder/inner/2.txt", "two"),
        ] {
            std::fs::write(root.join(name), body).expect("seed");
        }
        fixture.converge(12);
        assert_eq!(names_in(&fixture.cloud_root), vec!["Folder"]);

        // The rename, and a second and a half later an edit, a
        // deletion, and a rename of a folder inside it: before the
        // rename itself is through its quiet window.
        std::fs::rename(root.join("Folder"), root.join("FOLDER")).expect("rename");
        fixture.tick(1_500);
        std::fs::write(root.join("FOLDER/edit.txt"), "edited after the rename").expect("edit");
        std::fs::remove_file(root.join("FOLDER/gone.txt")).expect("delete");
        std::fs::rename(root.join("FOLDER/inner"), root.join("FOLDER/moved")).expect("rename");
        for _ in 0..60 {
            fixture.tick(1_000);
        }
        fixture.converge(24);

        for side in [&fixture.watch_root, &fixture.cloud_root] {
            assert_eq!(names_in(side), vec!["FOLDER"], "{}", side.display());
            assert_eq!(
                names_in(&side.join("FOLDER")),
                vec!["edit.txt", "keep.txt", "moved"],
                "{}",
                side.display()
            );
            assert_eq!(
                names_in(&side.join("FOLDER/moved")),
                vec!["1.txt", "2.txt"],
                "{}",
                side.display()
            );
            assert_eq!(
                std::fs::read(side.join("FOLDER/edit.txt")).expect("edit"),
                b"edited after the rename",
                "{}",
                side.display()
            );
        }
        assert_eq!(fixture.runtime.conflict_count(), 0);
        assert_eq!(
            fixture.runtime.state_db().queue_depth().expect("depth"),
            0,
            "{:?}",
            fixture.runtime.state_db().queued_summaries()
        );
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0,
            "{:?}",
            fixture.runtime.state_db().failed_summaries()
        );

        // What the cloud says about the move of the folder is not a
        // change: nothing comes down, and nothing leaves this device.
        let at = timestamp_ms(fixture.now_ms);
        fixture
            .feed
            .emit_removed(fixture.cloud_root.join("Folder"), at);
        fixture
            .feed
            .emit_removed(fixture.cloud_root.join("Folder/keep.txt"), at);
        for name in ["FOLDER/keep.txt", "FOLDER/edit.txt", "FOLDER/moved/1.txt"] {
            fixture.feed.emit_created(fixture.cloud_root.join(name), at);
        }
        fixture.converge(12);
        assert_eq!(
            names_in(&fixture.watch_root.join("FOLDER")),
            vec!["edit.txt", "keep.txt", "moved"]
        );
        assert_eq!(fixture.runtime.conflict_count(), 0);
    }

    #[test]
    fn a_folder_renamed_in_the_cloud_by_capitals_only_is_renamed_here() {
        let mut fixture = BidirectionalFixture::new().watching_the_local_tree();
        fixture.tick(6_000); // baseline
        std::fs::create_dir(fixture.watch_root.join("CaseDir")).expect("dir");
        std::fs::write(fixture.watch_root.join("CaseDir/a.txt"), b"first").expect("seed");
        std::fs::write(fixture.watch_root.join("CaseDir/b.txt"), b"second").expect("seed");
        fixture.converge(12);
        assert_eq!(names_in(&fixture.cloud_root), vec!["CaseDir"]);

        // The rename in the cloud, as its watcher reports it: the files
        // under the new spelling appear, the ones under the old one go.
        std::fs::rename(
            fixture.cloud_root.join("CaseDir"),
            fixture.cloud_root.join("casedir"),
        )
        .expect("rename in the cloud");
        let at = timestamp_ms(fixture.now_ms);
        for name in ["a.txt", "b.txt"] {
            fixture
                .feed
                .emit_removed(fixture.cloud_root.join("CaseDir").join(name), at);
            fixture
                .feed
                .emit_created(fixture.cloud_root.join("casedir").join(name), at);
        }
        for _ in 0..20 {
            fixture.tick(1_000);
        }
        fixture.converge(24);

        assert_eq!(names_in(&fixture.watch_root), vec!["casedir"]);
        assert_eq!(names_in(&fixture.cloud_root), vec!["casedir"]);
        assert_eq!(
            names_in(&fixture.watch_root.join("casedir")),
            vec!["a.txt", "b.txt"]
        );
        assert_eq!(fixture.runtime.conflict_count(), 0);
        assert_eq!(fixture.runtime.state_db().queue_depth().expect("depth"), 0);
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0
        );
    }

    /// Syncs `name` through one offline run and returns the fixture.
    pub(super) fn offline_fixture_with_synced(name: &str, body: &[u8]) -> OfflineFixture {
        let fixture = OfflineFixture::new();
        std::fs::write(fixture.watch_root.join(name), body).expect("seed");
        drop(fixture.run(&[]));
        assert_eq!(names_in(&fixture.cloud_root), vec![name.to_string()]);
        fixture
    }

    #[test]
    fn an_edit_made_while_an_ignore_rule_hid_the_file_uploads_when_the_rule_goes() {
        let fixture = offline_fixture_with_synced("a.secret", b"synced");
        let (local, cloud) = (
            fixture.watch_root.join("a.secret"),
            fixture.cloud_root.join("a.secret"),
        );
        // The rule hides the file, the file changes here, and a walk
        // passes while it is hidden.
        std::fs::write(fixture.watch_root.join(".vaporignore"), "*.secret\n").expect("rule");
        std::fs::write(&local, b"edited while hidden").expect("edit");
        drop(fixture.run(&[]));
        assert_eq!(std::fs::read(&cloud).expect("cloud"), b"synced");

        // The rule goes. The cloud copy never changed: the edit is the
        // one change, and it goes up under the name.
        std::fs::remove_file(fixture.watch_root.join(".vaporignore")).expect("rule gone");
        let runtime = fixture.run(&[]);
        assert_eq!(
            std::fs::read(&cloud).expect("cloud"),
            b"edited while hidden"
        );
        assert_eq!(
            std::fs::read(&local).expect("local"),
            b"edited while hidden"
        );
        for side in [&fixture.watch_root, &fixture.cloud_root] {
            assert!(
                names_in(side)
                    .iter()
                    .all(|name| !name.contains("~conflict-")),
                "{:?}",
                names_in(side)
            );
        }
        assert_eq!(runtime.conflict_count(), 0);
        assert_eq!(runtime.state_db().failed_depth().expect("failed"), 0);
    }

    #[test]
    fn a_file_removed_while_an_ignore_rule_hid_it_is_not_removed_from_the_cloud() {
        let fixture = offline_fixture_with_synced("a.secret", b"synced");
        let (local, cloud) = (
            fixture.watch_root.join("a.secret"),
            fixture.cloud_root.join("a.secret"),
        );
        // The rule hides the file and a walk passes. Then the file is
        // removed here: under the rule that frees space on this device,
        // it does not say "delete it everywhere".
        std::fs::write(fixture.watch_root.join(".vaporignore"), "*.secret\n").expect("rule");
        drop(fixture.run(&[]));
        std::fs::remove_file(&local).expect("remove under the rule");
        drop(fixture.run(&[]));
        assert_eq!(std::fs::read(&cloud).expect("cloud"), b"synced");

        // The rule goes. The cloud copy stays, and the file comes back.
        std::fs::remove_file(fixture.watch_root.join(".vaporignore")).expect("rule gone");
        let runtime = fixture.run(&[]);
        assert_eq!(std::fs::read(&cloud).expect("cloud"), b"synced");
        assert_eq!(std::fs::read(&local).expect("local"), b"synced");
        assert_eq!(runtime.state_db().failed_depth().expect("failed"), 0);

        // From here on the file is watched again: removed now, it is a
        // deletion like any other.
        std::fs::remove_file(&local).expect("remove in scope");
        drop(fixture.run(&[]));
        assert!(!cloud.exists(), "a removal in scope reaches the cloud");
    }

    #[test]
    fn a_spelling_changed_on_this_device_while_stopped_is_carried_to_the_cloud() {
        let fixture = offline_fixture_with_synced("report.txt", b"quarterly");
        std::fs::rename(
            fixture.watch_root.join("report.txt"),
            fixture.watch_root.join("Report.txt"),
        )
        .expect("rename");

        let runtime = fixture.run(&[]);
        assert_eq!(names_in(&fixture.watch_root), vec!["Report.txt"]);
        assert_eq!(names_in(&fixture.cloud_root), vec!["Report.txt"]);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("Report.txt")).expect("cloud"),
            b"quarterly"
        );
        assert!(runtime.trash().expect("trash").list().is_empty());
        assert_eq!(runtime.state_db().failed_depth().expect("failed"), 0);
    }

    #[test]
    fn a_spelling_changed_in_the_cloud_while_stopped_is_carried_to_this_device() {
        let fixture = offline_fixture_with_synced("notes.txt", b"minutes");
        std::fs::rename(
            fixture.cloud_root.join("notes.txt"),
            fixture.cloud_root.join("Notes.txt"),
        )
        .expect("cloud rename");

        let runtime = fixture.run(&[]);
        assert_eq!(names_in(&fixture.cloud_root), vec!["Notes.txt"]);
        assert_eq!(names_in(&fixture.watch_root), vec!["Notes.txt"]);
        assert_eq!(
            std::fs::read(fixture.watch_root.join("Notes.txt")).expect("local"),
            b"minutes"
        );
        assert!(runtime.trash().expect("trash").list().is_empty());
        assert_eq!(runtime.state_db().failed_depth().expect("failed"), 0);
    }

    #[test]
    fn when_both_sides_changed_the_spelling_the_spelling_of_this_device_stays() {
        let fixture = offline_fixture_with_synced("draft.txt", b"chapter one");
        if std::fs::symlink_metadata(fixture.watch_root.join("DRAFT.TXT")).is_err() {
            // A volume that keeps spellings apart sees two plain
            // renames to two names; there is no one name to settle.
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

        // No time decides: in two-way the name on this device has
        // priority, and the cloud object is moved to it.
        let runtime = fixture.run(&[]);
        assert_eq!(names_in(&fixture.cloud_root), vec!["DRAFT.txt"]);
        assert_eq!(names_in(&fixture.watch_root), vec!["DRAFT.txt"]);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("DRAFT.txt")).expect("cloud"),
            b"chapter one"
        );
        assert!(runtime.trash().expect("trash").list().is_empty());
        assert_eq!(runtime.conflict_count(), 0);
        assert_eq!(runtime.state_db().failed_depth().expect("failed"), 0);
        assert!(
            runtime
                .state_db()
                .sync_index(&fixture.watch_root.join("DRAFT.txt"))
                .expect("read")
                .is_some(),
            "the index row is under the spelling that stayed"
        );
    }

    #[test]
    fn a_folder_spelling_changed_in_the_cloud_while_stopped_renames_the_local_folder() {
        let fixture = OfflineFixture::new();
        std::fs::create_dir(fixture.watch_root.join("photos")).expect("dir");
        std::fs::write(fixture.watch_root.join("photos/a.jpg"), b"first").expect("seed");
        drop(fixture.run(&[]));
        std::fs::rename(
            fixture.cloud_root.join("photos"),
            fixture.cloud_root.join("Photos"),
        )
        .expect("cloud rename");

        let runtime = fixture.run(&[]);
        assert_eq!(names_in(&fixture.cloud_root), vec!["Photos"]);
        if crate::name_collision::folds_case_at(&fixture.watch_root) {
            // One folder under two spellings: the local one is renamed.
            assert_eq!(names_in(&fixture.watch_root), vec!["Photos"]);
        } else {
            // Two different names on this volume: the file moves to the
            // new folder, and the old folder holds no file.
            assert!(names_in(&fixture.watch_root).contains(&"Photos".to_string()));
            assert!(!fixture.watch_root.join("photos/a.jpg").exists());
        }
        assert_eq!(names_in(&fixture.watch_root.join("Photos")), vec!["a.jpg"]);
        assert_eq!(
            std::fs::read(fixture.watch_root.join("Photos/a.jpg")).expect("local"),
            b"first"
        );
        assert_eq!(runtime.state_db().failed_depth().expect("failed"), 0);
        if crate::name_collision::folds_case_at(&fixture.watch_root) {
            assert!(runtime.trash().expect("trash").list().is_empty());
        }
    }

    /// Seeds `count` synced files (uploaded, indexed).
    fn seed_synced_files(fixture: &mut BidirectionalFixture, count: usize) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for index in 0..count {
            let path = fixture
                .watch_root
                .join(format!("d{}/f{index}.txt", index % 5));
            std::fs::create_dir_all(path.parent().unwrap()).expect("dir");
            std::fs::write(&path, format!("payload {index}")).expect("seed");
            fixture.record_local_event(&path, FsEventKind::Created, fixture.now_ms);
            paths.push(path);
        }
        fixture.converge(30);
        assert_eq!(
            fixture
                .runtime
                .state_db()
                .sync_index_count()
                .expect("index"),
            count,
            "every seeded file must be indexed"
        );
        paths
    }

    #[test]
    fn a_cloud_deletion_applied_locally_lands_in_the_trash_and_restores() {
        let mut fixture = BidirectionalFixture::new();
        let trash_root = fixture._temp.path().join("trash/default");
        fixture.runtime.attach_trash(crate::trash::LocalTrash::new(
            "default",
            trash_root,
            crate::trash::TrashSettings::default(),
            Arc::new(vapor_platform::InMemoryTrashBin::unsupported()),
        ));
        fixture.tick(6_000);
        let local = fixture.watch_root.join("docs/keep.txt");
        std::fs::create_dir_all(local.parent().unwrap()).expect("dir");
        std::fs::write(&local, b"worth keeping").expect("seed");
        fixture.record_local_event(&local, FsEventKind::Created, fixture.now_ms);
        fixture.converge(20);
        let cloud = fixture.cloud_root.join("docs/keep.txt");
        assert!(cloud.exists());

        // Another device deletes it in the cloud.
        std::fs::remove_file(&cloud).expect("cloud unlink");
        fixture
            .feed
            .emit_removed(cloud.clone(), timestamp_ms(fixture.now_ms));
        fixture.converge(20);
        if local.exists() {
            let queue: Vec<_> = fixture
                .runtime
                .state_db()
                .list_queue_intents(16)
                .expect("queue")
                .into_iter()
                .map(|r| (r.kind, r.attempt_count, r.last_error, r.path))
                .collect();
            let failed = fixture.runtime.state_db().failed_depth().expect("failed");
            let decisions: Vec<_> = fixture
                .runtime
                .state_db()
                .decisions(true)
                .expect("decisions")
                .into_iter()
                .map(|d| (d.kind, d.question))
                .collect();
            panic!(
                "the deletion applies locally; queue={queue:?} failed={failed} decisions={decisions:?} local_root={:?} cloud_root={:?}",
                fixture.watch_root, fixture.cloud_root
            );
        }
        let entries = fixture.runtime.trash().expect("trash").list();
        assert_eq!(entries.len(), 1, "the removed file is kept in the trash");
        assert_eq!(entries[0].original_path, local);
        assert_eq!(entries[0].reason, constants::trash::REASON_CLOUD_DELETE);

        // The user brings it back; it syncs up like any other write.
        let landed = fixture
            .runtime
            .trash()
            .expect("trash")
            .restore(&entries[0].id)
            .expect("restore");
        assert_eq!(landed, local);
        assert_eq!(std::fs::read(&local).expect("restored"), b"worth keeping");
        fixture.record_local_event(&local, FsEventKind::Created, fixture.now_ms);
        fixture.converge(20);
        assert_eq!(
            std::fs::read(&cloud).expect("re-uploaded"),
            b"worth keeping"
        );
        assert!(fixture.runtime.trash().expect("trash").list().is_empty());
    }

    #[test]
    fn a_pull_only_mirror_removal_lands_in_the_trash() {
        let mut fixture = BidirectionalFixture::new_with_mode(vapor_shared::SyncMode::PullOnly);
        let trash_root = fixture._temp.path().join("trash/default");
        fixture.runtime.attach_trash(crate::trash::LocalTrash::new(
            "default",
            trash_root,
            crate::trash::TrashSettings::default(),
            Arc::new(vapor_platform::InMemoryTrashBin::unsupported()),
        ));
        let local_only = fixture.watch_root.join("only-here.txt");
        std::fs::write(&local_only, b"not in the cloud").expect("seed");
        fixture
            .runtime
            .enqueue_startup_reconstruction_reconcile(timestamp_ms(fixture.now_ms))
            .expect("reconcile");
        fixture.converge(30);
        assert!(
            !local_only.exists(),
            "pull-only removes the local-only file"
        );
        let entries = fixture.runtime.trash().expect("trash").list();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].reason, constants::trash::REASON_MIRROR_DELETE);
        assert_eq!(entries[0].original_path, local_only);
    }

    #[test]
    fn a_local_rename_becomes_a_server_side_move_instead_of_a_reupload() {
        let mut fixture = BidirectionalFixture::new();
        let timeline = crate::timeline::TimelineBuffer::new(64);
        fixture.runtime.attach_timeline(timeline.clone());
        fixture.tick(6_000);
        let before = fixture.watch_root.join("report-v1.bin");
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&before, &payload).expect("seed");
        fixture.record_local_event(&before, FsEventKind::Created, fixture.now_ms);
        fixture.converge(30);
        assert!(fixture.cloud_root.join("report-v1.bin").exists());

        // The user renames it. The watcher reports a delete and a create.
        let after = fixture.watch_root.join("archive/report-final.bin");
        std::fs::create_dir_all(after.parent().unwrap()).expect("dir");
        std::fs::rename(&before, &after).expect("rename");
        fixture.record_local_event(&before, FsEventKind::Removed, fixture.now_ms);
        fixture.record_local_event(&after, FsEventKind::Created, fixture.now_ms);
        let mut moves = 0;
        for _ in 0..30 {
            let report = fixture.tick(6_000);
            moves += report.moves;
            if report.staged_executor.active_total == 0
                && fixture.runtime.state_db().queue_depth().expect("depth") == 0
            {
                break;
            }
        }
        assert_eq!(moves, 1, "the rename is one server-side move");
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("archive/report-final.bin")).expect("moved"),
            payload
        );
        assert!(!fixture.cloud_root.join("report-v1.bin").exists());
        assert!(
            fixture
                .runtime
                .state_db()
                .sync_index(&after)
                .expect("index")
                .is_some(),
            "the moved file is indexed under its new path"
        );
        assert!(
            fixture
                .runtime
                .state_db()
                .sync_index(&before)
                .expect("index")
                .is_none()
        );
    }

    #[test]
    fn a_cloud_rename_becomes_a_local_rename_instead_of_a_download() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let local = fixture.watch_root.join("photo.raw");
        let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
        std::fs::write(&local, &payload).expect("seed");
        fixture.record_local_event(&local, FsEventKind::Created, fixture.now_ms);
        fixture.converge(30);
        let cloud_before = fixture.cloud_root.join("photo.raw");
        assert!(cloud_before.exists());

        // Another device renames it in the cloud; the feed reports a
        // removal and a creation.
        let cloud_after = fixture.cloud_root.join("2026/photo-renamed.raw");
        std::fs::create_dir_all(cloud_after.parent().unwrap()).expect("dir");
        std::fs::rename(&cloud_before, &cloud_after).expect("cloud rename");
        fixture
            .feed
            .emit_removed(cloud_before.clone(), timestamp_ms(fixture.now_ms));
        fixture
            .feed
            .emit_created(cloud_after.clone(), timestamp_ms(fixture.now_ms));
        let mut moves = 0;
        for _ in 0..30 {
            let report = fixture.tick(6_000);
            moves += report.moves;
            if report.staged_executor.active_total == 0
                && fixture.runtime.state_db().queue_depth().expect("depth") == 0
            {
                break;
            }
        }
        let local_after = fixture.watch_root.join("2026/photo-renamed.raw");
        assert_eq!(moves, 1, "the cloud rename is one local rename");
        assert_eq!(std::fs::read(&local_after).expect("renamed"), payload);
        assert!(
            !local.exists(),
            "the old name is gone, not trashed as a deletion"
        );
        assert!(
            fixture.runtime.trash().is_none()
                || fixture.runtime.trash().expect("trash").list().is_empty()
        );
        assert!(
            fixture
                .runtime
                .state_db()
                .sync_index(&local_after)
                .expect("index")
                .is_some()
        );
    }

    #[test]
    fn a_folder_renamed_in_the_cloud_leaves_no_empty_local_folder_behind() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let root = fixture.watch_root.clone();
        std::fs::create_dir_all(root.join("albums")).expect("albums");
        std::fs::create_dir_all(root.join("inbox")).expect("inbox");
        let payload =
            |seed: u32| -> Vec<u8> { (0..200_000u32).map(|i| ((i + seed) % 251) as u8).collect() };
        for (path, seed) in [
            ("albums/a.raw", 1),
            ("albums/b.raw", 2),
            ("inbox/note.raw", 3),
        ] {
            std::fs::write(root.join(path), payload(seed)).expect("seed");
            fixture.record_local_event(&root.join(path), FsEventKind::Created, fixture.now_ms);
        }
        fixture.converge(30);
        assert!(fixture.cloud_root.join("albums/b.raw").exists());

        // Another device renames one folder in the cloud, and moves the
        // only file out of the other folder, which it keeps.
        let cloud = fixture.cloud_root.clone();
        std::fs::rename(cloud.join("albums"), cloud.join("archive")).expect("folder rename");
        std::fs::rename(cloud.join("inbox/note.raw"), cloud.join("note.raw")).expect("file move");
        for (from, to) in [
            ("albums/a.raw", "archive/a.raw"),
            ("albums/b.raw", "archive/b.raw"),
            ("inbox/note.raw", "note.raw"),
        ] {
            fixture
                .feed
                .emit_removed(cloud.join(from), timestamp_ms(fixture.now_ms));
            fixture
                .feed
                .emit_created(cloud.join(to), timestamp_ms(fixture.now_ms));
        }
        let mut moves = 0;
        for _ in 0..40 {
            let report = fixture.tick(6_000);
            moves += report.moves;
            if report.staged_executor.active_total == 0
                && fixture.runtime.state_db().queue_depth().expect("depth") == 0
            {
                break;
            }
        }

        assert_eq!(moves, 3, "each file moved with no transfer");
        assert!(root.join("archive/a.raw").is_file());
        assert!(root.join("archive/b.raw").is_file());
        assert!(root.join("note.raw").is_file());
        assert!(
            !root.join("albums").exists(),
            "the cloud has no such folder any more, and the local one was empty"
        );
        assert!(
            root.join("inbox").is_dir(),
            "the cloud still has this folder, so the empty local one stays"
        );
    }

    #[test]
    fn a_start_with_a_cursor_that_expired_runs_one_whole_scope_scan_after_the_baseline() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        std::fs::write(fixture.watch_root.join("kept.txt"), b"kept").expect("seed");
        let kept = fixture.watch_root.join("kept.txt");
        fixture.record_local_event(&kept, FsEventKind::Created, fixture.now_ms);
        fixture.converge(20);

        // A restart: the feed does not know the stored cursor any more,
        // and the startup reconcile is queued.
        let now = timestamp_ms(fixture.now_ms);
        let cursor_key = format!(
            "{}{}",
            constants::provider::CHANGES_CURSOR_STATE_KEY_PREFIX,
            fixture.runtime.profile_id
        );
        fixture
            .runtime
            .state_db
            .set_state(&cursor_key, "999999", now)
            .expect("stale cursor");
        fixture.runtime.remote_poller = RemotePoller::new(&fixture.runtime.profile_id.clone());
        fixture
            .runtime
            .schedule_startup_reconcile(now)
            .expect("startup reconcile");

        let mut scans = 0;
        let mut expired_at_tick = None;
        let mut first_scan_started_at_tick = None;
        for tick in 0..30 {
            let report = fixture.tick(6_000);
            if report.remote_poll.cursor_expired {
                expired_at_tick = Some(tick);
            }
            if report.started_reconcile_root.is_some() && first_scan_started_at_tick.is_none() {
                first_scan_started_at_tick = Some(tick);
            }
            scans += usize::from(report.completed_reconcile_root.is_some());
        }

        let expired = expired_at_tick.expect("the stale cursor was reported");
        let started = first_scan_started_at_tick.expect("the scan ran");
        assert!(
            started > expired,
            "the scan started at tick {started}, before the baseline that follows the expiry at tick {expired}"
        );
        assert_eq!(scans, 1, "one scan repairs the gap of the feed");
        assert!(
            fixture
                .runtime
                .state_db
                .state(&cursor_key)
                .expect("state read")
                .is_some(),
            "the feed has a cursor again"
        );
    }

    #[test]
    fn dropped_watcher_events_schedule_a_whole_scope_reconcile() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        fixture.converge(20);
        // A file lands while the watcher's queue overflows: the only
        // report the engine gets is "something changed" on the root.
        std::fs::write(fixture.watch_root.join("missed.txt"), b"never reported").expect("seed");
        let root = fixture.watch_root.clone();
        fixture.record_local_event(&root, FsEventKind::Other, fixture.now_ms);
        let mut scheduled = false;
        for _ in 0..8 {
            fixture.tick(6_000);
            let queued = fixture
                .runtime
                .state_db()
                .list_queue_intents(16)
                .expect("queue")
                .into_iter()
                .any(|intent| {
                    intent.kind == PendingIntentKind::ReconcileSubtree && intent.path == root
                });
            let running =
                fixture.runtime.app().running_reconcile_root().as_deref() == Some(root.as_path());
            if queued || running {
                scheduled = true;
                break;
            }
        }
        assert!(
            scheduled,
            "a whole-scope reconcile is what answers dropped events"
        );
        fixture.converge(30);
        assert!(
            fixture.cloud_root.join("missed.txt").exists(),
            "the whole-scope reconcile finds what the watcher dropped"
        );
    }

    #[test]
    fn a_watcher_event_for_the_root_itself_checks_the_root_before_reconciling() {
        // FSEvents reports a root change as a rename-away and a mount
        // as a create; inotify reports a deleted or moved root. None
        // of it is a deletion to mirror, and a reconcile over a
        // replacement folder would read that folder as deletions, so
        // the root is checked first.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let kept = fixture.watch_root.join("kept.txt");
        std::fs::write(&kept, b"kept").expect("seed");
        fixture.record_local_event(&kept, FsEventKind::Created, fixture.now_ms);
        fixture.converge(12);
        assert!(fixture.cloud_root.join("kept.txt").is_file());

        // The adopted root is swapped for an empty folder, and the
        // watcher reports the root removed.
        let parked = fixture._temp.path().join("parked");
        std::fs::rename(&fixture.watch_root, &parked).expect("swap out");
        std::fs::create_dir_all(&fixture.watch_root).expect("empty replacement");
        let root = fixture.watch_root.clone();
        fixture.record_local_event(&root, FsEventKind::Removed, fixture.now_ms);
        for _ in 0..12 {
            fixture.tick(6_000);
        }
        assert!(
            fixture.cloud_root.join("kept.txt").is_file(),
            "the replacement folder is never mirrored as deletions"
        );
        assert!(
            fixture
                .runtime
                .state_db()
                .decisions(false)
                .expect("decisions")
                .iter()
                .any(|decision| decision.kind == crate::root_identity::DECISION_KIND),
            "the replaced root is put to the user"
        );
        assert_eq!(fixture.runtime.app().snapshot().run_state, RunState::Error);
        assert_eq!(
            fixture
                .runtime
                .state_db()
                .list_queue_intents(16)
                .expect("queue")
                .iter()
                .filter(|record| record.kind == PendingIntentKind::Delete)
                .count(),
            0,
            "no delete of the root or its files is queued"
        );
    }

    #[test]
    fn a_file_that_an_ignore_rule_change_frees_syncs_without_a_restart() {
        let mut fixture = BidirectionalFixture::new();
        let filter = fixture
            .runtime
            .path_filter
            .clone()
            .expect("the fixture has a path filter");
        // The daemon runs: its first rules are in.
        fixture.tick(6_000);
        assert!(filter.rules_ready());
        // Two files that no event announced; a rule hides one of them.
        std::fs::write(fixture.watch_root.join(".vaporignore"), "draft.txt\n").expect("rules");
        std::fs::write(fixture.watch_root.join("draft.txt"), b"not ready").expect("draft");
        std::fs::write(fixture.watch_root.join("plain.txt"), b"plain").expect("plain");
        // The watcher saw the rules file change.
        filter.request_reload();
        fixture.converge(24);
        assert!(
            fixture.cloud_root.join("plain.txt").is_file(),
            "the new rules decide another way, so a whole-scope reconcile ran"
        );
        assert!(!fixture.cloud_root.join("draft.txt").exists(), "hidden");

        // The rule goes. Nothing touches the draft, and no restart and
        // no `vapor reconcile` follows.
        std::fs::write(fixture.watch_root.join(".vaporignore"), "").expect("rules");
        filter.request_reload();
        fixture.converge(24);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("draft.txt")).expect("the freed file synced"),
            b"not ready"
        );

        // A reload that changes nothing queues no reconcile.
        let generation = filter.rules_generation();
        filter.request_reload();
        fixture.tick(6_000);
        assert_eq!(filter.rules_generation(), generation);
    }

    #[test]
    fn no_event_becomes_an_intent_while_the_ignore_rules_are_read_again() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // the first rules are in
        let filter = fixture
            .runtime
            .path_filter
            .clone()
            .expect("the fixture has a path filter");
        // An ignore file appears next to the folder it hides, and the
        // watcher reports the files one by one, under the old rules.
        let root = fixture.watch_root.clone();
        std::fs::create_dir_all(root.join("build")).expect("build dir");
        std::fs::create_dir_all(root.join("src")).expect("src dir");
        std::fs::write(root.join(".gitignore"), "build/\n").expect("rules");
        std::fs::write(root.join("build/out.bin"), b"artifact").expect("artifact");
        std::fs::write(root.join("src/main.rs"), b"fn main() {}").expect("source");
        filter.request_reload();
        for path in ["build/out.bin", "src/main.rs"] {
            fixture.record_local_event(&root.join(path), FsEventKind::Created, fixture.now_ms);
        }

        // The rebuild has not run. However long the events have been
        // quiet, none is released on the word of the old rules.
        let long_after = timestamp_ms(fixture.now_ms + 60_000);
        let (released, _, _) = fixture.runtime.stabilize_events(long_after);
        assert_eq!(released, 0);

        // The rules land, and they decide for the events that waited.
        fixture.converge(24);
        assert!(fixture.cloud_root.join("src/main.rs").is_file());
        assert!(
            !fixture.cloud_root.join("build/out.bin").exists(),
            "a file the new rules hide never reached the cloud"
        );
    }

    #[test]
    fn nothing_is_polled_or_leased_before_the_first_ignore_rules_are_in() {
        let mut fixture = BidirectionalFixture::new();
        // As in the daemon: the walk for the ignore files runs on a
        // worker, and lands on a later tick.
        fixture.runtime.provider_call_mode =
            crate::provider_jobs::ProviderCallMode::Threaded { waker: None };
        let filter = fixture
            .runtime
            .path_filter
            .clone()
            .expect("the fixture has a path filter");
        let root = fixture.watch_root.clone();
        std::fs::write(root.join(".gitignore"), "secret.txt\n").expect("rules");
        std::fs::write(root.join("secret.txt"), b"hidden").expect("hidden file");
        std::fs::write(root.join("plain.txt"), b"plain").expect("plain file");
        for path in ["secret.txt", "plain.txt"] {
            fixture.record_local_event(&root.join(path), FsEventKind::Created, fixture.now_ms);
        }

        // Bounded by iteration count: the loop ends as soon as the
        // worker delivers the rules.
        for _ in 0..1_000_000 {
            let report = fixture.tick(6_000);
            if filter.rules_ready() {
                break;
            }
            assert_eq!(report.leased_intents, 0, "{report:?}");
            assert_eq!(report.durable_enqueues, 0, "{report:?}");
            assert!(!report.remote_poll.polled, "{report:?}");
            std::thread::yield_now();
        }
        assert!(filter.rules_ready());

        for _ in 0..1_000_000 {
            fixture.tick(6_000);
            if fixture.cloud_root.join("plain.txt").is_file() {
                break;
            }
            std::thread::yield_now();
        }
        assert!(fixture.cloud_root.join("plain.txt").is_file());
        assert!(!fixture.cloud_root.join("secret.txt").exists());
    }

    #[test]
    fn a_full_cloud_makes_uploads_wait_and_they_go_up_when_there_is_room() {
        let (mut fixture, cloud_full) = BidirectionalFixture::new_with_cloud_full_switch();
        fixture.tick(6_000);
        cloud_full.store(true, std::sync::atomic::Ordering::SeqCst);

        let local = fixture.watch_root.join("report.txt");
        std::fs::write(&local, b"waits for room").expect("local file");
        fixture.record_local_event(&local, FsEventKind::Created, fixture.now_ms);
        fixture.converge(4);
        assert!(!fixture.cloud_root.join("report.txt").exists());
        let snapshot = fixture.runtime.app().snapshot();
        assert_eq!(snapshot.run_state, RunState::Error, "{snapshot:?}");
        assert!(snapshot.reason.contains("uploads wait"), "{snapshot:?}");
        // The upload is queued, not failed, and no attempt was spent.
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0
        );
        let queued = fixture
            .runtime
            .state_db()
            .list_queue_intents(4)
            .expect("queue");
        assert_eq!(queued.len(), 1, "{queued:?}");
        assert_eq!(queued[0].attempt_count, 0);

        // A download does not wait for room in the cloud.
        std::fs::write(fixture.cloud_root.join("inbound.txt"), b"inbound").expect("cloud file");
        fixture.feed.emit_created(
            fixture.cloud_root.join("inbound.txt"),
            timestamp_ms(fixture.now_ms),
        );
        fixture.converge(4);
        assert!(fixture.watch_root.join("inbound.txt").is_file());

        // Someone frees space. The upload comes back on its own.
        cloud_full.store(false, std::sync::atomic::Ordering::SeqCst);
        fixture.tick(constants::engine::CLOUD_FULL_RETRY_SECONDS * 1_000 + 1_000);
        fixture.converge(4);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("report.txt")).expect("uploaded"),
            b"waits for room"
        );
        fixture.tick(120_000);
        let snapshot = fixture.runtime.app().snapshot();
        assert_eq!(snapshot.run_state, RunState::Running, "{snapshot:?}");
    }

    #[test]
    fn a_request_for_sync_now_tries_the_uploads_that_wait_for_room_at_once() {
        let (mut fixture, cloud_full) = BidirectionalFixture::new_with_cloud_full_switch();
        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        fixture.runtime.attach_control(control.clone());
        fixture.tick(6_000);
        cloud_full.store(true, std::sync::atomic::Ordering::SeqCst);
        let local = fixture.watch_root.join("report.txt");
        std::fs::write(&local, b"waits for room").expect("local file");
        fixture.record_local_event(&local, FsEventKind::Created, fixture.now_ms);
        fixture.converge(4);
        assert!(!fixture.cloud_root.join("report.txt").exists());
        assert_eq!(fixture.runtime.app().snapshot().run_state, RunState::Error);

        // The user frees space and asks for sync now, long before the
        // 15 minutes of the automatic retry are over.
        cloud_full.store(false, std::sync::atomic::Ordering::SeqCst);
        control.request_reconcile();
        fixture.converge(8);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("report.txt")).expect("uploaded"),
            b"waits for room"
        );
        let snapshot = fixture.runtime.app().snapshot();
        assert_eq!(snapshot.run_state, RunState::Running, "{snapshot:?}");

        // A request while the cloud is still full changes nothing for
        // good: the upload waits again, and the state says so again.
        cloud_full.store(true, std::sync::atomic::Ordering::SeqCst);
        let second = fixture.watch_root.join("second.txt");
        std::fs::write(&second, b"still no room").expect("local file");
        fixture.record_local_event(&second, FsEventKind::Created, fixture.now_ms);
        fixture.converge(4);
        control.request_reconcile();
        fixture.converge(8);
        assert!(!fixture.cloud_root.join("second.txt").exists());
        let snapshot = fixture.runtime.app().snapshot();
        assert_eq!(snapshot.run_state, RunState::Error, "{snapshot:?}");
        assert!(snapshot.reason.contains("uploads wait"), "{snapshot:?}");
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0
        );
    }

    #[test]
    fn a_request_for_sync_now_finds_the_waiting_uploads_after_a_restart() {
        let (mut fixture, cloud_full) = BidirectionalFixture::new_with_cloud_full_switch();
        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        fixture.runtime.attach_control(control.clone());
        fixture.tick(6_000);
        cloud_full.store(true, std::sync::atomic::Ordering::SeqCst);
        let local = fixture.watch_root.join("report.txt");
        std::fs::write(&local, b"waits for room").expect("local file");
        fixture.record_local_event(&local, FsEventKind::Created, fixture.now_ms);
        fixture.converge(4);
        assert!(!fixture.cloud_root.join("report.txt").exists());

        // A restart keeps the queue and nothing of the memory. The
        // daemon that starts reads from the queue that uploads wait.
        fixture.runtime.cloud_full = None;
        fixture
            .runtime
            .recall_uploads_that_wait_for_room()
            .expect("recall");
        let recalled = fixture.runtime.cloud_full.as_ref().expect("recalled");
        assert!(
            recalled.0.contains("the cloud storage is full"),
            "{recalled:?}"
        );
        assert!(
            !recalled
                .0
                .contains(constants::engine::CLOUD_FULL_WAIT_PREFIX.trim_end()),
            "the state repeats the provider's message, not the mark of the queue row"
        );

        // Even with no memory of the full cloud, the request finds the
        // waiting upload in the queue.
        fixture.runtime.cloud_full = None;
        cloud_full.store(false, std::sync::atomic::Ordering::SeqCst);
        control.request_reconcile();
        fixture.converge(8);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("report.txt")).expect("uploaded"),
            b"waits for room"
        );
        assert!(fixture.runtime.cloud_full.is_none());
    }

    #[test]
    fn a_hold_that_comes_and_goes_does_not_end_the_users_pause() {
        let mut fixture = BidirectionalFixture::new();
        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        fixture.runtime.attach_control(control.clone());
        fixture.tick(6_000);
        control.request_pause();
        fixture.tick(250);
        assert_eq!(fixture.runtime.app().snapshot().run_state, RunState::Paused);

        // A hold sits over the pause for a while (a cloud root that
        // went away), and then it is gone.
        fixture
            .runtime
            .app
            .set_run_state(RunState::Error, "the cloud folder is missing");
        fixture
            .runtime
            .restore_run_state_after_root_recovery("the cloud folder is back");

        let snapshot = fixture.runtime.app().snapshot();
        assert_eq!(snapshot.run_state, RunState::Paused, "{snapshot:?}");

        control.request_resume();
        fixture.tick(250);
        assert_eq!(
            fixture.runtime.app().snapshot().run_state,
            RunState::Running
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_name_with_a_backslash_is_asked_about_one_time_and_never_fails() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let file = fixture.watch_root.join("report\\2025.txt");
        let folder = fixture.watch_root.join("old\\archive");
        std::fs::write(&file, b"a backslash in the name").expect("file");
        std::fs::create_dir_all(&folder).expect("folder");
        std::fs::write(folder.join("inside.txt"), b"inside").expect("inside");
        std::fs::write(fixture.watch_root.join("plain.txt"), b"plain").expect("plain");

        // The watcher reports the file, and a whole-scope scan follows.
        fixture.record_local_event(&file, FsEventKind::Created, fixture.now_ms);
        fixture
            .runtime
            .enqueue_startup_reconstruction_reconcile(timestamp_ms(fixture.now_ms))
            .expect("queue a whole-scope scan");
        fixture.converge(24);

        assert!(fixture.cloud_root.join("plain.txt").is_file());
        assert_eq!(
            std::fs::read_dir(&fixture.cloud_root)
                .expect("cloud")
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().contains('\\'))
                .count(),
            0,
            "no name with a backslash reaches the cloud"
        );
        assert_eq!(
            fixture.runtime.state_db().failed_depth().expect("failed"),
            0,
            "a name that cannot sync is not a failed intent"
        );
        let mut asked: Vec<PathBuf> = fixture
            .runtime
            .state_db()
            .decisions(true)
            .expect("decisions")
            .into_iter()
            .filter(|decision| decision.kind == crate::unsyncable::DECISION_KIND)
            .filter_map(|decision| decision.path)
            .collect();
        asked.sort();
        let mut expected = vec![file.clone(), folder.clone()];
        expected.sort();
        assert_eq!(asked, expected, "the file and the folder, each one time");

        // Renamed: the next scan withdraws the question and the file syncs.
        std::fs::rename(&file, fixture.watch_root.join("report-2025.txt")).expect("rename");
        fixture
            .runtime
            .enqueue_startup_reconstruction_reconcile(timestamp_ms(fixture.now_ms))
            .expect("queue a whole-scope scan");
        fixture.converge(24);
        assert!(fixture.cloud_root.join("report-2025.txt").is_file());
        let open: Vec<PathBuf> = fixture
            .runtime
            .state_db()
            .decisions(false)
            .expect("decisions")
            .into_iter()
            .filter(|decision| decision.kind == crate::unsyncable::DECISION_KIND)
            .filter_map(|decision| decision.path)
            .collect();
        assert_eq!(open, vec![folder]);
    }

    #[test]
    fn a_whole_scope_scan_counts_what_the_ignore_rules_hide() {
        let mut fixture = BidirectionalFixture::new();
        assert_eq!(
            fixture.runtime.ignored_paths(),
            None,
            "no scan has finished"
        );
        // `build/` and `*.log` are default rules. A folder counts one
        // time, whatever it holds.
        for file in [
            "build/a.o",
            "build/deep/b.o",
            "notes/today.log",
            "notes/plan.txt",
        ] {
            let path = fixture.watch_root.join(file);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            std::fs::write(path, b"x").expect("seed");
        }
        fixture
            .runtime
            .enqueue_startup_reconstruction_reconcile(timestamp_ms(fixture.now_ms))
            .expect("queue a whole-scope scan");
        fixture.converge(24);
        assert!(fixture.cloud_root.join("notes/plan.txt").is_file());
        assert_eq!(fixture.runtime.ignored_paths(), Some(2));
    }

    #[test]
    fn a_local_root_that_comes_back_asks_for_new_watchers_one_time() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        assert!(!fixture.runtime.take_local_root_changed());

        // The root goes away (a volume unplugged) and comes back as the
        // same folder under the same path.
        let parked = fixture._temp.path().join("parked");
        std::fs::rename(&fixture.watch_root, &parked).expect("unplug");
        for _ in 0..12 {
            fixture.tick(6_000);
        }
        assert_eq!(fixture.runtime.app().snapshot().run_state, RunState::Error);
        assert!(!fixture.runtime.take_local_root_changed(), "still away");

        std::fs::rename(&parked, &fixture.watch_root).expect("plug back in");
        for _ in 0..12 {
            fixture.tick(6_000);
        }
        assert_eq!(
            fixture.runtime.app().snapshot().run_state,
            RunState::Running
        );
        assert!(
            fixture.runtime.take_local_root_changed(),
            "a watch that follows the old folder reports nothing from this one"
        );
        assert!(!fixture.runtime.take_local_root_changed(), "asked one time");
    }

    #[test]
    fn a_file_gone_from_a_swapped_root_is_never_deleted_from_the_cloud() {
        // The watcher can report the files of a root that was swapped
        // before it reports the root, or without reporting the root at
        // all. The periodic root check is seconds away. Nothing may be
        // deleted in that window.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let kept = fixture.watch_root.join("kept.txt");
        std::fs::write(&kept, b"kept").expect("seed");
        fixture.record_local_event(&kept, FsEventKind::Created, fixture.now_ms);
        fixture.converge(12);
        assert!(fixture.cloud_root.join("kept.txt").is_file());

        let parked = fixture._temp.path().join("parked");
        std::fs::rename(&fixture.watch_root, &parked).expect("swap out");
        std::fs::create_dir_all(&fixture.watch_root).expect("empty replacement");
        // Only the file's own event arrives.
        fixture.record_local_event(&kept, FsEventKind::Removed, fixture.now_ms);
        for _ in 0..12 {
            fixture.tick(1_000);
        }
        assert!(
            fixture.cloud_root.join("kept.txt").is_file(),
            "a file that is gone because the root was swapped was deleted from the cloud"
        );
        assert!(
            fixture
                .runtime
                .state_db()
                .decisions(false)
                .expect("decisions")
                .iter()
                .any(|decision| decision.kind == crate::root_identity::DECISION_KIND),
            "the replaced root is put to the user"
        );
        assert_eq!(fixture.runtime.app().snapshot().run_state, RunState::Error);
    }

    #[test]
    fn a_repeated_create_event_for_a_synced_file_never_rewrites_the_cloud_copy() {
        // FSEvents can report a directory's creation after the file
        // events inside it; the runtime then reports the directory's
        // children again. A file that is already synced and unchanged
        // must not be uploaded a second time: a rewrite races any
        // cloud-side change of the same moment, and the changes feed
        // reads the rewrite's own events as echoes.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        let local = fixture.watch_root.join("docs/keep.txt");
        std::fs::create_dir_all(local.parent().unwrap()).expect("dir");
        std::fs::write(&local, b"worth keeping").expect("seed");
        fixture.record_local_event(&local, FsEventKind::Created, fixture.now_ms);
        fixture.converge(20);
        let cloud = fixture.cloud_root.join("docs/keep.txt");
        let before = std::fs::metadata(&cloud).expect("cloud copy");

        // The directory is reported again, and with it the file.
        let docs = fixture.watch_root.join("docs");
        fixture.record_local_event(&docs, FsEventKind::Created, fixture.now_ms);
        let mut completed = 0;
        for _ in 0..8 {
            completed += fixture.tick(6_000).completed_intents;
        }
        assert!(completed >= 1, "the repeated event is worked, as a no-op");
        let after = std::fs::metadata(&cloud).expect("cloud copy");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(after.ino(), before.ino(), "the cloud copy was rewritten");
        }
        assert_eq!(
            after.modified().expect("mtime"),
            before.modified().expect("mtime"),
            "the cloud copy was rewritten"
        );
    }

    #[test]
    fn a_repeated_create_event_for_a_file_the_cloud_just_deleted_applies_the_deletion() {
        // The same late directory report, but the cloud copy was
        // deleted a moment earlier and the feed has not said so yet.
        // The local copy is exactly what was last synced, so this is
        // the cloud's deletion to apply here, not a modification to
        // re-upload; a re-upload would undo the deletion and the feed
        // would read its own events as echoes.
        let mut fixture = BidirectionalFixture::new();
        let trash_root = fixture._temp.path().join("trash/default");
        fixture.runtime.attach_trash(crate::trash::LocalTrash::new(
            "default",
            trash_root,
            crate::trash::TrashSettings::default(),
            Arc::new(vapor_platform::InMemoryTrashBin::unsupported()),
        ));
        fixture.tick(6_000);
        let local = fixture.watch_root.join("docs/keep.txt");
        std::fs::create_dir_all(local.parent().unwrap()).expect("dir");
        std::fs::write(&local, b"worth keeping").expect("seed");
        fixture.record_local_event(&local, FsEventKind::Created, fixture.now_ms);
        fixture.converge(20);
        let cloud = fixture.cloud_root.join("docs/keep.txt");
        assert!(cloud.is_file());

        std::fs::remove_file(&cloud).expect("cloud delete");
        let docs = fixture.watch_root.join("docs");
        fixture.record_local_event(&docs, FsEventKind::Created, fixture.now_ms);
        for _ in 0..12 {
            fixture.tick(6_000);
        }
        assert!(!cloud.exists(), "the deletion is not undone by a re-upload");
        assert!(!local.exists(), "the deletion applies here");
        assert_eq!(
            fixture.runtime.trash().expect("trash").list().len(),
            1,
            "the removed copy is in the trash"
        );
    }

    #[test]
    fn cloud_root_recovery_does_not_override_an_active_pause() {
        let mut fixture = BidirectionalFixture::new();
        // Boot state: cloud root was unavailable and the user's pause
        // is active.
        fixture.runtime.cloud_root_ready = false;
        fixture
            .runtime
            .app
            .set_run_state(RunState::Paused, "user paused via vapor pause");

        // The cloud root becomes reachable and the retry recovers it.
        fixture.tick(70_000);

        assert!(
            fixture.runtime.cloud_root_ready,
            "the retry must recover the cloud root"
        );
        // The key guarantee: recovery does not flip Paused -> Running.
        // (The human-facing `reason` string is shared with throttle-state
        // updates and is refreshed every tick, so it is not asserted here.)
        assert_eq!(
            fixture.runtime.app.snapshot().run_state,
            RunState::Paused,
            "recovery must not silently clear an active pause"
        );
    }

    #[test]
    fn resume_while_cloud_root_unavailable_reports_error_not_running() {
        let mut fixture = BidirectionalFixture::new();
        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        fixture.runtime.attach_control(control.clone());
        // Cloud root unavailable → build's Error state, then a user pause.
        fixture.runtime.cloud_root_ready = false;
        fixture.runtime.app.set_run_state(
            RunState::Error,
            "cloud root /nope is unavailable; sync work is blocked until it can be ensured",
        );

        // Resume while still unavailable: applied in isolation (retry would
        // otherwise recover the fixture's real cloud dir) it must re-derive
        // Error, not report Running.
        control.request_resume();
        fixture
            .runtime
            .apply_pending_control_requests(timestamp_ms(1_000))
            .expect("apply control");

        assert_eq!(
            fixture.runtime.app.snapshot().run_state,
            RunState::Error,
            "resume must not report Running while the cloud root is unavailable"
        );
        assert!(
            fixture.runtime.app.snapshot().reason.contains("cloud root"),
            "the real blocker must remain visible in the reason"
        );
    }

    #[test]
    fn reconcile_request_boosts_remote_poll_cadence_and_lands_on_timeline() {
        let mut fixture = BidirectionalFixture::new();
        let timeline = crate::timeline::TimelineBuffer::new(256);
        fixture.runtime.attach_timeline(timeline.clone());
        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        fixture.runtime.attach_control(control.clone());

        let first = fixture.tick(6_000); // baseline poll
        assert!(first.remote_poll.polled);
        // A quarter of a second later: the watched feed is read each
        // second, so it is not due.
        let quiet = fixture.tick(250);
        assert!(!quiet.remote_poll.polled);

        control.request_reconcile();
        let boosted = fixture.tick(250);
        assert!(
            boosted.remote_poll.polled,
            "the reconcile boost must clear the remote poll cadence"
        );
        assert!(
            timeline
                .snapshot(None)
                .iter()
                .any(|entry| entry.kind == constants::timeline::KIND_RECONCILE),
            "a reconcile request must land on the activity timeline"
        );
    }
}
