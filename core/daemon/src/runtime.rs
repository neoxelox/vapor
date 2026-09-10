use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use vapor_platform::fs_caps::NativeFilesystemCapabilities;
use vapor_platform::fs_watch::native_watcher_available;
use vapor_providers::Provider;
use vapor_providers::filesystem::hash_hex_of_file_with;
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

/// The single implicit profile id used until the multi-profile model
/// fans the runtime out per profile.
pub const DEFAULT_PROFILE_ID: &str = "default";

/// Consecutive tick failures tolerated before the runtime loop gives up.
/// One inconsistent row or transient I/O error is logged and survived;
/// a structurally broken database fails fast after this many attempts.
const MAX_CONSECUTIVE_TICK_ERRORS: u32 = 5;

#[derive(Debug)]
pub enum DaemonRuntimeError {
    Watcher(FsEventsWatcherError),
    StateDb(StateDbError),
    /// The local root the profile adopted is not there. Waited for by
    /// the caller; never re-created.
    LocalRootMissing(PathBuf),
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
    match recorded {
        None => match provider.ensure_cloud_sync_directory(cloud_root) {
            Ok(()) => match provider.adopt_root(cloud_root, device_id) {
                Ok(identity) => {
                    logging::info(
                        "Adopted the cloud sync directory",
                        &[
                            ("cloud_sync_directory", cloud_root.to_string()),
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
                Err(error) => CloudRootOutcome::Unreachable(error.message),
            },
            Err(error) => CloudRootOutcome::Unreachable(error.message),
        },
        Some(recorded) => match classify_cloud_probe(&recorded, provider.root_identity(cloud_root))
        {
            RootStatus::Ready => match provider.ensure_cloud_sync_directory(cloud_root) {
                Ok(()) => CloudRootOutcome::Ready { adopted: None },
                Err(error) => CloudRootOutcome::Unreachable(error.message),
            },
            RootStatus::Missing => CloudRootOutcome::Missing,
            RootStatus::Unreachable(message) => CloudRootOutcome::Unreachable(message),
            RootStatus::Replaced { found, .. } => CloudRootOutcome::Replaced { found },
        },
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
/// poll interval — which in turn lets a fully idle daemon sleep at the
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
    /// Renames carried out as moves (server-side or local) instead of
    /// transfers.
    pub moves: usize,
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
    /// the remote-change mapping so ignore rules apply symmetrically —
    /// an ignored name never syncs in either direction.
    path_filter: Option<Arc<crate::fs_events::SharedEventPathFilter>>,
    metrics_sampler: Arc<dyn MetricsSampler>,
    clock: SharedClock,
    tick_waker: Arc<TickWaker>,
    runtime_control: Option<Arc<crate::runtime_control::RuntimeControl>>,
    status_publisher: Option<Arc<dyn StatusPublisher>>,
    last_throttle_sample_inst: Option<Instant>,
    last_stale_lease_sweep_inst: Option<Instant>,
    running_reconcile_intent_id: Option<i64>,
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
    last_root_check_inst: Option<Instant>,
    /// A watcher signal asked for a whole-scope reconcile; it is
    /// queued once the next root check finds the adopted root in
    /// place.
    reconcile_after_root_check: bool,
    /// Whether the provider-side sync root has been ensured. While
    /// `false`, no work is leased and the remote feed is not polled;
    /// ingest keeps capturing intent state durably.
    cloud_root_ready: bool,
    last_cloud_root_attempt_inst: Option<Instant>,
    /// Incremental comparison walk of the currently-running reconcile.
    /// Survives slice pauses so a large tree converges across slices
    /// instead of restarting from scratch (strict mirror).
    reconcile_walker: Option<crate::reconcile_walk::ReconcileWalker>,
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
    /// Daemon-wide bandwidth shaper; shared across profiles.
    bandwidth_shaper: Arc<Mutex<vapor_providers::BandwidthShaper>>,
    /// User-idle signal for the idle-boost gates. The native HID bridge
    /// is a platform follow-up; headless semantics (always idle) apply
    /// until it lands.
    idle_notifier: Arc<dyn vapor_platform::IdleNotifier>,
    /// Auto-tuned per-tick transfer step budget, shared across
    /// profiles.
    transfer_step_bytes: Arc<std::sync::atomic::AtomicU64>,
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
    /// Heuristic active-coding signal; ORs `user_active` into
    /// the throttle inputs when code-class files churn rapidly.
    active_coding: crate::safeguards::ActiveCodingHeuristic,
    /// Mass-deletion guard; holds a deletion burst in either direction
    /// behind a decision.
    mass_change_guard: crate::safeguards::MassChangeGuard,
    /// Resolved `safeguards` config: threshold, window, and ratio
    /// feeding the guard; `enabled = false` bypasses it.
    mass_delete_settings: crate::safeguards::MassDeleteGuardSettings,
    /// Where a file Vapor removes on this device goes. `None` until
    /// the profile runtime attaches one; the bare fixtures unlink.
    trash: Option<crate::trash::LocalTrash>,
    last_trash_purge_inst: Option<Instant>,
    /// Flush boost deadline (monotonic). While set and in the
    /// future, deferred reconciles release immediately regardless of
    /// the idle gate.
    flush_boost_until_inst: Option<Instant>,
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
            EventPathFilterOptions::from_process_environment(),
            state_db,
            provider,
            metrics_sampler,
            clock,
        )
    }

    /// Fully-parameterized start used by the daemon bootstrap: explicit
    /// filter options (resolved from `vapor.json` + environment) instead
    /// of environment-only defaults.
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
        {
            return true;
        }

        self.recorder
            .as_ref()
            .map(|recorder| {
                recorder.with_state(|maps| {
                    maps.pending_event_count() > 0 || maps.pending_intent_count() > 0
                })
            })
            .unwrap_or(false)
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
    /// publisher was constructed with — i.e. it would lie about
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
        self.apply_pending_control_requests(now)?;
        self.sample_throttle_inputs(now, throttle_inputs);
        self.reload_path_filter_if_requested();
        self.sweep_stale_leases_if_due(now)?;
        self.purge_trash_if_due(now);
        self.retry_cloud_root_if_needed()?;
        self.check_roots_if_due(now)?;
        self.apply_resolved_decisions(now)?;
        self.withdraw_holds_left_empty(now)?;

        let (stabilized_events, suppressed_local_echoes, stabilize_mirror_reverts) =
            self.stabilize_events(now);

        // Pause semantics ("stops admitting new work"): ingest, debounce,
        // and the durable flush keep running so intent state is never
        // lost, and work already in flight runs to completion — but no
        // new work is released or leased while paused. An unavailable
        // cloud root blocks the same way. Evaluated *after*
        // stabilization so a mass-deletion guard trip stops
        // admission in the same tick that detected the storm.
        let paused = self.app.snapshot().run_state == RunState::Paused
            || !self.cloud_root_ready
            || self.root_hold.is_some();
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
                &mut self.remote_echoes,
                self.sync_scope.local_sync_directory.as_deref(),
                self.path_filter.as_deref(),
                self.sync_scope.sync_mode,
                &self.clock,
                now,
                &self.provider_call_mode,
            )?;
            report.mirror_reverts += report.remote_poll.mirror_reverts;
            report.mirror_deletes += report.remote_poll.mirror_deletes;
            if report.remote_poll.name_collisions > 0
                && let Some(timeline) = &self.timeline
            {
                for (wanted, existing) in self.remote_poller.take_name_collisions() {
                    timeline.push(
                        "collision",
                        self.profile_id.clone(),
                        name_collision_message(&wanted, &existing),
                        now,
                    );
                }
            }
            self.mirror_revert_count += report.remote_poll.mirror_reverts as u64;
            self.mirror_delete_count += report.remote_poll.mirror_deletes as u64;
        }

