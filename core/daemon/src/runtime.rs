use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use vapor_platform::fs_caps::NativeFilesystemCapabilities;
use vapor_providers::Provider;
use vapor_providers::filesystem::hash_hex_of_file;
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
/// (C8-19) fans the runtime out per profile.
pub const DEFAULT_PROFILE_ID: &str = "default";

/// Consecutive tick failures tolerated before the runtime loop gives up.
/// One inconsistent row or transient I/O error is logged and survived;
/// a structurally broken database fails fast after this many attempts.
const MAX_CONSECUTIVE_TICK_ERRORS: u32 = 5;

#[derive(Debug)]
pub enum DaemonRuntimeError {
    Watcher(FsEventsWatcherError),
    StateDb(StateDbError),
}

impl From<FsEventsWatcherError> for DaemonRuntimeError {
    fn from(value: FsEventsWatcherError) -> Self {
        Self::Watcher(value)
    }
}

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
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
    /// local applies (loop prevention, C8-7).
    pub suppressed_local_echoes: usize,
    pub durable_enqueues: usize,
    pub leased_intents: usize,
    pub started_staged_intents: usize,
    pub completed_intents: usize,
    pub requeued_intents: usize,
    /// Intents finalized as terminal failures this tick.
    pub failed_intents: usize,
    /// Strict-mirror reverts observed this tick (one-way modes; C8-65).
    pub mirror_reverts: usize,
    /// Strict-mirror deletions performed this tick (one-way modes; C8-65).
    pub mirror_deletes: usize,
    /// Keep-both conflict copies created this tick (C8-14).
    pub conflicts: usize,
    pub started_reconcile_root: Option<PathBuf>,
    pub completed_reconcile_root: Option<PathBuf>,
    pub staged_executor: StagedExecutorSnapshot,
    /// Remote changes-feed poll outcome (C8-6).
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
    /// Whether the provider-side sync root has been ensured. While
    /// `false`, no work is leased and the remote feed is not polled;
    /// ingest keeps capturing intent state durably (C8-50).
    cloud_root_ready: bool,
    last_cloud_root_attempt_inst: Option<Instant>,
    /// Incremental comparison walk of the currently-running reconcile.
    /// Survives slice pauses so a large tree converges across slices
    /// instead of restarting from scratch (C8-60/C8-62 strict mirror).
    reconcile_walker: Option<crate::reconcile_walk::ReconcileWalker>,
    /// Cumulative strict-mirror observability counters (C8-63/C8-65):
    /// one-way modes must never be silent about the data they rewrite.
    mirror_revert_count: u64,
    mirror_delete_count: u64,
    /// Cumulative keep-both conflict copies created (C8-14).
    conflict_count: u64,
    /// Daemon-wide activity timeline (C8-30); shared across profiles.
    timeline: Option<Arc<crate::timeline::TimelineBuffer>>,
    /// Daemon-wide resource budget (C8-36); shared across profiles.
    resource_budget: Arc<Mutex<crate::resource_budget::ResourceBudget>>,
    /// Daemon-wide bandwidth shaper (C8-38); shared across profiles.
    bandwidth_shaper: Arc<Mutex<vapor_providers::BandwidthShaper>>,
    /// User-idle signal for the idle-boost gates. The native HID bridge
    /// is a platform follow-up; headless semantics (always idle) apply
    /// until it lands.
    idle_notifier: Arc<dyn vapor_platform::IdleNotifier>,
    /// Auto-tuned per-tick transfer step budget (C8-42), shared across
    /// profiles.
    transfer_step_bytes: Arc<std::sync::atomic::AtomicU64>,
    /// Latest published ceilings + last sampled inputs (C8-40).
    latest_ceilings: Option<crate::resource_budget::EffectiveCeilings>,
    last_sampled_inputs: Option<ThrottleInputs>,
    /// Last states emitted to the timeline, so transitions emit once.
    last_timeline_run_state: Option<RunState>,
    last_timeline_throttle: Option<vapor_shared::ThrottleState>,
    /// Stable device identifier (C8-15). Resolved and persisted by the
    /// bootstrap; ephemeral (hostname-derived, unpersisted) in ad-hoc
    /// embeddings and tests.
    device_id: String,
    /// C8-55 — heuristic active-coding signal; ORs `user_active` into
    /// the throttle inputs when code-class files churn rapidly.
    active_coding: crate::safeguards::ActiveCodingHeuristic,
    /// C8-57 — mass-deletion guard; pauses the daemon and raises a
    /// timeline alert on a local deletion storm.
    mass_change_guard: crate::safeguards::MassChangeGuard,
    /// C8-56 — flush boost deadline (monotonic). While set and in the
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
        logging::warning(
            "Received shutdown signal; exiting daemon runtime loop cleanly",
            &[],
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
    /// control requests (pause / resume / flush / reconcile). Wave 7
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
        self.retry_cloud_root_if_needed();

        let (stabilized_events, suppressed_local_echoes, stabilize_mirror_reverts) =
            self.stabilize_events(now);

        // Pause semantics ("stops admitting new work"): ingest, debounce,
        // and the durable flush keep running so intent state is never
        // lost, and work already in flight runs to completion — but no
        // new work is released or leased while paused. An unavailable
        // cloud root blocks the same way (C8-50). Evaluated *after*
        // stabilization so a mass-deletion guard trip (C8-57) stops
        // admission in the same tick that detected the storm.
        let paused = self.app.snapshot().run_state == RunState::Paused || !self.cloud_root_ready;
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
        if self.cloud_root_ready {
            report.remote_poll = self.remote_poller.poll_if_due(
                &mut self.app,
                &mut self.state_db,
                &mut self.remote_echoes,
                self.sync_scope.local_sync_directory.as_deref(),
                self.sync_scope.sync_mode,
                &self.clock,
                now,
            )?;
            report.mirror_reverts += report.remote_poll.mirror_reverts;
            report.mirror_deletes += report.remote_poll.mirror_deletes;
            self.mirror_revert_count += report.remote_poll.mirror_reverts as u64;
            self.mirror_delete_count += report.remote_poll.mirror_deletes as u64;
        }

        let staged_report = {
            let mut env = ExecutionEnv {
                local_root: self.sync_scope.local_sync_directory.as_deref(),
                sync_mode: self.sync_scope.sync_mode,
                device_id: &self.device_id,
                transfer_step_bytes: self
                    .transfer_step_bytes
                    .load(std::sync::atomic::Ordering::Relaxed),
                bandwidth: &self.bandwidth_shaper,
                tags: &self.tags,
                local_echoes: &mut self.local_echoes,
                remote_echoes: &mut self.remote_echoes,
            };
            self.staged_executor
                .advance(&mut self.app, &mut self.state_db, &mut env, now)?
        };
        report.completed_intents += staged_report.completed;
        report.requeued_intents += staged_report.retried;
        report.failed_intents += staged_report.failed;
        report.mirror_deletes += staged_report.mirror_deletes;
        self.mirror_delete_count += staged_report.mirror_deletes as u64;
        report.conflicts += staged_report.conflicts;
        self.conflict_count += staged_report.conflicts as u64;

        if let Some(reconcile_intent_id) = self.running_reconcile_intent_id {
            // A running reconcile performs one bounded chunk of real
            // comparison work per tick, then answers to the controller's
            // slice/throttle checkpoint. The walker survives pauses so a
            // large tree converges across slices instead of restarting.
            match self.process_reconcile_walk(now) {
                Err(walk_error) => {
                    logging::warning(
                        "Reconcile comparison walk failed; yielding and retrying later",
                        &[("error", format!("{walk_error:?}"))],
                    );
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
                    }
                    if self
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
                    } else if walk_done
                        && let Some(completed_root) = self.complete_running_reconcile()?
                    {
                        self.reconcile_walker = None;
                        self.state_db.complete_leased(reconcile_intent_id)?;
                        self.running_reconcile_intent_id = None;
                        if self.startup_reconstruction_barrier
                            && self.sync_scope.local_sync_directory.as_ref()
                                == Some(&completed_root)
                        {
                            self.startup_reconstruction_barrier = false;
                            self.startup_barrier_expires_inst = None;
                        }
                        report.completed_intents += 1;
                        report.completed_reconcile_root = Some(completed_root);
                    }
                }
            }
        }

        if !paused {
            self.process_ready_queue(now, &mut report)?;
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
        if let Some(watcher) = &self.watcher {
            watcher.path_filter().rebuild_if_requested();
        }
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
    /// these per-profile recorders (C8-23).
    pub(crate) fn event_recorder(&self) -> Option<Arc<BoundedFsEventRecorder>> {
        self.recorder.clone()
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

        sync_scope.local_sync_directory = sync_scope
            .local_sync_directory
            .take()
            .map(normalize_watch_root)
            .transpose()?;

        let cloud_root_ready = app
            .ensure_cloud_sync_directory(sync_scope.cloud_sync_directory.as_str())
            .is_ok();

        let tick_waker = Arc::new(TickWaker::default());
        let recorder = sync_scope
            .local_sync_directory
            .as_ref()
            .map(|watch_root| Arc::new(BoundedFsEventRecorder::new(watch_root.clone())));
        let watcher = if start_watcher {
            match (&sync_scope.local_sync_directory, &recorder) {
                (Some(watch_root), Some(recorder)) => Some(FsEventsWatcher::start(
                    watch_root.clone(),
                    Arc::new(NotifyingRecorder {
                        inner: recorder.clone(),
                        waker: tick_waker.clone(),
                    }),
                    filter_options,
                )?),
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
            app.set_run_state(
                RunState::Running,
                format!("watching {}", local_sync_directory.display()),
            );
        } else {
            app.set_run_state(RunState::Paused, "no local sync directory configured");
        }

        let debounce =
            DebounceLoop::with_windows_and_clock(DebounceWindows::default(), clock.clone());
        let tick_interval = debounce.tick_interval();
        Ok(Self {
            app,
            sync_scope,
            state_db,
            recorder,
            debounce,
            staged_executor: StagedExecutor::with_clock(clock.clone()),
            scheduler: KeyedSupersedingScheduler::default(),
            watcher,
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
            flush_boost_until_inst: None,
        })
    }

    /// Wires the shared daemon activity timeline in (C8-30).
    pub fn attach_timeline(&mut self, timeline: Arc<crate::timeline::TimelineBuffer>) {
        self.timeline = Some(timeline);
    }

    /// Wires the daemon-wide resource management set in (C8-36..C8-42):
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

    /// Latest effective ceilings + utilization for the IPC surface
    /// (C8-40). `None` until the first 1s sample.
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
            memory_utilization_percent: 0,
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

    /// Per-intent "why stuck" rows (C8-29): active executor stages plus
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

        for (intent_id, path, kind, stage, elapsed_ms) in self.staged_executor.active_stages() {
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
                attempt_count: 0,
                last_error: String::new(),
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
            let stage = if retrying { "Retrying" } else { "Queued" };
            let blocker_reason = if paused {
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
    /// outcomes (C8-30). Cheap: only fires on changes and non-zero
    /// counters.
    fn emit_timeline_events(&mut self, report: &RuntimeTickReport, now: SystemTime) {
        let Some(timeline) = self.timeline.clone() else {
            return;
        };
        let profile_id = DEFAULT_PROFILE_ID;

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
    /// persisted in `vapor.json`; C8-15 forbids silent regeneration).
    pub fn set_device_id(&mut self, device_id: impl Into<String>) {
        self.device_id = device_id.into();
    }

    /// Cumulative keep-both conflict copies created since daemon start.
    pub fn conflict_count(&self) -> u64 {
        self.conflict_count
    }

    /// Cumulative count of strict-mirror reverts / deletions performed
    /// by the one-way modes since daemon start (C8-65 diagnostics).
    pub fn mirror_counters(&self) -> (u64, u64) {
        (self.mirror_revert_count, self.mirror_delete_count)
    }

    /// Retries ensuring the provider-side sync root while it is
    /// unavailable (C8-50). Between attempts, sync work stays blocked
    /// and intents accumulate durably — never dropped.
    fn retry_cloud_root_if_needed(&mut self) {
        if self.cloud_root_ready {
            return;
        }
        let now_inst = self.clock.now();
        let retry_interval =
            Duration::from_secs(constants::engine::CLOUD_ROOT_ENSURE_RETRY_SECONDS);
        let due = self
            .last_cloud_root_attempt_inst
            .map(|last| now_inst.saturating_duration_since(last) >= retry_interval)
            .unwrap_or(true);
        if !due {
            return;
        }
        self.last_cloud_root_attempt_inst = Some(now_inst);
        if self
            .app
            .ensure_cloud_sync_directory(self.sync_scope.cloud_sync_directory.as_str())
            .is_ok()
        {
            self.cloud_root_ready = true;
            match self.sync_scope.local_sync_directory.as_ref() {
                Some(path) => self.app.set_run_state(
                    RunState::Running,
                    format!(
                        "cloud sync directory recovered; watching {}",
                        path.display()
                    ),
                ),
                None => self
                    .app
                    .set_run_state(RunState::Paused, "no local sync directory configured"),
            }
        }
    }

    /// Drains any pause / resume / flush / reconcile requests recorded
    /// on the attached `RuntimeControl` and applies them. Called at
    /// the top of every tick. Wave 7 / `cli.md` L3.
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
                let reason = match self.sync_scope.local_sync_directory.as_ref() {
                    Some(path) => {
                        format!("user resumed via vapor resume; watching {}", path.display())
                    }
                    None => "user resumed via vapor resume".to_string(),
                };
                self.app.set_run_state(RunState::Running, reason);
                // An explicit resume is the human-in-the-loop reset for
                // the mass-deletion guard (C8-57): the operator looked
                // at the alert and decided the changes are legitimate.
                self.mass_change_guard.reset();
            }
        }
        if flush_request {
            // Flush boost (C8-56): pull deferred work forward for a
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
                    DEFAULT_PROFILE_ID,
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
        // Active-coding heuristic (C8-55): rapid code-class churn means
        // the user is working even when no permissioned HID signal is
        // available. Strictly additive — it can only raise caution.
        let mut inputs = inputs;
        if !inputs.user_active && self.active_coding.is_active(now) {
            inputs.user_active = true;
        }
        // Sampling cadence uses the monotonic clock so wall-clock rewinds
        // cannot force an extra sample (or skip one). The injected clock
        // makes that property test-checkable. C2-3.
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

            // Resource budget evaluation (C8-36..C8-40) on the same
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
            self.apply_memory_ceiling(ceilings.memory_percent);
            self.latest_ceilings = Some(ceilings);
            self.last_sampled_inputs = Some(inputs);
        }
    }

    /// Memory-ceiling reactions (C8-39): bounded caches trim toward
    /// their documented floors when the entry population outgrows the
    /// ceiling-derived budget, and restore when clearly under it
    /// (hysteresis at half the budget). Floors are enforced by the
    /// caches themselves — loop prevention and observability never
    /// degrade below their guarantees.
    fn apply_memory_ceiling(&mut self, memory_percent: u8) {
        const ENTRIES_PER_MEMORY_PERCENT: usize = 400;
        let budget_entries = usize::from(memory_percent) * ENTRIES_PER_MEMORY_PERCENT;
        let usage = self.local_echoes.len()
            + self.remote_echoes.len()
            + self
                .timeline
                .as_ref()
                .map(|timeline| timeline.len())
                .unwrap_or(0);

        if usage > budget_entries {
            let squeezed_ttl =
                Duration::from_millis(vapor_shared::constants::self_write_cache::MIN_TTL_MILLIS);
            let squeezed_entries = vapor_shared::constants::self_write_cache::MIN_ENTRIES;
            self.local_echoes.set_bounds(squeezed_ttl, squeezed_entries);
            self.remote_echoes
                .set_bounds(squeezed_ttl, squeezed_entries);
            if let Some(timeline) = &self.timeline {
                timeline.set_max_entries(budget_entries / 4);
            }
        } else if usage < budget_entries / 2 {
            let default_ttl = Duration::from_millis(
                vapor_shared::constants::self_write_cache::DEFAULT_TTL_MILLIS,
            );
            let default_entries = vapor_shared::constants::self_write_cache::MAX_ENTRIES;
            self.local_echoes.set_bounds(default_ttl, default_entries);
            self.remote_echoes.set_bounds(default_ttl, default_entries);
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
        // barrier (C2-3 discipline, same as every other elapsed check).
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

        // Flush boost (C8-56): while boosted, deferred reconciles
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
        let Some(recorder) = &self.recorder else {
            return (0, 0, 0);
        };

        let Some(watch_root) = self.sync_scope.local_sync_directory.as_ref() else {
            return (0, 0, 0);
        };

        let stabilized = self.debounce.run_tick_for_recorder(recorder, now);
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
            if is_local_self_write_echo(&self.tags, &mut self.local_echoes, &event, now) {
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
            if event.last_event_kind == crate::fs_events::FsEventKind::Removed
                && self.mass_change_guard.record_delete(now)
            {
                // Mass-change / ransomware guard (C8-57): stop admitting
                // work before the deletion storm replicates to the cloud.
                // Ingest keeps capturing intent state durably; an explicit
                // `vapor resume` is the human-in-the-loop reset.
                let reason = format!(
                    "mass-deletion guard: {} or more local deletions inside {}s; \
                     sync paused — review the changes, then run `vapor resume`",
                    constants::engine::MASS_DELETE_THRESHOLD,
                    constants::engine::MASS_DELETE_WINDOW_SECONDS,
                );
                self.app.set_run_state(RunState::Paused, reason.clone());
                if let Some(timeline) = &self.timeline {
                    timeline.push("guard", DEFAULT_PROFILE_ID, reason.clone(), now);
                }
                logging::warning(
                    "Mass-deletion guard tripped; pausing sync",
                    &[
                        (
                            "threshold",
                            constants::engine::MASS_DELETE_THRESHOLD.to_string(),
                        ),
                        (
                            "window_seconds",
                            constants::engine::MASS_DELETE_WINDOW_SECONDS.to_string(),
                        ),
                    ],
                );
            }
            if self.sync_scope.sync_mode == vapor_shared::SyncMode::PullOnly {
                // Pull-only (C8-60): local events never produce
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
            self.scheduler.upsert_stabilized_event(event);
            accepted += 1;
        }
        (accepted, suppressed, mirror_reverts)
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

        // Priority classes (C8-56): within one flush batch, key config
        // and code paths enqueue before lockfile noise, so they get the
        // lower durable ids that break lease-order ties. Stable sort
        // preserves arrival order inside each class.
        let windows = crate::debounce::DebounceWindows::default();
        claimed.sort_by_key(|intent| {
            crate::safeguards::intent_priority_rank(windows.classify_path(&intent.path).0)
        });

        let batch: Vec<_> = claimed
            .iter()
            .map(|intent| (intent.path.clone(), intent.kind, intent.last_observed_at))
            .collect();
        let enqueued = self.state_db.enqueue_intents_coalesced(&batch)?;

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

    fn process_ready_queue(
        &mut self,
        now: SystemTime,
        report: &mut RuntimeTickReport,
    ) -> Result<(), DaemonRuntimeError> {
        self.evaluate_startup_barrier(now);

        if self.startup_reconstruction_barrier && self.running_reconcile_intent_id.is_some() {
            return Ok(());
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

        for intent in self.state_db.lease_ready_batch(now, batch_limit)? {
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
                    if self.try_start_staged_intent(intent, now) {
                        report.started_staged_intents += 1;
                    } else {
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

        Ok(())
    }

    fn try_start_staged_intent(&mut self, intent: DurableIntentRecord, now: SystemTime) -> bool {
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
            self.reconcile_walker = Some(crate::reconcile_walk::ReconcileWalker::new(
                &scope_root,
                &running_root,
            ));
        }
        let walker = self
            .reconcile_walker
            .as_mut()
            .expect("walker was just ensured");
        walker.process(
            self.app.provider(),
            self.sync_scope.sync_mode,
            &mut self.state_db,
            constants::engine::RECONCILE_DIRS_PER_CHECKPOINT,
            now,
        )
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
fn blocked_intent_requeue_delay() -> Duration {
    Duration::from_millis(constants::engine::BLOCKED_INTENT_REQUEUE_DELAY_MILLIS)
}

/// Loop prevention on the local ingest path (C8-7): decides whether a
/// stabilized local event is an echo of a write/delete the daemon itself
/// performed while applying remote changes.
///
/// Removals correlate by path + recency. Writes correlate by op-id tag
/// first; the content-hash fallback runs only when a live write record
/// exists for the path and the observed size matches the recorded size,
/// so the fallback never hashes a file that obviously diverged.
fn is_local_self_write_echo(
    tags: &OpIdTagStore,
    local_echoes: &mut SelfWriteCache,
    event: &crate::debounce::StabilizedEvent,
    now: SystemTime,
) -> bool {
    let key = event.path.to_string_lossy();
    if event.last_event_kind == FsEventKind::Removed {
        return local_echoes.matches_delete(&key, now);
    }

    let op_id = tags.read_op_id(&event.path);
    if local_echoes.matches_write(&key, op_id.as_deref(), None, now) {
        return true;
    }
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
    let Ok(content_hash) = hash_hex_of_file(&event.path) else {
        return false;
    };
    local_echoes.matches_write(&key, None, Some(&content_hash), now)
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
    use vapor_providers::default_provider;

    #[test]
    fn start_queues_whole_scope_reconcile_for_restart_reconstruction() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");

        let runtime =
            DaemonRuntime::start(test_sync_scope(&watch_root), state_db, default_provider())
                .expect("runtime");

        assert!(runtime.has_live_watcher());
        assert_eq!(runtime.state_db().queue_depth().expect("queue depth"), 1);
        assert_eq!(
            runtime.state_db().pending_depth().expect("pending depth"),
            1
        );
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
            default_provider(),
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
            default_provider(),
        )
        .expect("runtime");

        assert_eq!(
            runtime.sync_scope().local_sync_directory,
            Some(std::fs::canonicalize(&real_watch_root).expect("canonical watch root"),)
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

        let runtime =
            DaemonRuntime::start(test_sync_scope(&watch_root), state_db, default_provider())
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

        let mut runtime =
            DaemonRuntime::start(test_sync_scope(&watch_root), state_db, default_provider())
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
        // cloud directory names the provider-side root (C8-2).
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
        // bytes — no simulator (C8-13).
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
        assert_eq!(first_tick.completed_intents, 0);

        // Drive follow-up ticks until the pipeline completes the upload.
        let mut completed = 0;
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
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut state_db = DurableStateDb::open(&database_path).expect("open durable state db");
        for index in 0..6 {
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
            default_provider(),
            Arc::new(StaticMetricsSampler::default()),
            system_clock(),
            false,
        )
        .expect("runtime");

        let first_tick = runtime
            .tick_with_inputs(timestamp_ms(250), ThrottleInputs::default())
            .expect("runtime tick");

        assert_eq!(first_tick.started_staged_intents, 4);
        assert_eq!(first_tick.staged_executor.planner_running, 4);
        assert_eq!(runtime.state_db().leased_depth().expect("leased depth"), 4);
        assert_eq!(
            runtime.state_db().pending_depth().expect("pending depth"),
            2
        );
    }

    #[test]
    fn composed_runtime_tick_regression_stays_under_guardrail() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");
        let mut runtime = DaemonRuntime::build(
            test_sync_scope(&watch_root),
            EventPathFilterOptions::default(),
            state_db,
            default_provider(),
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
            default_provider(),
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
        // IdleDrain. C2-4.
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

    /// Bidirectional tick-harness fixture (C8-10): a real filesystem
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
            let cloud_root = cloud_root.canonicalize().expect("canonical cloud root");
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
        // remote deletion (C8-17 preservation guard).
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
        // C8-60 / C8-66: cloud is authoritative. A local edit converges
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
        assert!(reverts >= 1, "the revert must be observable (C8-63)");
        assert!(deletes >= 1, "the removal must be observable (C8-63)");
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
        // C8-62 / C8-66: local is authoritative. Remote edits are
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
        // C8-61: the default mode still moves both directions and never
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
        // C8-66: intents durably enqueued under the previous mode must
        // not fire after a mode change (restart with new config). Stale
        // Upload intents complete as gated no-ops in pull-only.
        let temp = TempDir::new().expect("temp dir");
        let watch_root = temp.path().join("watch");
        let cloud_root = temp.path().join("cloud");
        std::fs::create_dir_all(&watch_root).expect("watch root");
        std::fs::create_dir_all(&cloud_root).expect("cloud root");
        let watch_root = watch_root.canonicalize().expect("canonical watch root");
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
        // C8-14: simultaneous local + foreign remote edits of a synced
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
        // edit survives in a conflict copy named per the C8-14 template.
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
        // C8-17: a remote deletion racing a local modification loses —
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
        // C8-12 happy-path race smoke: five runs alternating which side
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
        // C8-45 constraint compatibility: on filesystems without xattr
        // (FAT, network mounts), op-id tags fall back to side-files.
        // The full echo-suppression flow must still hold.
        let temp = TempDir::new().expect("temp dir");
        let watch_root = temp.path().join("watch");
        let cloud_root = temp.path().join("cloud");
        std::fs::create_dir_all(&watch_root).expect("watch root");
        std::fs::create_dir_all(&cloud_root).expect("cloud root");
        let cloud_root = cloud_root.canonicalize().expect("canonical cloud");
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
        // C8-11 guard-rail: a burst of provider-backed uploads must
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
        // C8-38/C8-41: with a tiny measured link capacity the shaper
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
        // C8-37/C8-41: the AlwaysIdle notifier + IdleDrain inputs engage
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
            boosted.caps.planner_workers
                > vapor_shared::constants::engine::IDLE_DRAIN_PLANNER_WORKERS,
            "boost must raise caps above the base ({} <= {})",
            boosted.caps.planner_workers,
            vapor_shared::constants::engine::IDLE_DRAIN_PLANNER_WORKERS
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
            snapped.caps.planner_workers
                <= vapor_shared::constants::engine::IDLE_DRAIN_PLANNER_WORKERS,
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
        let watch_root = watch_root.canonicalize().expect("canonical watch root");
        let database_path = temp.path().join("state/vapor.sqlite");
        let local_file = watch_root.join("durable.txt");
        std::fs::write(&local_file, b"survives restarts").expect("seed local");

        let sync_scope = || SyncScope {
            local_sync_directory: Some(watch_root.clone()),
            cloud_sync_directory: cloud_root.to_string_lossy().into_owned(),
            sync_mode: vapor_shared::SyncMode::TwoWay,
        };

        // First daemon: lease the intent into flight, then "crash"
        // (drop) before completing it.
        {
            let mut state_db = DurableStateDb::open(&database_path).expect("open durable state db");
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
            clock.advance(Duration::from_millis(250));
            let report = runtime
                .tick_with_inputs(timestamp_ms(250), ThrottleInputs::default())
                .expect("tick");
            assert_eq!(report.leased_intents, 1, "intent must be in flight");
            assert_eq!(runtime.state_db().leased_depth().expect("leased"), 1);
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
            default_provider(),
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
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut state_db = DurableStateDb::open(&database_path).expect("open durable state db");
        let subtree_root = watch_root.join("project");
        state_db
            .enqueue_intent(
                &subtree_root,
                PendingIntentKind::ReconcileSubtree,
                timestamp_ms(0),
            )
            .expect("enqueue reconcile intent");

        let clock = Arc::new(crate::clock::ManualClock::at_now());
        let mut runtime = DaemonRuntime::build(
            test_sync_scope(&watch_root),
            EventPathFilterOptions::default(),
            state_db,
            default_provider(),
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
        assert_eq!(paused.requeued_intents, 1);
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
        // C2-1 invariant: the runtime's public `tick(now)` path must obtain
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
            default_provider(),
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
        // IdleDrain — the existing behavior the C2-1 plumbing must preserve.
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        std::fs::create_dir_all(&watch_root).expect("create watch root");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let state_db = DurableStateDb::open(&database_path).expect("open durable state db");

        let mut runtime =
            DaemonRuntime::start(test_sync_scope(&watch_root), state_db, default_provider())
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
        // Regression test for the Wave 7 status-stuck bug: without the
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
            default_provider(),
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

    // ---- Optional advanced safeguards (C8-55..C8-57) ----

    #[test]
    fn mass_deletion_storm_pauses_daemon_raises_alert_and_resume_rearms() {
        let mut fixture = BidirectionalFixture::new();
        let timeline = crate::timeline::TimelineBuffer::new(256);
        fixture.runtime.attach_timeline(timeline.clone());
        let control = Arc::new(crate::runtime_control::RuntimeControl::new());
        fixture.runtime.attach_control(control.clone());
        fixture.tick(6_000); // baseline

        // A deletion storm: threshold-many distinct paths removed inside
        // one window. Spread across sibling directories so the storm
        // compactor does not swallow them before they stabilize.
        for index in 0..constants::engine::MASS_DELETE_THRESHOLD {
            let path = fixture
                .watch_root
                .join(format!("dir-{}", index % 40))
                .join(format!("victim-{index}.bin"));
            fixture.record_local_event(&path, FsEventKind::Removed, fixture.now_ms);
        }
        fixture.tick(6_000); // stabilize the burst

        assert_eq!(
            fixture.runtime.app.snapshot().run_state,
            RunState::Paused,
            "guard must pause on a mass-deletion storm"
        );
        assert!(
            fixture
                .runtime
                .app
                .snapshot()
                .reason
                .contains("vapor resume"),
            "pause reason must tell the user the way out"
        );
        assert!(
            timeline
                .snapshot(None)
                .iter()
                .any(|entry| entry.kind == "guard"),
            "guard trip must land on the activity timeline"
        );

        // Explicit resume re-arms the guard with an empty window: the
        // daemon runs again and a single further delete does not re-trip.
        control.request_resume();
        fixture.tick(1_000);
        assert_eq!(fixture.runtime.app.snapshot().run_state, RunState::Running);
        let lone = fixture.watch_root.join("post-resume.bin");
        fixture.record_local_event(&lone, FsEventKind::Removed, fixture.now_ms);
        fixture.tick(6_000);
        assert_eq!(
            fixture.runtime.app.snapshot().run_state,
            RunState::Running,
            "one delete after resume is normal use, not a storm"
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
