use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use vapor_providers::Provider;
use vapor_shared::{RunState, constants};

use crate::debounce::DebounceLoop;
use crate::event_intents::BoundedFsEventRecorder;
use crate::executor::{StagedExecutor, StagedExecutorSnapshot};
use crate::fs_events::{FsEventsWatcher, FsEventsWatcherError, normalize_watch_root};
use crate::logging;
use crate::scheduler::KeyedSupersedingScheduler;
use crate::state_db::{DurableIntentRecord, DurableStateDb, StateDbError};
use crate::sync_directories::SyncScope;
use crate::throttle::ThrottleInputs;
use crate::{DaemonApp, event_intents::PendingIntentKind};

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

impl From<StateDbError> for DaemonRuntimeError {
    fn from(value: StateDbError) -> Self {
        Self::StateDb(value)
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
    last_throttle_sample_at: Option<SystemTime>,
    running_reconcile_intent_id: Option<i64>,
    startup_reconstruction_barrier: bool,
    startup_barrier_expires_at: Option<SystemTime>,
    tick_interval: Duration,
    throttle_sample_interval: Duration,
    startup_barrier_deadline: Duration,
}

impl DaemonRuntime {
    pub fn start(
        sync_scope: SyncScope,
        state_db: DurableStateDb,
        provider: Box<dyn Provider>,
    ) -> Result<Self, DaemonRuntimeError> {
        let mut runtime = Self::build(sync_scope, state_db, provider, true)?;
        runtime.enqueue_startup_reconstruction_reconcile(SystemTime::now())?;
        Ok(runtime)
    }

    pub fn run_forever(&mut self) -> Result<(), DaemonRuntimeError> {
        loop {
            self.tick(SystemTime::now())?;
            thread::sleep(self.tick_interval);
        }
    }

    pub fn tick(&mut self, now: SystemTime) -> Result<RuntimeTickReport, DaemonRuntimeError> {
        self.tick_with_inputs(now, ThrottleInputs::default())
    }

    pub fn tick_with_inputs(
        &mut self,
        now: SystemTime,
        throttle_inputs: ThrottleInputs,
    ) -> Result<RuntimeTickReport, DaemonRuntimeError> {
        self.sample_throttle_inputs(now, throttle_inputs);

        let mut report = RuntimeTickReport {
            released_deferred_reconciles: self.release_ready_deferred_reconciles(now),
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
                    self.startup_barrier_expires_at = None;
                }
                report.completed_intents += 1;
                report.completed_reconcile_root = Some(completed_root);
            }
        }

        self.process_ready_queue(now, &mut report)?;

        report.staged_executor = self.staged_executor.snapshot();

        Ok(report)
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
        mut state_db: DurableStateDb,
        provider: Box<dyn Provider>,
        start_watcher: bool,
    ) -> Result<Self, DaemonRuntimeError> {
        let now = SystemTime::now();
        let mut app = DaemonApp::new(provider);
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

        let recorder = sync_scope
            .local_sync_directory
            .as_ref()
            .map(|watch_root| Arc::new(BoundedFsEventRecorder::new(watch_root.clone())));
        let watcher = if start_watcher {
            match (&sync_scope.local_sync_directory, &recorder) {
                (Some(watch_root), Some(recorder)) => Some(FsEventsWatcher::start(
                    watch_root.clone(),
                    recorder.clone(),
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

        let debounce = DebounceLoop::default();
        let tick_interval = debounce.tick_interval();
        Ok(Self {
            app,
            sync_scope,
            state_db,
            recorder,
            debounce,
            staged_executor: StagedExecutor::new(tick_interval),
            scheduler: KeyedSupersedingScheduler::default(),
            watcher,
            last_throttle_sample_at: None,
            running_reconcile_intent_id: None,
            startup_reconstruction_barrier: false,
            startup_barrier_expires_at: None,
            tick_interval,
            throttle_sample_interval: Duration::from_millis(
                constants::engine::THROTTLE_SAMPLE_INTERVAL_MILLIS,
            ),
            startup_barrier_deadline: Duration::from_millis(
                constants::engine::STARTUP_RECONSTRUCTION_BARRIER_DEADLINE_MILLIS,
            ),
        })
    }

    fn sample_throttle_inputs(&mut self, now: SystemTime, inputs: ThrottleInputs) {
        let should_sample = self
            .last_throttle_sample_at
            .map(|last| {
                now.duration_since(last)
                    .map(|elapsed| elapsed >= self.throttle_sample_interval)
                    .unwrap_or(true)
            })
            .unwrap_or(true);
        if should_sample {
            self.app.apply_throttle_inputs(inputs);
            self.last_throttle_sample_at = Some(now);
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
        self.startup_barrier_expires_at = Some(now + self.startup_barrier_deadline);
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

    fn evaluate_startup_barrier(&mut self, now: SystemTime) {
        if !self.startup_reconstruction_barrier {
            return;
        }

        let Some(expires_at) = self.startup_barrier_expires_at else {
            return;
        };

        if now < expires_at {
            return;
        }

        self.startup_reconstruction_barrier = false;
        self.startup_barrier_expires_at = None;
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
        let mut enqueued = 0;
        while let Some(intent) = self.scheduler.claim_next() {
            self.state_db
                .enqueue_intent(&intent.path, intent.kind, intent.last_observed_at)?;
            enqueued += 1;
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
                                now + self.tick_interval,
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
                                now + self.tick_interval,
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
                            now + self.tick_interval,
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
                            now + self.tick_interval,
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
            return Err(StateDbError::InvalidIntentState(format!(
                "intent {intent_id} was not leased during runtime requeue"
            ))
            .into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs_events::{FsEventKind, FsEventRecord, FsEventRecording};
    use crate::sync_directories::SyncScope;
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
        let mut runtime = DaemonRuntime::build(
            test_sync_scope(&watch_root),
            state_db,
            default_provider(),
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

        let first_tick = runtime
            .tick_with_inputs(timestamp_ms(1_500), ThrottleInputs::default())
            .expect("runtime tick");
        let second_tick = runtime
            .tick_with_inputs(timestamp_ms(1_750), ThrottleInputs::default())
            .expect("runtime tick");
        let third_tick = runtime
            .tick_with_inputs(timestamp_ms(2_000), ThrottleInputs::default())
            .expect("runtime tick");
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
            state_db,
            default_provider(),
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
            state_db,
            default_provider(),
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
        assert!(
            elapsed < Duration::from_secs(3),
            "composed runtime tick took {:?}, expected < 3s",
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
        let mut runtime = DaemonRuntime::build(
            test_sync_scope(&watch_root),
            state_db,
            default_provider(),
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

        let started = runtime
            .tick_with_inputs(timestamp_ms(32_000), ThrottleInputs::default())
            .expect("runtime tick");
        assert_eq!(started.released_deferred_reconciles, 1);
        assert!(started.started_reconcile_root.is_some());
        assert_eq!(runtime.state_db().leased_depth().expect("leased depth"), 1);

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

    fn timestamp_ms(milliseconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(milliseconds)
    }
}