        let staged_report = {
            let mut env = ExecutionEnv {
                local_root: self.sync_scope.local_sync_directory.as_deref(),
                sync_mode: self.sync_scope.sync_mode,
                device_id: &self.device_id,
                hash_algorithm: self.app.provider().content_hash_algorithm(),
                transfer_step_bytes: &self.transfer_step_bytes,
                bandwidth: &self.bandwidth_shaper,
                tags: &self.tags,
                local_echoes: &mut self.local_echoes,
                remote_echoes: &mut self.remote_echoes,
                deletion_guard: self
                    .mass_delete_settings
                    .enabled
                    .then_some(&mut self.mass_change_guard),
                trash: self.trash.as_ref(),
            };
            self.staged_executor
                .advance(&mut self.app, &mut self.state_db, &mut env, now)?
        };
        self.announce_decisions(&staged_report.decisions_opened, now);
        report.completed_intents += staged_report.completed;
        report.requeued_intents += staged_report.retried;
        report.failed_intents += staged_report.failed;
        report.mirror_deletes += staged_report.mirror_deletes;
        self.mirror_delete_count += staged_report.mirror_deletes as u64;
        report.conflicts += staged_report.conflicts;
        self.conflict_count += staged_report.conflicts as u64;
        report.moves += staged_report.moves;
        if staged_report.cloud_root_unavailable > 0 {
            self.mark_cloud_root_unavailable("a provider transfer reported the root missing", now);
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
                        for path in mismatches {
                            self.open_type_mismatch_decision(&path, now)?;
                        }
                        self.reconcile_unsyncable_names(&unsyncable, whole_scope, now)?;
                        if let Some(timeline) = &self.timeline {
                            for (wanted, existing) in collisions {
                                timeline.push(
                                    "collision",
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
                            self.reconcile_walker = None;
                            self.state_db.complete_leased(reconcile_intent_id)?;
                            self.running_reconcile_intent_id = None;
                            if self.sync_scope.local_sync_directory.as_ref()
                                == Some(&completed_root)
                            {
                                // The merge flag covers one whole-scope walk.
                                self.state_db
                                    .delete_state(constants::state::MERGE_WITHOUT_DELETIONS_KEY)?;
                                if self.startup_reconstruction_barrier {
                                    self.startup_reconstruction_barrier = false;
                                    self.startup_barrier_expires_inst = None;
                                }
                            }
                            report.completed_intents += 1;
                            report.completed_reconcile_root = Some(completed_root);
                        } else {
                            // The controller lost the running reconcile —
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
                        self.requeue_runtime_intent(
                            reconcile_intent_id,
                            now + self.tick_interval,
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
                    local_root: self.sync_scope.local_sync_directory.as_deref(),
                    sync_mode: self.sync_scope.sync_mode,
                    device_id: &self.device_id,
                    hash_algorithm: self.app.provider().content_hash_algorithm(),
                    transfer_step_bytes: &self.transfer_step_bytes,
                    bandwidth: &self.bandwidth_shaper,
                    tags: &self.tags,
                    local_echoes: &mut self.local_echoes,
                    remote_echoes: &mut self.remote_echoes,
                    deletion_guard: self
                        .mass_delete_settings
                        .enabled
                        .then_some(&mut self.mass_change_guard),
                    trash: self.trash.as_ref(),
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
                report.completed_intents += admission_report.completed;
                report.requeued_intents += admission_report.retried;
                report.failed_intents += admission_report.failed;
                report.mirror_deletes += admission_report.mirror_deletes;
                self.mirror_delete_count += admission_report.mirror_deletes as u64;
                report.conflicts += admission_report.conflicts;
                self.conflict_count += admission_report.conflicts as u64;
                report.moves += admission_report.moves;
                self.announce_decisions(&admission_report.decisions_opened, now);
                if let Some(timeline) = &self.timeline {
                    for (wanted, existing) in &admission_report.name_collisions {
                        timeline.push(
                            "collision",
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
            }
        }

        report.staged_executor = self.staged_executor.snapshot();
        self.emit_timeline_events(&report, now);

        if let Some(publisher) = self.status_publisher.as_ref() {
            publisher.publish(DaemonStatusSnapshot::from_app(&self.app));
        }

        Ok(report)
    }

    /// Rebuilds the watcher's path filter when the callback observed an
    /// ignore-file change. The rebuild (tree walk + glob compilation)
    /// deliberately runs here on the runtime thread, never in the
    /// callback.
    fn reload_path_filter_if_requested(&mut self) {
        if let Some(path_filter) = &self.path_filter {
            path_filter.rebuild_if_requested();
        }
    }

    /// Periodic in-run recovery of leases that exceeded the lease
    /// timeout (an orphaned execution). Startup recovery handles dead
    /// processes; this sweep handles a lease lost *within* a live run.
    /// Drops managed-trash entries past their retention, at startup and
    /// then on a slow cadence. A directory scan of the trash, cheap at
    /// the cadence and only while the daemon is otherwise ticking.
    fn purge_trash_if_due(&mut self, now: SystemTime) {
        let Some(trash) = &self.trash else {
            return;
        };
        let now_inst = self.clock.now();
        let interval = Duration::from_secs(constants::trash::PURGE_INTERVAL_SECONDS);
        let due = self
            .last_trash_purge_inst
            .map(|last| now_inst.saturating_duration_since(last) >= interval)
            .unwrap_or(true);
        if !due {
            return;
        }
        self.last_trash_purge_inst = Some(now_inst);
        let purged = trash.purge_expired(now);
        if purged > 0 {
            logging::info(
                "Purged expired trash entries",
                &[
                    ("profile_id", self.profile_id.clone()),
                    ("purged", purged.to_string()),
                ],
            );
        }
    }

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
    /// profiles watching the same directory — and the deduplicated
    /// watcher feeding them — reload ignore-rule changes together.
    pub(crate) fn adopt_shared_path_filter(
        &mut self,
        path_filter: Arc<crate::fs_events::SharedEventPathFilter>,
    ) {
        self.path_filter = Some(path_filter);
    }

    /// Whether any queued or in-flight work justifies the fast tick
    /// cadence — the multi-profile loop aggregates this across
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
            start_watcher,
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
        start_watcher: bool,
    ) -> Result<Self, DaemonRuntimeError> {
        let now = clock.now_system();
        let recovered_count = state_db.recover_leased(now)?;
        match state_db.prune_tombstones(now) {
            Ok(pruned) if pruned > 0 => logging::info(
                "Pruned tombstones past the retention window",
                &[("pruned", pruned.to_string())],
            ),
            Ok(_) => {}
            Err(error) => logging::warning(
                "Tombstone pruning failed; continuing",
                &[("error", error.to_string())],
            ),
        }
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

        let device_id = vapor_shared::device_id::derive_device_id();
        let mut local_replacement: Option<Option<String>> = None;
        if let Some(local_root) = sync_scope.local_sync_directory.as_deref() {
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
        sync_scope.local_sync_directory = sync_scope
            .local_sync_directory
            .take()
            .map(normalize_watch_root)
            .transpose()?;

        // The cloud root: adopted on first contact, checked against the
        // recorded identity afterwards. Synchronous only here, at
        // startup; every later probe runs on a worker. A runtime with
        // no local root (a parked profile) never touches the cloud.
        let cloud_outcome = if sync_scope.local_sync_directory.is_some() {
            probe_cloud_root(
                app.provider_handle(),
                sync_scope.cloud_sync_directory.as_str(),
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
                cloud_missing = sync_scope.local_sync_directory.is_some();
                false
            }
            CloudRootOutcome::Unreachable(_) => false,
        };

        let tick_waker = Arc::new(TickWaker::default());
        let recorder = sync_scope
            .local_sync_directory
            .as_ref()
            .map(|watch_root| Arc::new(BoundedFsEventRecorder::new(watch_root.clone())));
        // Built even when no watcher starts (multi-profile, tests): the
        // reconcile walk and remote-change mapping filter through it, so
        // remote-side ingest honors the same rules as local ingest.
        let path_filter = sync_scope.local_sync_directory.as_ref().map(|watch_root| {
            Arc::new(crate::fs_events::SharedEventPathFilter::new(
                watch_root,
                filter_options,
            ))
        });
        let watcher = if start_watcher {
            match (&sync_scope.local_sync_directory, &recorder, &path_filter) {
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

        if !cloud_root_ready {
            app.set_run_state(
                RunState::Error,
                format!(
                    "cloud sync directory {} is unavailable; sync work is blocked until it can be ensured",
                    sync_scope.cloud_sync_directory
                ),
            );
        } else if let Some(local_sync_directory) = &sync_scope.local_sync_directory {
            if start_watcher && watcher.is_none() {
                app.set_run_state(
                    RunState::Error,
                    format!(
                        "no native filesystem watcher on this host yet; local changes under {} are not detected",
                        local_sync_directory.display()
                    ),
                );
            } else {
                app.set_run_state(
                    RunState::Running,
                    format!("watching {}", local_sync_directory.display()),
                );
            }
        } else {
            app.set_run_state(RunState::Paused, "no local sync directory configured");
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
            running_reconcile_intent_id: None,
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
            last_root_check_inst: None,
            reconcile_after_root_check: false,
            profile_id: DEFAULT_PROFILE_ID.to_string(),
            timeline_default_entries: None,
            cloud_root_ready,
            last_cloud_root_attempt_inst: None,
            reconcile_walker: None,
            mirror_revert_count: 0,
            mirror_delete_count: 0,
            conflict_count: 0,
            timeline: None,
            resource_budget: Arc::new(Mutex::new(crate::resource_budget::ResourceBudget::new(
                crate::resource_budget::EffectiveBudgetConfig::resolve(
                    &vapor_shared::config::VaporConfig::default(),
                ),
            ))),
            bandwidth_shaper: Arc::new(Mutex::new(vapor_providers::BandwidthShaper::unlimited())),
            idle_notifier: Arc::new(vapor_platform::AlwaysIdleNotifier),
            transfer_step_bytes: Arc::new(std::sync::atomic::AtomicU64::new(
                constants::engine::TRANSFER_STAGE_STEP_BYTES,
            )),
            latest_ceilings: None,
            last_sampled_inputs: None,
            last_timeline_run_state: None,
            last_timeline_throttle: None,
            device_id: vapor_shared::device_id::derive_device_id(),
            active_coding: crate::safeguards::ActiveCodingHeuristic::default(),
            mass_change_guard: crate::safeguards::MassChangeGuard::default(),
            mass_delete_settings: crate::safeguards::MassDeleteGuardSettings::default(),
            trash: None,
            last_trash_purge_inst: None,
            flush_boost_until_inst: None,
        };
        if let Some(found) = local_replacement {
            runtime.hold_for_replaced_root(crate::root_identity::RootSide::Local, found, now)?;
        } else if let Some(found) = cloud_replacement {
            runtime.hold_for_replaced_root(crate::root_identity::RootSide::Cloud, found, now)?;
        } else if cloud_missing {
            runtime.hold_for_missing_root(crate::root_identity::RootSide::Cloud, now)?;
        }
        if !cloud_root_ready {
            runtime.last_cloud_root_attempt_inst = Some(runtime.clock.now());
        }
        Ok(runtime)
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

    /// Applies the resolved `safeguards` config group: rebuilds the
    /// mass-delete guard with the configured window/threshold. Called
    /// at composition, before any events flow.
    pub fn configure_mass_delete_guard(
        &mut self,
        settings: crate::safeguards::MassDeleteGuardSettings,
    ) {
        self.mass_delete_settings = settings;
        self.mass_change_guard = crate::safeguards::MassChangeGuard::with_ratio(
            settings.window,
            settings.threshold,
            settings.ratio_percent,
        );
    }

    /// Attaches the profile's trash. Every local removal the engine
    /// performs from then on goes through it.
    pub fn attach_trash(&mut self, trash: crate::trash::LocalTrash) {
        self.trash = Some(trash);
    }

    /// Applies the `trash` config group to the attached trash.
    pub fn configure_trash(&mut self, settings: crate::trash::TrashSettings) {
        if let Some(trash) = &mut self.trash {
            trash.configure(settings);
        }
    }

    pub fn trash(&self) -> Option<&crate::trash::LocalTrash> {
        self.trash.as_ref()
    }

    /// Passes the explicit transfer-concurrency ceiling to the app (see
    /// `resourceLimits.maxConcurrentTransfers`).
    pub fn set_max_concurrent_transfers(&mut self, ceiling: Option<usize>) {
        self.app.set_max_concurrent_transfers(ceiling);
    }

    /// Wires the daemon-wide resource management set in:
    /// the budget, the bandwidth shaper, the shared transfer step knob,
    /// and the idle signal. The multi-profile runtime shares one of
    /// each across every profile.
    pub fn attach_resource_management(
        &mut self,
        budget: Arc<Mutex<crate::resource_budget::ResourceBudget>>,
        shaper: Arc<Mutex<vapor_providers::BandwidthShaper>>,
        transfer_step_bytes: Arc<std::sync::atomic::AtomicU64>,
        idle_notifier: Arc<dyn vapor_platform::IdleNotifier>,
    ) {
        self.resource_budget = budget;
        self.bandwidth_shaper = shaper;
        self.transfer_step_bytes = transfer_step_bytes;
        self.idle_notifier = idle_notifier;
    }

    pub fn set_idle_notifier(&mut self, idle_notifier: Arc<dyn vapor_platform::IdleNotifier>) {
        self.idle_notifier = idle_notifier;
    }

    /// Latest effective ceilings + utilization for the IPC surface
    ///. `None` until the first 1s sample.
    pub fn resource_budget_status(&self) -> Option<vapor_ipc::ResourceBudgetStatus> {
        let ceilings = self.latest_ceilings.as_ref()?;
        let inputs = self.last_sampled_inputs.as_ref();
        let bandwidth_rate_kbps = self
            .bandwidth_shaper
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .rate()
            .map(|bytes_per_sec| bytes_per_sec * 8 / 1_000)
            .unwrap_or(0);
        Some(vapor_ipc::ResourceBudgetStatus {
            effective_cpu_percent: ceilings.cpu_percent,
            effective_memory_percent: ceilings.memory_percent,
            effective_bandwidth_percent: ceilings.bandwidth_percent,
            cpu_utilization_percent: inputs.map(|i| i.vapor_cpu_load_percent).unwrap_or(0),
            memory_utilization_percent: inputs
                .and_then(|i| memory_share_percent(i.vapor_memory_bytes, i.device_memory_bytes))
                .unwrap_or(0),
            bandwidth_utilization_kbps: bandwidth_rate_kbps,
            idle_boost_state: ceilings.boost_state.to_string(),
            idle_boost_reason: ceilings.reason.clone(),
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
    pub fn intent_diagnostics(
        &self,
        profile_id: &str,
        limit: usize,
        now: SystemTime,
    ) -> Vec<vapor_ipc::IntentDiagnostic> {
        let mut rows = Vec::new();
        let mut active_ids = std::collections::BTreeSet::new();
        let workgate = self.app.workgate_snapshot();
        let paused = self.app.snapshot().run_state == RunState::Paused;

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
                    "upload concurrency {}/{} in use",
                    workgate.active_uploads, workgate.caps.upload_concurrency
                ),
                crate::executor::ExecutionStage::WaitingForDownload => format!(
                    "download concurrency {}/{} in use",
                    workgate.active_downloads, workgate.caps.download_concurrency
                ),
                _ => String::new(),
            };
            rows.push(vapor_ipc::IntentDiagnostic {
                intent_id,
                profile_id: profile_id.to_string(),
                path: path.to_string_lossy().into_owned(),
                action: format!("{kind:?}").to_lowercase(),
                stage: format!("{stage:?}"),
                elapsed_in_stage_ms: elapsed_ms,
                attempt_count,
                last_error,
                blocker_reason,
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
            let stage = if intent.held_by.is_some() {
                "Held"
            } else if retrying {
                "Retrying"
            } else {
                "Queued"
            };
            let blocker_reason = if let Some(decision) = intent.held_by {
                format!("waiting for decision #{decision} (vapor decisions list)")
            } else if paused {
                "daemon is paused".to_string()
            } else if !self.cloud_root_ready {
                "cloud sync directory is unavailable".to_string()
            } else if self.startup_reconstruction_barrier {
                "waiting for the startup reconstruction reconcile".to_string()
            } else if retrying {
                format!(
                    "retry backoff active (attempt {}), next attempt at {:?}",
                    intent.attempt_count, intent.available_at
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
            rows.push(vapor_ipc::IntentDiagnostic {
                intent_id: intent.id,
                profile_id: profile_id.to_string(),
                path: intent.path.to_string_lossy().into_owned(),
                action: format!("{:?}", intent.kind).to_lowercase(),
                stage: stage.to_string(),
                elapsed_in_stage_ms,
                attempt_count: intent.attempt_count,
                last_error: intent.last_error.unwrap_or_default(),
                blocker_reason,
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
                "run_state",
                profile_id,
                format!("{:?}: {}", run_state, self.app.snapshot().reason),
                now,
            );
            self.last_timeline_run_state = Some(run_state);
        }
        let throttle_state = self.app.snapshot().throttle_state;
        if self.last_timeline_throttle != Some(throttle_state) {
            timeline.push(
                "throttle",
                profile_id,
                format!(
                    "{:?}: {}",
                    throttle_state,
                    self.app
                        .throttle_decision()
                        .map(|decision| decision.reason.clone())
                        .unwrap_or_default()
                ),
                now,
            );
            self.last_timeline_throttle = Some(throttle_state);
        }
        if report.conflicts > 0 {
            timeline.push(
                "conflict",
                profile_id,
                format!("kept both versions for {} path(s)", report.conflicts),
                now,
            );
        }
        if report.failed_intents > 0 {
            timeline.push(
                "intent_failed",
                profile_id,
                format!("{} intent(s) failed terminally", report.failed_intents),
                now,
            );
        }
        if report.mirror_reverts > 0 || report.mirror_deletes > 0 {
            timeline.push(
                "mirror",
                profile_id,
                format!(
                    "strict mirror reverted {} and removed {} path(s)",
                    report.mirror_reverts, report.mirror_deletes
                ),
                now,
            );
        }
        if let Some(root) = &report.completed_reconcile_root {
            timeline.push(
                "reconcile",
                profile_id,
                format!("reconcile completed for {}", root.display()),
                now,
            );
        }
        if report.remote_poll.cursor_expired {
            timeline.push(
                "feed",
                profile_id,
                "remote changes cursor expired; whole-scope reconcile scheduled",
                now,
            );
        }
    }

    /// Overrides the device identifier (the bootstrap passes the value
    /// persisted in `vapor.json`; a later task forbids silent regeneration).
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
    }

    /// Cumulative keep-both conflict copies created since daemon start.
    pub fn conflict_count(&self) -> u64 {
        self.conflict_count
    }

    /// Cumulative count of strict-mirror reverts / deletions performed
    /// by the one-way modes since daemon start (diagnostics).
    pub fn mirror_counters(&self) -> (u64, u64) {
        (self.mirror_revert_count, self.mirror_delete_count)
    }

    /// Retries ensuring the provider-side sync root while it is
    /// unavailable. Between attempts, sync work stays blocked
    /// and intents accumulate durably — never dropped.
    /// Flips the daemon into the blocked cloud-root state when the root
    /// vanishes *mid-run* (deleted, unmounted, remote folder removed).
    /// Admission stops on the next tick (`paused` derives from
    /// `cloud_root_ready`), the periodic ensure-retry recreates the root,
    /// and recovery schedules a whole-scope reconcile — the same
    /// self-healing path a missing root takes at startup.
    fn mark_cloud_root_unavailable(&mut self, reason: &str, now: SystemTime) {
        if !self.cloud_root_ready {
            return;
        }
        self.cloud_root_ready = false;
        // Retry immediately on the next tick, then at the ensure cadence.
        self.last_cloud_root_attempt_inst = None;
        logging::warning(
            "Cloud sync directory became unavailable; blocking sync work until it is re-ensured",
            &[
                (
                    "cloud_sync_directory",
                    self.sync_scope.cloud_sync_directory.clone(),
                ),
                ("reason", reason.to_string()),
            ],
        );
        // Preserve an explicit pause (user or mass-deletion guard): the
        // root recovery path must not silently resume either.
        if self.app.snapshot().run_state != RunState::Paused {
            self.app.set_run_state(
                RunState::Error,
                format!(
                    "cloud sync directory {} is unavailable; sync work is blocked until it can be ensured",
                    self.sync_scope.cloud_sync_directory
                ),
            );
        }
        if let Some(timeline) = &self.timeline {
            timeline.push(
                "cloud-root",
                self.profile_id.clone(),
                "cloud sync directory became unavailable; sync blocked until it is restored",
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
        // the slower ensure cadence.
        let interval = if self.root_hold.is_some() {
            Duration::from_secs(constants::engine::ROOT_CHECK_INTERVAL_SECONDS)
        } else {
            Duration::from_secs(constants::engine::CLOUD_ROOT_ENSURE_RETRY_SECONDS)
        };
        let Some(outcome) = self.harvest_or_start_cloud_probe(now_inst, interval) else {
            return Ok(());
        };
        let now = self.clock.now_system();
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
                self.release_root_hold(crate::root_identity::RootSide::Cloud, now);
                if let Err(error) = self.forget_the_outage() {
                    logging::warning(
                        "Could not drop the deletions queued during the outage",
                        &[("error", error.to_string())],
                    );
                }
                // A recovered root may have drifted while unreachable: a
                // whole-scope reconcile converges it. The two-way walk
                // only deletes locally behind a remote-origin tombstone,
                // so a root that came back emptier re-uploads instead of
                // mirroring the emptiness back.
                if let Err(error) = self.enqueue_startup_reconstruction_reconcile(now) {
                    logging::warning(
                        "Could not schedule the post-recovery whole-scope reconcile",
                        &[("error", format!("{error:?}"))],
                    );
                }
                self.restore_run_state_after_root_recovery("cloud sync directory recovered");
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
            CloudRootOutcome::Unreachable(message) => logging::debug(
                "Cloud root probe failed; retrying at the ensure cadence",
                &[("error", message)],
            ),
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
        let cloud_root = self.sync_scope.cloud_sync_directory.clone();
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
        if self.root_hold.is_some() {
            let reason = self
                .root_hold
                .as_ref()
                .map(|hold| hold.reason.clone())
                .unwrap_or_default();
            self.app.set_run_state(RunState::Error, reason);
            return;
        }
        if !self.cloud_root_ready {
            self.app.set_run_state(
                RunState::Error,
                format!(
                    "cloud sync directory {} is unavailable; sync work is blocked until it comes back",
                    self.sync_scope.cloud_sync_directory
                ),
            );
            return;
        }
        if self.app.snapshot().run_state == RunState::Paused {
            return;
        }
        match self.sync_scope.local_sync_directory.as_ref() {
            Some(path) => self.app.set_run_state(
                RunState::Running,
                format!("{what}; watching {}", path.display()),
            ),
            None => self
                .app
                .set_run_state(RunState::Paused, "no local sync directory configured"),
        }
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
                .local_sync_directory
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            RootSide::Cloud => self.sync_scope.cloud_sync_directory.clone(),
        };
        let path = match side {
            RootSide::Local => self.sync_scope.local_sync_directory.clone(),
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
            "{} sync directory {root} is not the folder this profile adopted; answer decision #{decision_id} (vapor decisions list) or put the original folder back",
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
                .local_sync_directory
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            RootSide::Cloud => self.sync_scope.cloud_sync_directory.clone(),
        };
        let path = match side {
            RootSide::Local => self.sync_scope.local_sync_directory.clone(),
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
            "{} sync directory {root} is missing; Vapor waits for it and never re-creates a folder it synced before (decision #{decision_id}: vapor decisions list)",
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
                self.state_db.withdraw_decision(decision.id, now)?;
                if let Some(timeline) = &self.timeline {
                    timeline.push(
                        "decision",
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
            if let Err(error) = self.state_db.withdraw_decision(decision_id, now) {
                logging::warning(
                    "Could not withdraw the root-replaced decision",
                    &[("error", error.to_string())],
                );
            }
            if let Some(timeline) = &self.timeline {
                timeline.push(
                    "decision",
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
        if !due {
            return Ok(());
        }
        self.last_root_check_inst = Some(now_inst);
        if let Some(local_root) = self.sync_scope.local_sync_directory.clone() {
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
                        self.restore_run_state_after_root_recovery("local sync directory is back");
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
                CloudRootOutcome::Ready { .. } => {}
                CloudRootOutcome::Unreachable(message) => logging::debug(
                    "Cloud root check failed; keeping the last known state",
                    &[("error", message)],
                ),
                CloudRootOutcome::Missing => {
                    self.mark_cloud_root_unavailable("the cloud sync directory is missing", now);
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
        let (pause_request, reconcile_request, flush_request) = match self.runtime_control.as_ref()
        {
            Some(control) => {
                let pause = control.take_pause_request();
                let reconcile = control.take_reconcile_request();
                let flush = control.take_flush_request();
                (pause, reconcile, flush)
            }
            None => (None, false, false),
        };

        if let Some(pause) = pause_request {
            if pause {
                self.app
                    .set_run_state(RunState::Paused, "user paused via vapor pause");
            } else {
                // Re-derive the run state from what can actually admit
                // work: resuming while the cloud root is unavailable must
                // not report Running (which would mask the real blocker),
                // since the tick loop still leases nothing.
                if !self.cloud_root_ready {
                    self.app.set_run_state(
                        RunState::Error,
                        format!(
                            "cloud sync directory {} is unavailable; sync work is blocked until it can be ensured",
                            self.sync_scope.cloud_sync_directory
                        ),
                    );
                } else {
                    let reason = match self.sync_scope.local_sync_directory.as_ref() {
                        Some(path) => {
                            format!("user resumed via vapor resume; watching {}", path.display())
                        }
                        None => "user resumed via vapor resume".to_string(),
                    };
                    self.app.set_run_state(RunState::Running, reason);
                }
            }
        }
        if flush_request {
            // Flush boost: pull deferred work forward for a
            // bounded window. Deferred reconciles release immediately
            // (bypassing the idle gate) and the remote feed polls on
            // the next tick; execution still answers to the normal
            // throttle ladder, so device-impact invariants hold.
            let window = Duration::from_secs(constants::engine::FLUSH_BOOST_SECONDS);
            self.flush_boost_until_inst = Some(self.clock.now() + window);
            self.remote_poller.request_immediate_poll();
            if let Some(timeline) = &self.timeline {
                timeline.push(
                    "flush",
                    self.profile_id.as_str(),
                    format!(
                        "flush requested: deferred work released for {}s",
                        constants::engine::FLUSH_BOOST_SECONDS
                    ),
                    now,
                );
            }
            logging::info(
                "Flush boost activated",
                &[(
                    "window_seconds",
                    constants::engine::FLUSH_BOOST_SECONDS.to_string(),
                )],
            );
        }
        if reconcile_request {
            self.enqueue_startup_reconstruction_reconcile(now)?;
        }
        Ok(())
    }

    fn sample_throttle_inputs(&mut self, now: SystemTime, inputs: ThrottleInputs) {
        // Active-coding heuristic: rapid code-class churn means
        // the user is working even when no permissioned HID signal is
        // available. Strictly additive — it can only raise caution.
        let mut inputs = inputs;
        if !inputs.user_active && self.active_coding.is_active(now) {
            inputs.user_active = true;
        }
        // Sampling cadence uses the monotonic clock so wall-clock rewinds
        // cannot force an extra sample (or skip one). The injected clock
        // makes that property test-checkable.
        let now_inst = self.clock.now();
        let should_sample = self
            .last_throttle_sample_inst
            .map(|last| now_inst.saturating_duration_since(last) >= self.throttle_sample_interval)
            .unwrap_or(true);
        if should_sample {
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
            let capacity_kbps = inputs
                .network_throughput_kbps
                .unwrap_or(constants::engine::ASSUMED_LINK_CAPACITY_KBPS);
            let rate_bytes_per_sec =
                u64::from(capacity_kbps) * 1_000 / 8 * u64::from(ceilings.bandwidth_percent) / 100;
            self.bandwidth_shaper
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .set_rate(Some(rate_bytes_per_sec.max(1)));
            self.apply_memory_ceiling(ceilings.memory_percent, &inputs);
            self.latest_ceilings = Some(ceilings);
            self.last_sampled_inputs = Some(inputs);
        }
    }

    /// Memory-ceiling reactions. The ceiling is a share of device
    /// memory; when the host reports the daemon's resident size the
    /// comparison is against that share, and the bounded caches trim
    /// toward their documented floors while the process sits over it,
    /// restoring once it is clearly under (hysteresis at half the
    /// budget). The same trim also fires when the cache population
    /// alone outgrows an entry budget derived from the ceiling, which is
    /// the only signal on hosts without a memory sampler. Floors are
    /// enforced by the caches themselves, so loop prevention and
    /// observability never degrade below their guarantees.
    fn apply_memory_ceiling(&mut self, memory_percent: u8, inputs: &ThrottleInputs) {
        const ENTRIES_PER_MEMORY_PERCENT: usize = 400;
        let budget_entries = usize::from(memory_percent) * ENTRIES_PER_MEMORY_PERCENT;
        let usage = self.local_echoes.len()
            + self.remote_echoes.len()
            + self
                .timeline
                .as_ref()
                .map(|timeline| timeline.len())
                .unwrap_or(0);
        let (over_bytes, under_bytes) =
            match (inputs.vapor_memory_bytes, inputs.device_memory_bytes) {
                (Some(resident), Some(device)) if device > 0 => {
                    let budget = device / 100 * u64::from(memory_percent);
                    (resident > budget, resident < budget / 2)
                }
                _ => (false, true),
            };

        if usage > budget_entries || over_bytes {
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
                self.timeline_default_entries
                    .get_or_insert(timeline.max_entries());
                timeline.set_max_entries(budget_entries / 4);
            }
        } else if usage < budget_entries / 2 && under_bytes {
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
        let Some(local_sync_directory) = self.sync_scope.local_sync_directory.as_ref() else {
            return Ok(());
        };

        self.state_db
            .enqueue_startup_reconcile_intent(local_sync_directory, now)?;
        self.startup_reconstruction_barrier = true;
        // Monotonic deadline: a wall-clock rewind must not extend the
        // barrier (discipline, same as every other elapsed check).
        self.startup_barrier_expires_inst = Some(self.clock.now() + self.startup_barrier_deadline);
        logging::info(
            "Queued startup whole-scope reconcile for volatile-state reconstruction",
            &[
                ("root", local_sync_directory.display().to_string()),
                (
                    "barrier_deadline_ms",
                    self.startup_barrier_deadline.as_millis().to_string(),
                ),
            ],
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
        logging::warning(
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

        // Flush boost: while boosted, deferred reconciles
        // release immediately, bypassing both their not-before times
        // and the IdleDrain gate. Execution still answers to the
        // throttle ladder — this only moves work from "deferred" to
        // "queued".
        if self.flush_boost_active() {
            let scheduler = &mut self.scheduler;
            return recorder.with_mut_state(|maps| {
                let released = maps.take_all_deferred_reconcile_intents();
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

    fn flush_boost_active(&self) -> bool {
        self.flush_boost_until_inst
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

        let Some(watch_root) = self.sync_scope.local_sync_directory.clone() else {
            return (0, 0, 0);
        };
        let watch_root = watch_root.as_path();

        let stabilized = if force {
            self.debounce.drain_all_for_recorder(&recorder, now)
        } else {
            self.debounce.run_tick_for_recorder(&recorder, now)
        };
        let hash_algorithm = self.app.provider().content_hash_algorithm();
        let mut accepted = 0;
        let mut suppressed = 0;
        let mut mirror_reverts = 0;
        for event in stabilized {
            if !crate::fs_events::resolve_event_path_within_watch_root(watch_root, &event.path) {
                logging::warning(
                    "Dropped stabilized event that resolves outside watch root",
                    &[
                        ("watch_root", watch_root.display().to_string()),
                        ("path", event.path.display().to_string()),
                    ],
                );
                continue;
            }
            if is_local_self_write_echo(
                &mut self.local_echoes,
                &self.tags,
                &event,
                hash_algorithm,
                now,
            ) {
                suppressed += 1;
                logging::debug(
                    "Suppressed stabilized event as a self-write echo",
                    &[("path", event.path.display().to_string())],
                );
                continue;
            }
            // Safeguard taps (post-echo-suppression, so the engine's own
            // applied writes never count as user activity or deletions).
            self.active_coding
                .record_stabilized(event.debounce_class, now);
            // The deletion guard lives in the executor, at the moment a
            // deletion would become irreversible, so it sees both
            // directions and never counts a no-op.
            let intent_kind = crate::scheduler::intent_kind_for_stabilized_event(&event);
            if intent_kind == PendingIntentKind::Delete
                && let Ok(Some(decision_id)) = self
                    .state_db
                    .drop_held_at(&event.path, PendingIntentKind::ApplyRemoteDelete)
            {
                // The cloud's deletion of this path was held behind a
                // question; the user just deleted it here too.
                logging::info(
                    "Dropped a held cloud deletion: the file was deleted on this device as well",
                    &[
                        ("path", event.path.display().to_string()),
                        ("decision_id", decision_id.to_string()),
                    ],
                );
            }
            if self.sync_scope.sync_mode == vapor_shared::SyncMode::PullOnly {
                // Pull-only: local events never produce
                // local-to-remote intents. A local change is divergence
                // from the cloud source of truth, so it schedules a
                // restore-from-cloud for that path instead: the download
                // reverts edits, re-materializes deletions, and removes
                // local-only files when no remote counterpart exists.
                self.scheduler.upsert_intent(
                    event.path.clone(),
                    PendingIntentKind::Download,
                    event.last_observed_at,
                );
                mirror_reverts += 1;
                accepted += 1;
                continue;
            }
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
            self.scheduler
                .upsert_stabilized_event_as(event, intent_kind);
            accepted += 1;
        }
        (accepted, suppressed, mirror_reverts)
    }

    /// Walks a directory that just appeared and records one `Created`
    /// event per regular file underneath it, pruning ignored names the
    /// way the watcher bridge would. Stops at
    /// `SYNTHESIZED_SUBTREE_EVENT_CAP` files and leaves a subtree
    /// reconcile marker for the rest. Returns the number of files
    /// reported.
    fn synthesize_subtree_events(&mut self, directory: &Path, observed_at: SystemTime) -> usize {
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
    /// memory when the process exits. Returns the number of intents
    /// enqueued.
    pub fn flush_for_shutdown(&mut self, now: SystemTime) -> Result<usize, DaemonRuntimeError> {
        self.stabilize_events_with(now, true);
        self.drain_pending_intents();
        self.flush_scheduler_to_durable_queue()
    }

    /// Pushes a timeline event for every decision the executor opened
    /// this tick, so surfaces learn about the question right away.
    fn announce_decisions(&mut self, ids: &[i64], now: SystemTime) {
        for id in ids {
            let Ok(Some(decision)) = self.state_db.decision(*id) else {
                continue;
            };
            if let Some(timeline) = &self.timeline {
                timeline.push(
                    "decision",
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
                &serde_json::json!({ "reason": name.reason }),
                now,
            )?;
            logging::warning(
                "A local file has a name that cannot be synced; asking once",
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
            self.state_db.withdraw_decision(decision.id, now)?;
            if let Some(timeline) = &self.timeline {
                timeline.push(
                    "decision",
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

    /// A mass-deletion question whose every held deletion the other
    /// side applied meanwhile has nothing left to decide: withdraw it
    /// and re-arm the guard.
    fn withdraw_holds_left_empty(&mut self, now: SystemTime) -> Result<(), DaemonRuntimeError> {
        for decision in self.state_db.decisions(false)? {
            if decision.kind != "mass-deletion"
                || !self.state_db.held_intents(decision.id)?.is_empty()
            {
                continue;
            }
            self.state_db.withdraw_decision(decision.id, now)?;
            self.mass_change_guard.reset();
            logging::info(
                "Withdrew the mass-deletion decision: the other side already applied every held deletion",
                &[("decision_id", decision.id.to_string())],
            );
            if let Some(timeline) = &self.timeline {
                timeline.push(
                    "decision",
                    self.profile_id.clone(),
                    format!(
                        "Decision {} (mass-deletion) withdrawn: the other side already applied every held deletion",
                        decision.id
                    ),
                    now,
                );
            }
        }
        Ok(())
    }

    /// Acts on decisions the user answered through the CLI since the
    /// last tick: releases or drops the held intents, resets the guard
    /// that held them, and marks the decision applied.
    fn apply_resolved_decisions(&mut self, now: SystemTime) -> Result<(), DaemonRuntimeError> {
        let resolved = self.state_db.resolved_unapplied_decisions()?;
        for decision in resolved {
            let choice = decision.choice.clone().unwrap_or_default();
            let summary = match (decision.kind.as_str(), choice.as_str()) {
                ("mass-deletion", "apply") => {
                    let released = self.state_db.release_held(decision.id, now)?;
                    self.mass_change_guard.reset();
                    format!("applied: {released} held deletion(s) released")
                }
                ("mass-deletion", "discard") => {
                    // The deletions are not wanted: restore each path
                    // from the side that still has it.
                    let held = self.state_db.held_intents(decision.id)?;
                    let mut restores = Vec::new();
                    for intent in &held {
                        let restore = match intent.kind {
                            PendingIntentKind::Delete => PendingIntentKind::Download,
                            PendingIntentKind::ApplyRemoteDelete => PendingIntentKind::Upload,
                            other => other,
                        };
                        restores.push((intent.path.clone(), restore, now));
                    }
                    let dropped = self.state_db.drop_held(decision.id)?;
                    let enqueued = self.state_db.enqueue_intents_coalesced_with(
                        &restores,
                        crate::safeguards::IntentSource::Fresh,
                        true,
                    )?;
                    self.mass_change_guard.reset();
                    format!(
                        "discarded: {dropped} deletion(s) dropped, {enqueued} restore(s) enqueued"
                    )
                }
                (crate::type_mismatch::DECISION_KIND, choice) => {
                    match self.apply_type_mismatch(&decision, choice, now) {
                        Ok(summary) => summary,
                        Err(message) => {
                            logging::warning(
                                "Could not apply the type-mismatch answer; will retry",
                                &[("decision_id", decision.id.to_string()), ("error", message)],
                            );
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
                            // Left answered-but-unapplied: the next tick
                            // tries again, and the log says why.
                            logging::warning(
                                "Could not reattach the sync root; will retry",
                                &[("decision_id", decision.id.to_string()), ("error", message)],
                            );
                            continue;
                        }
                    }
                }
                (crate::unsyncable::DECISION_KIND, crate::unsyncable::OPTION_SKIP) => {
                    "skipped: the file stays on this device only".to_string()
                }
                (kind, choice) => {
                    // A kind this runtime does not know how to apply is
                    // left answered-but-unapplied for a build that does;
                    // it is reported, never silently consumed.
                    logging::warning(
                        "Answered decision has no applier in this build",
                        &[
                            ("decision_id", decision.id.to_string()),
                            ("kind", kind.to_string()),
                            ("choice", choice.to_string()),
                        ],
                    );
                    continue;
                }
            };
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
                    "decision",
                    self.profile_id.clone(),
                    format!("Decision {} ({}) {}", decision.id, decision.kind, summary),
                    now,
                );
            }
        }
        Ok(())
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
        let id = self.state_db.create_decision(
            DECISION_KIND,
            crate::state_db::DecisionScope::Path,
            Some(path),
            &question(path, local_is_dir),
            &options(local_is_dir),
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
                std::fs::rename(&path, &copy)
                    .map_err(|error| format!("cannot rename {} aside: {error}", path.display()))?;
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
            OPTION_PREFER_CLOUD => {
                let trash = self
                    .trash
                    .as_ref()
                    .ok_or_else(|| "no trash attached".to_string())?;
                trash
                    .discard(&path, constants::trash::REASON_CLOUD_DELETION, now)
                    .map_err(|error| {
                        format!("cannot move {} to the trash: {error}", path.display())
                    })?;
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
                self.state_db
                    .enqueue_intents_coalesced(
                        &[(path.clone(), PendingIntentKind::Delete, now)],
                        crate::safeguards::IntentSource::Fresh,
                    )
                    .map_err(|error| error.to_string())?;
                "preferred the local side: the cloud side is being removed; the local side goes up"
                    .to_string()
            }
            other => return Err(format!("no applier for choice {other:?}")),
        };
        // The reconcile that follows materializes the winning side.
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
                    .local_sync_directory
                    .clone()
                    .ok_or_else(|| "no local sync directory configured".to_string())?;
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
                let cloud_root = self.sync_scope.cloud_sync_directory.clone();
                if decision.kind == crate::root_identity::MISSING_DECISION_KIND {
                    self.app
                        .provider()
                        .ensure_cloud_sync_directory(cloud_root.as_str())
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
        self.enqueue_startup_reconstruction_reconcile(now)
            .map_err(|error| format!("{error:?}"))?;
        self.restore_run_state_after_root_recovery("sync root reattached");
        Ok(format!(
            "{} the {} sync directory; merging without deletions",
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

        let batch_limit = if self.startup_reconstruction_barrier {
            1
        } else {
            let workgate = self.app.workgate_snapshot();
            let next_is_reconcile = self
                .state_db
                .peek_next_ready_kind(now)?
                .map(|kind| kind == PendingIntentKind::ReconcileSubtree)
                .unwrap_or(false);
            self.staged_executor
                .admission_capacity(workgate)
                .min(workgate.available_planner_workers())
                + usize::from(self.running_reconcile_intent_id.is_none() && next_is_reconcile)
        };

        // `lease_ready_batch` marks every returned row leased up front, so
        // any early `break` below must hand the unprocessed remainder back
        // to the pending state — an abandoned leased row would sit invisible
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
                        && self.sync_scope.local_sync_directory.as_ref() == Some(&intent.path);
                    self.scheduler.upsert_intent(
                        intent.path.clone(),
                        intent.kind,
                        intent.available_at,
                    );
                    match self.app.try_start_reconcile(&mut self.scheduler, now) {
                        Ok(Some(root)) => {
                            self.running_reconcile_intent_id = Some(intent.id);
                            report.started_reconcile_root = Some(root);
                            if is_startup_root_reconcile {
                                break;
                            }
                            continue;
                        }
                        Ok(None) => {
                            self.scheduler.discard_pending(&intent.path);
                            self.requeue_runtime_intent(
                                intent.id,
                                now + blocked_intent_requeue_delay(),
                                "waiting for idle reconcile slot",
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
                            // admitting — breaking here would pin every
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
        let Some(scope_root) = self.sync_scope.local_sync_directory.clone() else {
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
            self.reconcile_walker = Some(
                crate::reconcile_walk::ReconcileWalker::new(
                    &scope_root,
                    &running_root,
                    self.path_filter.clone(),
                )
                .with_merge_without_deletions(merge)
                .with_device_id(&self.device_id),
            );
        }
        // Bound the chunk by a wall-clock slice so a slow provider's
        // enumerate cannot hold the tick thread for the whole directory
        // budget; the high directory budget lets a fast provider converge
        // a large tree quickly under IdleDrain.
        let clock = self.clock.clone();
        let deadline =
            clock.now() + Duration::from_millis(constants::engine::RECONCILE_SLICE_MILLIS);
        let should_continue = move || clock.now() < deadline;
        let walker = self
            .reconcile_walker
            .as_mut()
            .expect("walker was just ensured");
        walker.process(
            self.app.provider_handle(),
            &self.provider_call_mode,
            self.sync_scope.sync_mode,
            &mut self.state_db,
            constants::engine::RECONCILE_DIRS_PER_SLICE_IDLE_DRAIN,
            now,
            &should_continue,
        )
    }

    /// Forces this runtime into the blocking `Error` run state with a
    /// reason (used by the multi-profile shell to surface a profile that
    /// cannot run — e.g. an invalid provider — without performing any sync
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
            // The row vanished or changed state underneath us — an
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

/// Requeue delay applied when a leased intent cannot start because no
/// permit / slot is available. Coarser than the tick interval so blocked
/// intents don't churn a lease+requeue write pair on every tick.
/// Timeline text for a remote name that would alias a local file.
fn name_collision_message(wanted: &Path, existing: &Path) -> String {
    format!(
        "{} exists in the cloud but this filesystem cannot hold it next to {}; \
         both are left untouched until you rename one of them",
        wanted.display(),
        existing.display()
    )
}

fn blocked_intent_requeue_delay() -> Duration {
    Duration::from_millis(constants::engine::BLOCKED_INTENT_REQUEUE_DELAY_MILLIS)
}

/// Loop prevention on the local ingest path: decides whether a
/// stabilized local event is an echo of a write/delete the daemon itself
/// performed while applying remote changes.
///
/// Removals correlate by path + recency. Writes correlate by op-id tag
/// first; the content-hash fallback runs only when a live write record
/// exists for the path and the observed size matches the recorded size,
/// so the fallback never hashes a file that obviously diverged.
fn is_local_self_write_echo(
    local_echoes: &mut SelfWriteCache,
    tags: &OpIdTagStore,
    event: &crate::debounce::StabilizedEvent,
    algorithm: vapor_providers::HashAlgorithm,
    now: SystemTime,
) -> bool {
    let key = event.path.to_string_lossy();
    if event.last_event_kind == FsEventKind::Removed {
        return local_echoes.matches_delete(&key, now);
    }

    // Write echoes correlate by the file's *current* content, never by
    // the op-id tag alone: the tag survives later writes, so a tag-only
    // match would keep suppressing genuine user edits for the record's
    // whole TTL after a download-apply (an edit made right after a
    // download would silently never upload). The size gate keeps the
    // hash off every obviously-diverged file; the record always carries
    // both (the executor records size + hash on apply).
    if !local_echoes.has_write_record(&key, now) {
        return false;
    }
    let Ok(metadata) = std::fs::symlink_metadata(&event.path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    if let Some(expected_size) = local_echoes.expected_write_size(&key, now)
        && expected_size != metadata.len()
    {
        return false;
    }
    // Large files: correlate by the op-id tag rather than a full-file
    // hash. Hashing a multi-GB downloaded file inline here would stall
    // debounce release, remote polling, and executor advancement — and
    // would run even under Suspended (stabilization precedes the throttle
    // gate), violating "under Suspended, hashing stops". The size gate
    // above already rejects the common divergent-edit case.
    if metadata.len() > constants::engine::HASH_STAGE_STEP_BYTES {
        let op_id = tags.read_op_id(&event.path);
        return local_echoes.matches_write(&key, op_id.as_deref(), None, now);
    }
    let Ok(content_hash) = hash_hex_of_file_with(&event.path, algorithm) else {
        return false;
    };
    local_echoes.matches_write(&key, None, Some(&content_hash), now)
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
    fn start_skips_whole_scope_reconcile_when_no_local_sync_directory_exists() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");

        let runtime = DaemonRuntime::start(
            SyncScope {
                local_sync_directory: None,
                cloud_sync_directory: "/Vapor".to_string(),
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
            runtime.sync_scope().local_sync_directory,
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
            runtime.sync_scope().local_sync_directory.clone().unwrap()
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
            runtime.sync_scope().local_sync_directory.clone()
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
            local_sync_directory: Some(watch_root.clone()),
            cloud_sync_directory: cloud_root.to_string_lossy().into_owned(),
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
            .local_sync_directory
            .clone()
            .expect("runtime watch root");
        // Real payload on disk: the pipeline hashes and uploads actual
        // bytes — no simulator.
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
        let planner_cap = crate::throttle::idle_drain_concurrency();
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
            .local_sync_directory
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
        // fsync-bound and slow on cold-cache CI runners — Windows disk I/O in
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
    fn runtime_reconcile_flow_is_idle_biased_and_completes_across_ticks() {
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
            .local_sync_directory
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
                    user_active: true,
                    ..ThrottleInputs::default()
                },
            )
            .expect("runtime tick");
        assert_eq!(blocked.released_deferred_reconciles, 0);
        assert!(blocked.started_reconcile_root.is_none());

        // Advance past the throttle dwell window for `Throttled`
        // (MIN_DWELL_THROTTLED_SECONDS = 5 s) before the second
        // evaluation so the controller is allowed to transition back to
        // IdleDrain.
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
            local_sync_directory: Some(watch_root.to_path_buf()),
            cloud_sync_directory: "/Vapor".to_string(),
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
    }

    impl BidirectionalFixture {
        fn new() -> Self {
            Self::new_with_mode(vapor_shared::SyncMode::TwoWay)
        }

        fn new_with_mode(sync_mode: vapor_shared::SyncMode) -> Self {
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
                local_sync_directory: Some(watch_root.clone()),
                cloud_sync_directory: cloud_root.to_string_lossy().into_owned(),
                sync_mode,
            };
            let runtime = DaemonRuntime::build(
                sync_scope,
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
                .local_sync_directory
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
            }
        }

        /// One tick, advancing both clocks by `advance_ms`.
        fn tick(&mut self, advance_ms: u64) -> RuntimeTickReport {
            self.clock.advance(Duration::from_millis(advance_ms));
            self.now_ms += advance_ms;
            self.runtime
                .tick_with_inputs(timestamp_ms(self.now_ms), ThrottleInputs::default())
                .expect("runtime tick")
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

    #[test]
    fn cloud_deletion_of_a_previously_uploaded_file_propagates_locally() {
        // Field-testing repro: local create A (uploads), cloud create B
        // (downloads), local delete B (propagates up), cloud delete A —
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
        // echo through the feed. Loop prevention must suppress it — no
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
    fn remote_delete_echo_is_suppressed_after_local_delete_propagates() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline

        // Seed both sides, then delete locally and propagate.
        let local_file = fixture.watch_root.join("shared.txt");
        std::fs::write(fixture.cloud_root.join("shared.txt"), b"payload").expect("seed remote");
        std::fs::write(&local_file, b"payload").expect("seed local");
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
        // record that echo. It must be suppressed before the scheduler —
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
                local_sync_directory: Some(watch_root.clone()),
                cloud_sync_directory: cloud_root.to_string_lossy().into_owned(),
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

    #[test]
    fn concurrent_edit_conflict_keeps_both_versions_on_both_sides() {
        // Simultaneous local + foreign remote edits of a synced
        // file resolve as keep-both — the remote canonical lands at the
        // original path, the local edit survives as a conflict copy, and
        // the copy propagates to the cloud.
        let mut fixture = BidirectionalFixture::new();
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
    }

    #[test]
    fn delete_modify_race_resolves_for_the_modification() {
        // A remote deletion racing a local modification loses —
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
                local_sync_directory: Some(watch_root.clone()),
                cloud_sync_directory: cloud_root.to_string_lossy().into_owned(),
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
            .local_sync_directory
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

        // The feed echo of our own upload must still be suppressed —
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
        // drain with parallel admission (planner cap 4 in IdleDrain),
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
        // With a tiny measured link capacity the shaper
        // grants almost nothing per second, so an upload holds at its
        // checkpoint; restoring capacity lets it complete.
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000); // baseline (default inputs)

        let starved_inputs = ThrottleInputs {
            network_throughput_kbps: Some(1), // ~31 bytes/s at 25%
            ..ThrottleInputs::default()
        };
        // Sample the starved inputs so the shaper rate collapses.
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

        // Capacity restored: the transfer completes.
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
        // The AlwaysIdle notifier + IdleDrain inputs engage
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
            boosted.caps.planner_workers > crate::throttle::idle_drain_concurrency(),
            "boost must raise caps above the base ({} <= {})",
            boosted.caps.planner_workers,
            crate::throttle::idle_drain_concurrency()
        );
        let status = fixture
            .runtime
            .resource_budget_status()
            .expect("published after first sample");
        assert_eq!(status.idle_boost_state, "active");
        assert!(status.effective_cpu_percent > 15);

        // Heavy foreign load drives the throttle out of IdleDrain: caps
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
            snapped.caps.planner_workers <= crate::throttle::idle_drain_concurrency(),
            "post-IdleDrain states never run against boosted caps"
        );
        let status = fixture
            .runtime
            .resource_budget_status()
            .expect("still published");
        assert_eq!(status.idle_boost_state, "off");
        assert_eq!(status.effective_cpu_percent, 15);
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
            local_sync_directory: Some(watch_root.clone()),
            cloud_sync_directory: cloud_root.to_string_lossy().into_owned(),
            sync_mode: vapor_shared::SyncMode::TwoWay,
        };

        // First daemon: lease the intent into flight, then "crash"
        // (drop) before completing it. The lease is taken directly on
        // the durable DB — exactly the state a daemon that died between
        // leasing and completing leaves behind. (A ticking runtime can
        // no longer model this window: stage chaining completes a small
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
            // Dropped here with the lease still open — simulated crash.
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
            local_sync_directory: Some(watch_root.clone()),
            cloud_sync_directory: cloud_root.to_string_lossy().into_owned(),
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
    struct OfflineFixture {
        temp: TempDir,
        watch_root: PathBuf,
        cloud_root: PathBuf,
        database_path: PathBuf,
    }

    impl OfflineFixture {
        fn new() -> Self {
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
                local_sync_directory: Some(self.watch_root.clone()),
                cloud_sync_directory: self.cloud_root.to_string_lossy().into_owned(),
                sync_mode: vapor_shared::SyncMode::TwoWay,
            }
        }

        /// Builds a runtime on the shared DB, schedules the startup
        /// reconcile, and drives it to quiescence.
        fn run(&self, seed_uploads: &[&Path]) -> DaemonRuntime {
            use crate::clock::Clock as _;
            let mut state_db = DurableStateDb::open(&self.database_path).expect("open state db");
            for path in seed_uploads {
                state_db
                    .enqueue_intent(path, PendingIntentKind::Upload, timestamp_ms(0))
                    .expect("enqueue");
            }
            let clock = Arc::new(crate::clock::ManualClock::at_now());
            let mut runtime = DaemonRuntime::build(
                self.scope(),
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
                    && runtime.state_db().held_intent_count().expect("held") == 0
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
        assert!(
            runtime
                .state_db()
                .decisions(false)
                .expect("open")
                .is_empty()
        );
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
            local_sync_directory: Some(watch_root.clone()),
            cloud_sync_directory: cloud_root.to_string_lossy().into_owned(),
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
        // Regression for the "pause is cosmetic" bug: `vapor pause` used
        // to flip the status string while the runtime kept leasing and
        // executing. Paused now means: ingest keeps capturing intents,
        // but nothing new is leased until resume.
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
        let runtime_watch_root = runtime
            .sync_scope()
            .local_sync_directory
            .clone()
            .expect("watch root");
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
        // Regression: a checkpoint-paused reconcile is tracked both as a
        // requeued durable row and as a pending scheduler intent; the
        // flush used to insert a fresh durable row for the scheduler copy
        // on every pause, multiplying whole-subtree reconciles.
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
        // (RECONCILE_DIRS_PER_SLICE_IDLE_DRAIN) cannot finish the
        // comparison: a finished walk completes instead of pausing, so
        // observing pause cycles requires a genuinely in-progress walk.
        for index in 0..(constants::engine::RECONCILE_DIRS_PER_SLICE_IDLE_DRAIN * 2) {
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
            local_sync_directory: Some(watch_root.clone()),
            cloud_sync_directory: cloud_root.to_string_lossy().into_owned(),
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

        // Tick 2 (user becomes active): the reconcile checkpoint-pauses.
        clock.advance(Duration::from_secs(1));
        let paused = runtime
            .tick_with_inputs(
                timestamp_ms(2_000),
                ThrottleInputs {
                    user_active: true,
                    ..ThrottleInputs::default()
                },
            )
            .expect("pause tick");
        assert_eq!(paused.requeued_intents, 1, "report: {paused:?}");
        assert_eq!(runtime.state_db().queue_depth().expect("depth"), 1);

        // Ticks 3..6 (still active): every pause cycle used to add one
        // duplicate durable row; coalescing must keep the depth at 1.
        for step in 1..=4u64 {
            clock.advance(Duration::from_secs(1));
            runtime
                .tick_with_inputs(
                    timestamp_ms(2_000 + step * 1_000),
                    ThrottleInputs {
                        user_active: true,
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
        // resulting throttle decision must downshift away from `IdleDrain`.
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
    fn tick_with_default_static_sampler_picks_idle_drain() {
        // Sanity check: with no overrides, the default static sampler
        // mirrors `ThrottleInputs::default()` so the engine starts in
        // IdleDrain — the existing behavior the clock plumbing must preserve.
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
        assert_eq!(decision.state, vapor_shared::ThrottleState::IdleDrain);
    }

    fn timestamp_ms(milliseconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(milliseconds)
    }

    #[test]
    fn attached_status_publisher_receives_fresh_snapshot_each_tick() {
        // Regression test for the status-stuck bug: without the
        // publisher hook, `vapor status` would always return the boot
        // snapshot. We attach a recording publisher and assert the run
        // state actually flips through it after a pause request.
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
        assert_eq!(initial.run_state, "Running");

        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        runtime.attach_control(control.clone());
        control.request_pause();

        runtime
            .tick_with_inputs(timestamp_ms(1_500), ThrottleInputs::default())
            .expect("runtime tick");

        let after = publisher
            .last()
            .expect("publisher saw a post-tick snapshot");
        assert_eq!(after.run_state, "Paused");
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
    fn requested_reconcile_with_flush_boost_downloads_external_cloud_file_and_drains() {
        // The e2e S10 shape: a file appears in the cloud root without a
        // feed event (external write), the user runs `vapor reconcile`
        // (startup-barrier semantics) and `vapor flush-now` (boost).
        // The scheduled download must complete and the queue drain —
        // nothing may wedge leased.
        let mut fixture = BidirectionalFixture::new();
        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        fixture.runtime.attach_control(control.clone());
        fixture.tick(6_000); // baseline

        std::fs::write(fixture.cloud_root.join("external.txt"), b"external payload")
            .expect("seed cloud file without a feed event");
        control.request_reconcile();
        control.request_flush();

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
        // mirror. Ignore rules must hold in *both* directions — through
        // the changes feed and through a requested reconcile — so the
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
        // coalescing). The delete must still reach the cloud — this
        // exact shape used to become an Upload plan that no-op'd as
        // "vanished before upload", leaving the remote copy immortal.
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
        // unlink — the part that used to flip the intent to Upload.
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
                .intent_diagnostics("default", 10, timestamp_ms(fixture.now_ms + 20));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].path, file.display().to_string());
        assert_eq!(diagnostics[0].action, "upload");
    }

    #[test]
    fn editing_a_file_last_synced_from_an_external_write_uploads_without_conflict() {
        // Real-provider everyday shape: an external writer changes the
        // cloud file (no op-id tag), we download it, then the user edits
        // locally. The upload must overwrite — the index hash proves the
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

    // ---- Optional advanced safeguards ----

    /// Seeds `count` synced files (uploaded, indexed) so a deletion
    /// burst has a tree to be measured against.
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
    fn a_local_deletion_burst_is_held_behind_a_decision_while_other_work_continues() {
        let mut fixture = BidirectionalFixture::new();
        let timeline = crate::timeline::TimelineBuffer::new(256);
        fixture.runtime.attach_timeline(timeline.clone());
        fixture
            .runtime
            .configure_mass_delete_guard(crate::safeguards::MassDeleteGuardSettings {
                enabled: true,
                threshold: 1_000,
                window: Duration::from_secs(60),
                ratio_percent: 25,
            });
        fixture.tick(6_000);
        let paths = seed_synced_files(&mut fixture, 40);

        // Twelve of forty files vanish at once: over a quarter of the
        // tree, well under the absolute threshold.
        for path in &paths[..12] {
            std::fs::remove_file(path).expect("unlink");
            fixture.record_local_event(path, FsEventKind::Removed, fixture.now_ms);
        }
        // And an ordinary edit arrives with them.
        let edited = &paths[30];
        std::fs::write(edited, b"edited during the burst").expect("edit");
        fixture.record_local_event(edited, FsEventKind::Modified, fixture.now_ms);
        fixture.converge(30);

        let decision = fixture
            .runtime
            .state_db()
            .open_decision("mass-deletion", None)
            .expect("query")
            .expect("the burst must open a mass-deletion decision");
        assert_eq!(
            decision.held_intents, 12,
            "the whole burst is held before any of it lands"
        );
        assert!(
            decision.question.contains("12 of the 40 files"),
            "the question names the whole burst: {}",
            decision.question
        );
        assert!(decision.question.contains("this device"));
        assert!(
            timeline
                .snapshot(None)
                .iter()
                .any(|entry| entry.kind == "decision"),
            "the decision must land on the timeline"
        );
        // The daemon is not paused; the edit went through.
        assert_eq!(fixture.runtime.app.snapshot().run_state, RunState::Running);
        assert_eq!(
            std::fs::read(fixture.cloud_root.join("d0/f30.txt")).expect("cloud copy"),
            b"edited during the burst"
        );
        // Held deletions have not touched the cloud.
        let surviving = paths[..12]
            .iter()
            .filter(|path| {
                fixture
                    .cloud_root
                    .join(path.strip_prefix(&fixture.watch_root).unwrap())
                    .exists()
            })
            .count();
        assert_eq!(surviving, 12, "held deletions must not propagate");

        // Answer: apply. The held deletions release and propagate; the
        // guard re-arms with an empty window.
        fixture
            .runtime
            .state_db_mut()
            .resolve_decision(decision.id, "apply", timestamp_ms(fixture.now_ms))
            .expect("resolve");
        fixture.converge(30);
        for path in &paths[..12] {
            let cloud = fixture
                .cloud_root
                .join(path.strip_prefix(&fixture.watch_root).unwrap());
            assert!(
                !cloud.exists(),
                "{} must be deleted after apply",
                cloud.display()
            );
        }
        assert!(
            fixture
                .runtime
                .state_db()
                .open_decision("mass-deletion", None)
                .expect("query")
                .is_none(),
            "the answered decision is closed"
        );
        assert!(!fixture.runtime.mass_change_guard.is_tripped());
    }

    #[test]
    fn an_answer_given_the_moment_the_guard_trips_covers_the_whole_burst() {
        // The user (or an app) answers as soon as the question appears,
        // while most of the burst is still queued behind the first
        // deletion. The answer must cover the burst the question
        // counted: no second question for the stragglers.
        let mut fixture = BidirectionalFixture::new();
        fixture
            .runtime
            .configure_mass_delete_guard(crate::safeguards::MassDeleteGuardSettings {
                enabled: true,
                threshold: 1_000,
                window: Duration::from_secs(60),
                ratio_percent: 25,
            });
        fixture.tick(6_000);
        let paths = seed_synced_files(&mut fixture, 40);
        for path in &paths[..12] {
            std::fs::remove_file(path).expect("unlink");
            fixture.record_local_event(path, FsEventKind::Removed, fixture.now_ms);
        }
        let mut decision = None;
        for _ in 0..40 {
            fixture.tick(1_000);
            decision = fixture
                .runtime
                .state_db()
                .open_decision("mass-deletion", None)
                .expect("query");
            if decision.is_some() {
                break;
            }
        }
        let decision = decision.expect("the burst opens a decision");
        assert_eq!(
            decision.held_intents, 12,
            "every queued deletion of the burst is held the moment the guard trips"
        );
        fixture
            .runtime
            .state_db_mut()
            .resolve_decision(decision.id, "apply", timestamp_ms(fixture.now_ms))
            .expect("resolve");
        fixture.converge(40);
        for path in &paths[..12] {
            let cloud = fixture
                .cloud_root
                .join(path.strip_prefix(&fixture.watch_root).unwrap());
            assert!(!cloud.exists(), "{} must be deleted", cloud.display());
        }
        let asked = fixture
            .runtime
            .state_db()
            .decisions(true)
            .expect("history")
            .into_iter()
            .filter(|decision| decision.kind == "mass-deletion")
            .count();
        assert_eq!(asked, 1, "one burst, one question");
    }

    #[test]
    fn a_hold_the_other_side_makes_moot_is_withdrawn_on_its_own() {
        // A local burst is held; meanwhile the same files are deleted
        // in the cloud (another device did the same cleanup). Nothing
        // is left to decide: the hold empties and the question goes.
        let mut fixture = BidirectionalFixture::new();
        fixture
            .runtime
            .configure_mass_delete_guard(crate::safeguards::MassDeleteGuardSettings {
                enabled: true,
                threshold: 1_000,
                window: Duration::from_secs(60),
                ratio_percent: 25,
            });
        fixture.tick(6_000);
        let paths = seed_synced_files(&mut fixture, 40);
        for path in &paths[..12] {
            std::fs::remove_file(path).expect("unlink");
            fixture.record_local_event(path, FsEventKind::Removed, fixture.now_ms);
        }
        fixture.converge(30);
        let decision = fixture
            .runtime
            .state_db()
            .open_decision("mass-deletion", None)
            .expect("query")
            .expect("the burst opens a decision");
        assert_eq!(decision.held_intents, 12);

        for path in &paths[..12] {
            let cloud = fixture
                .cloud_root
                .join(path.strip_prefix(&fixture.watch_root).unwrap());
            std::fs::remove_file(&cloud).expect("cloud delete");
            fixture
                .feed
                .emit_removed(cloud, timestamp_ms(fixture.now_ms));
        }
        // Held rows are not queued work, so the poll cadence, not the
        // queue, decides when the feed is read.
        for _ in 0..12 {
            fixture.tick(6_000);
        }
        fixture.converge(30);
        assert!(
            fixture
                .runtime
                .state_db()
                .open_decision("mass-deletion", None)
                .expect("query")
                .is_none(),
            "an emptied hold is withdrawn without an answer"
        );
        let withdrawn = fixture
            .runtime
            .state_db()
            .decisions(true)
            .expect("history")
            .into_iter()
            .find(|decision| decision.kind == "mass-deletion")
            .expect("kept in history");
        assert_eq!(withdrawn.choice.as_deref(), Some("withdrawn"));
        assert_eq!(
            fixture.runtime.state_db().queue_depth().expect("depth"),
            0,
            "nothing is left queued or held"
        );
        assert!(!fixture.runtime.mass_change_guard.is_tripped());
    }

    #[test]
    fn discarding_a_held_deletion_burst_restores_the_files() {
        let mut fixture = BidirectionalFixture::new();
        fixture
            .runtime
            .configure_mass_delete_guard(crate::safeguards::MassDeleteGuardSettings {
                enabled: true,
                threshold: 1_000,
                window: Duration::from_secs(60),
                ratio_percent: 25,
            });
        fixture.tick(6_000);
        let paths = seed_synced_files(&mut fixture, 40);
        for path in &paths[..12] {
            std::fs::remove_file(path).expect("unlink");
            fixture.record_local_event(path, FsEventKind::Removed, fixture.now_ms);
        }
        fixture.converge(30);
        let decision = fixture
            .runtime
            .state_db()
            .open_decision("mass-deletion", None)
            .expect("query")
            .expect("decision");
        fixture
            .runtime
            .state_db_mut()
            .resolve_decision(decision.id, "discard", timestamp_ms(fixture.now_ms))
            .expect("resolve");
        fixture.converge(30);
        // Every held path is back on disk, downloaded from the cloud.
        for path in &paths[..12] {
            assert!(
                path.exists(),
                "{} must be restored after discard",
                path.display()
            );
        }
        assert_eq!(fixture.runtime.state_db().queue_depth().expect("depth"), 0);
    }

    #[test]
    fn a_cloud_deletion_burst_is_held_and_discarding_it_restores_the_cloud_copies() {
        let mut fixture = BidirectionalFixture::new();
        fixture
            .runtime
            .configure_mass_delete_guard(crate::safeguards::MassDeleteGuardSettings {
                enabled: true,
                threshold: 1_000,
                window: Duration::from_secs(60),
                ratio_percent: 25,
            });
        fixture.tick(6_000);
        let paths = seed_synced_files(&mut fixture, 40);
        let cloud_paths: Vec<PathBuf> = paths[..12]
            .iter()
            .map(|path| {
                fixture
                    .cloud_root
                    .join(path.strip_prefix(&fixture.watch_root).unwrap())
            })
            .collect();
        // Another device removes twelve files in the cloud at once.
        for cloud in &cloud_paths {
            std::fs::remove_file(cloud).expect("cloud unlink");
            fixture
                .feed
                .emit_removed(cloud.clone(), timestamp_ms(fixture.now_ms));
        }
        fixture.converge(30);

        let decision = fixture
            .runtime
            .state_db()
            .open_decision("mass-deletion", None)
            .expect("query")
            .expect("the burst must open a mass-deletion decision");
        assert_eq!(decision.held_intents, 12);
        assert!(decision.question.contains("from the cloud"));
        for path in &paths[..12] {
            assert!(path.exists(), "{} must survive while held", path.display());
        }

        // The user did not mean it: the local copies go back up.
        fixture
            .runtime
            .state_db_mut()
            .resolve_decision(decision.id, "discard", timestamp_ms(fixture.now_ms))
            .expect("resolve");
        fixture.converge(30);
        for cloud in &cloud_paths {
            assert!(
                cloud.exists(),
                "{} must be restored after discard",
                cloud.display()
            );
        }
        assert_eq!(
            fixture
                .runtime
                .state_db()
                .held_intent_count()
                .expect("held"),
            0
        );
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
        assert_eq!(entries[0].reason, constants::trash::REASON_CLOUD_DELETION);

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
        assert_eq!(entries[0].reason, constants::trash::REASON_MIRROR_REMOVAL);
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
        let inode_before = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                before.ino()
            }
            #[cfg(not(unix))]
            {
                0u64
            }
        };

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
            assert_eq!(after.ino(), inode_before, "the cloud copy was rewritten");
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
        // Boot state: cloud root was unavailable and a pause (user or the
        // mass-deletion guard) is active.
        fixture.runtime.cloud_root_ready = false;
        fixture.runtime.app.set_run_state(
            RunState::Paused,
            "mass-deletion guard: review the changes, then run `vapor resume`",
        );

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
            "cloud sync directory /nope is unavailable; sync work is blocked until it can be ensured",
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
            fixture
                .runtime
                .app
                .snapshot()
                .reason
                .contains("cloud sync directory"),
            "the real blocker must remain visible in the reason"
        );
    }

    #[test]
    fn code_file_churn_trips_active_coding_heuristic_into_user_active_throttle() {
        let mut fixture = BidirectionalFixture::new();
        fixture.tick(6_000);
        assert_eq!(
            fixture.runtime.app.snapshot().throttle_state,
            vapor_shared::ThrottleState::IdleDrain,
            "quiet defaults sample as idle"
        );

        // Rapid code-class churn: threshold-many stabilized .rs events.
        for index in 0..constants::engine::ACTIVE_CODING_EVENT_THRESHOLD {
            let path = fixture.watch_root.join(format!("src/module-{index}.rs"));
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            std::fs::write(&path, b"fn main() {}").expect("seed code file");
            fixture.record_local_event(&path, FsEventKind::Modified, fixture.now_ms);
        }
        fixture.tick(6_000); // stabilize → heuristic records the churn
        fixture.tick(2_000); // next sample sees the heuristic signal

        assert_eq!(
            fixture.runtime.app.snapshot().throttle_state,
            vapor_shared::ThrottleState::Throttled,
            "active-coding heuristic must sample as user-active"
        );
    }

    #[test]
    fn flush_request_boosts_remote_poll_cadence_and_lands_on_timeline() {
        let mut fixture = BidirectionalFixture::new();
        let timeline = crate::timeline::TimelineBuffer::new(256);
        fixture.runtime.attach_timeline(timeline.clone());
        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        fixture.runtime.attach_control(control.clone());

        let first = fixture.tick(6_000); // baseline poll
        assert!(first.remote_poll.polled);
        let quiet = fixture.tick(1_000); // 1s later: cadence not due
        assert!(!quiet.remote_poll.polled);

        control.request_flush();
        let boosted = fixture.tick(1_000);
        assert!(
            boosted.remote_poll.polled,
            "flush boost must clear the remote poll cadence"
        );
        assert!(
            timeline
                .snapshot(None)
                .iter()
                .any(|entry| entry.kind == "flush"),
            "flush boost must land on the activity timeline"
        );
    }
}
