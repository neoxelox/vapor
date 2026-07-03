use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use vapor_providers::Provider;
use vapor_shared::{RunState, constants};

use crate::clock::{SharedClock, system_clock};
use crate::debounce::{DebounceLoop, DebounceWindows};
use crate::event_intents::BoundedFsEventRecorder;
use crate::executor::{StagedExecutor, StagedExecutorSnapshot};
use crate::fs_events::{
    FsEventErrorRecord, FsEventRecord, FsEventRecording, FsEventsWatcher, FsEventsWatcherError,
    normalize_watch_root,
};
use crate::ipc_service::{DaemonStatusSnapshot, StatusPublisher};
use crate::logging;
use crate::metrics::{MetricsSampler, StaticMetricsSampler};
use crate::path_filter::EventPathFilterOptions;
use crate::scheduler::KeyedSupersedingScheduler;
use crate::state_db::{DurableIntentRecord, DurableStateDb, StateDbError};
use crate::sync_directories::SyncScope;
use crate::throttle::ThrottleInputs;
use crate::{DaemonApp, event_intents::PendingIntentKind};

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
    pub durable_enqueues: usize,
    pub leased_intents: usize,
    pub started_staged_intents: usize,
    pub completed_intents: usize,
    pub requeued_intents: usize,
    pub started_reconcile_root: Option<PathBuf>,
    pub completed_reconcile_root: Option<PathBuf>,
    pub staged_executor: StagedExecutorSnapshot,
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
            || report.requeued_intents > 0;
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

        // Pause semantics ("stops admitting new work"): ingest, debounce,
        // and the durable flush keep running so intent state is never
        // lost, and work already in flight runs to completion — but no
        // new work is released or leased while paused.
        let paused = self.app.snapshot().run_state == RunState::Paused;

        let mut report = RuntimeTickReport {
            released_deferred_reconciles: if paused {
                0
            } else {
                self.release_ready_deferred_reconciles(now)
            },
            drained_pending_intents: self.drain_pending_intents(),
            stabilized_events: self.stabilize_events(now),
            durable_enqueues: self.flush_scheduler_to_durable_queue()?,
            ..RuntimeTickReport::default()
        };

        let staged_report = self
            .staged_executor
            .advance(&mut self.app, &mut self.state_db, now)?;
        report.completed_intents += staged_report.completed;

        if let Some(reconcile_intent_id) = self.running_reconcile_intent_id {
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
            } else if let Some(completed_root) = self.complete_running_reconcile()? {
                self.state_db.complete_leased(reconcile_intent_id)?;
                self.running_reconcile_intent_id = None;
                if self.startup_reconstruction_barrier
                    && self.sync_scope.local_sync_directory.as_ref() == Some(&completed_root)
                {
                    self.startup_reconstruction_barrier = false;
                    self.startup_barrier_expires_inst = None;
                }
                report.completed_intents += 1;
                report.completed_reconcile_root = Some(completed_root);
            }
        }

        if !paused {
            self.process_ready_queue(now, &mut report)?;
        }

        report.staged_executor = self.staged_executor.snapshot();

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

    fn build(
        mut sync_scope: SyncScope,
        filter_options: EventPathFilterOptions,
        mut state_db: DurableStateDb,
        provider: Box<dyn Provider>,
        metrics_sampler: Arc<dyn MetricsSampler>,
        clock: SharedClock,
        start_watcher: bool,
    ) -> Result<Self, DaemonRuntimeError> {
        let now = clock.now_system();
        let mut app = DaemonApp::new_with_clock(provider, clock.clone());
        let recovered_count = state_db.recover_leased(now)?;
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

        app.ensure_cloud_sync_directory(sync_scope.cloud_sync_directory.as_str());

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

        if let Some(local_sync_directory) = &sync_scope.local_sync_directory {
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
            staged_executor: StagedExecutor::with_clock(tick_interval, clock.clone()),
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
        })
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
        let (pause_request, reconcile_request) = match self.runtime_control.as_ref() {
            Some(control) => {
                let pause = control.take_pause_request();
                let reconcile = control.take_reconcile_request();
                let _ = control.take_flush_request();
                (pause, reconcile)
            }
            None => (None, false),
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
            }
        }
        if reconcile_request {
            self.enqueue_startup_reconstruction_reconcile(now)?;
        }
        Ok(())
    }

    fn sample_throttle_inputs(&mut self, _now: SystemTime, inputs: ThrottleInputs) {
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

        let app = &mut self.app;
        let scheduler = &mut self.scheduler;
        recorder.with_mut_state(|maps| app.release_ready_deferred_reconciles(maps, scheduler, now))
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

    fn stabilize_events(&mut self, now: SystemTime) -> usize {
        let Some(recorder) = &self.recorder else {
            return 0;
        };

        let Some(watch_root) = self.sync_scope.local_sync_directory.as_ref() else {
            return 0;
        };

        let stabilized = self.debounce.run_tick_for_recorder(recorder, now);
        let mut accepted = 0;
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
            self.scheduler.upsert_stabilized_event(event);
            accepted += 1;
        }
        accepted
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
        FsEventRecording::record_event(
            recorder.as_ref(),
            FsEventRecord {
                path: runtime_watch_root.join("src/main.rs"),
                kind: FsEventKind::Modified,
                observed_at: timestamp_ms(0),
            },
        );

        // Each scripted tick advances the monotonic clock by 250 ms so the
        // debounce / staged-executor elapsed-time gates fire deterministically.
        // The SystemTime arg keeps documenting the durable wall-clock value
        // recorded in the state DB.
        clock.advance(Duration::from_millis(1_500));
        let first_tick = runtime
            .tick_with_inputs(timestamp_ms(1_500), ThrottleInputs::default())
            .expect("runtime tick");
        clock.advance(Duration::from_millis(250));
        let second_tick = runtime
            .tick_with_inputs(timestamp_ms(1_750), ThrottleInputs::default())
            .expect("runtime tick");
        clock.advance(Duration::from_millis(250));
        let third_tick = runtime
            .tick_with_inputs(timestamp_ms(2_000), ThrottleInputs::default())
            .expect("runtime tick");
        clock.advance(Duration::from_millis(250));
        let fourth_tick = runtime
            .tick_with_inputs(timestamp_ms(2_250), ThrottleInputs::default())
            .expect("runtime tick");

        assert_eq!(first_tick.stabilized_events, 1);
        assert_eq!(first_tick.durable_enqueues, 1);
        assert_eq!(first_tick.started_staged_intents, 1);
        assert_eq!(first_tick.completed_intents, 0);
        assert_eq!(second_tick.staged_executor.hash_running, 1);
        assert_eq!(third_tick.staged_executor.upload_running, 1);
        assert_eq!(fourth_tick.completed_intents, 1);
        assert_eq!(runtime.state_db().queue_depth().expect("queue depth"), 0);
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
        }
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
}
