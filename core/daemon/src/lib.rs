#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use vapor_providers::{Provider, inert_stub_provider};
use vapor_shared::{RunState, StatusSnapshot, ThrottleState};

use crate::clock::{SharedClock, SystemClock};
use crate::event_intents::BoundedEventIntentMaps;
use crate::reconcile::{ReconcileCompletion, ReconcileController, ReconcilePause};
use crate::retry::{RetryDecision, RetryFailureKind};
use crate::scheduler::KeyedSupersedingScheduler;
use crate::state_db::{
    DurableFailedIntentRecord, DurableStateDb, ScheduledRetryRecord, StateDbError,
};
use crate::throttle::{ThrottleCaps, ThrottleController, ThrottleDecision, ThrottleInputs};
use crate::workgate::{
    ThrottleWorkgate, WorkClass, WorkPermit, WorkPermitDenied, WorkgateSnapshot,
};

pub mod build_info {
    include!(concat!(env!("OUT_DIR"), "/vapor_build_info.rs"));
}

pub mod bandwidth;
pub mod bootstrap;
pub mod clock;
pub mod config_reload;
pub mod conflict;
pub mod debounce;
pub mod event_intents;
pub mod executor;
pub mod fs_events;
pub mod ipc_server;
pub mod ipc_service;
pub(crate) mod local_hash;
pub mod logging;
pub mod metrics;
pub mod multi_runtime;
pub mod name_collision;
pub mod one_way;
pub mod path_filter;
pub mod profiles;
pub(crate) mod provider_jobs;
pub mod reconcile;
pub mod reconcile_walk;
pub mod remote_sync;
pub mod resource_budget;
pub mod retry;
pub mod root_identity;
pub mod runtime;
pub mod runtime_control;
pub mod safeguards;
pub mod scheduler;
pub mod self_write_cache;
pub mod singleton;
pub mod state_db;
pub mod storm;
pub mod sync_directories;
pub mod throttle;
pub mod timeline;
pub mod trash;
pub mod type_mismatch;
pub mod unsyncable;
pub mod workgate;

pub struct DaemonApp {
    snapshot: StatusSnapshot,
    /// `Arc` so provider-job workers can drive uploads/downloads off
    /// the tick thread while the app keeps trait-object access.
    provider: Arc<dyn Provider>,
    throttle_controller: ThrottleController,
    last_throttle_decision: Option<ThrottleDecision>,
    retry_slowdown_until: Option<SystemTime>,
    /// Effective CPU ceiling from the resource budget. `None`
    /// until the budget runtime publishes; caps then scale relative to
    /// the default ceiling.
    resource_cpu_ceiling_percent: Option<u8>,
    /// Explicit user ceiling on concurrent uploads / downloads
    /// (`resourceLimits.concurrentTransfers`); `None` = automatic
    /// (the core-derived throttle tier decides).
    concurrent_transfers: Option<usize>,
    reconcile_controller: ReconcileController,
    /// Shared behind a mutex so multiple profile runtimes gate against
    /// ONE daemon-level cap set (`data-flow.md §Multi-profile watch
    /// coordination`); a single-profile daemon simply owns the only
    /// clone.
    workgate: Arc<Mutex<ThrottleWorkgate>>,
    /// Holds the daemon's own CPU under the ceiling by measurement.
    cpu_regulator: crate::throttle::CpuRegulator,
    /// The last poll permission that was logged, so the line is
    /// written when the answer changes and not on every tick.
    logged_poll_permission: std::cell::Cell<Option<bool>>,
}

impl Default for DaemonApp {
    fn default() -> Self {
        Self::new(inert_stub_provider())
    }
}

impl std::fmt::Debug for DaemonApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonApp")
            .field("snapshot", &self.snapshot)
            .field("provider", &self.provider.name())
            .field("last_throttle_decision", &self.last_throttle_decision)
            .field("retry_slowdown_until", &self.retry_slowdown_until)
            .field("reconcile_controller", &self.reconcile_controller)
            .finish()
    }
}

impl DaemonApp {
    pub fn new(provider: Box<dyn Provider>) -> Self {
        Self::new_with_clock(provider, Arc::new(SystemClock))
    }

    pub fn new_with_clock(provider: Box<dyn Provider>, clock: SharedClock) -> Self {
        let snapshot = StatusSnapshot::default();
        let initial_throttle_state = snapshot.throttle_state;
        let throttle_controller = ThrottleController::with_clock(clock.clone());
        let throttle_caps = throttle_controller.caps_for(initial_throttle_state);
        Self::new_with_shared_workgate(
            provider,
            clock,
            Arc::new(Mutex::new(ThrottleWorkgate::new(
                initial_throttle_state,
                throttle_caps,
            ))),
        )
    }

    /// Composes an app around a shared daemon-level workgate (the
    /// multi-profile runtime hands every profile the same instance).
    pub fn new_with_shared_workgate(
        provider: Box<dyn Provider>,
        clock: SharedClock,
        workgate: Arc<Mutex<ThrottleWorkgate>>,
    ) -> Self {
        logging::info("Initialized daemon app state", &[]);
        Self {
            snapshot: StatusSnapshot::default(),
            provider: Arc::from(provider),
            throttle_controller: ThrottleController::with_clock(clock.clone()),
            last_throttle_decision: None,
            retry_slowdown_until: None,
            resource_cpu_ceiling_percent: None,
            concurrent_transfers: None,
            reconcile_controller: ReconcileController::with_clock(clock),
            workgate,
            cpu_regulator: crate::throttle::CpuRegulator::default(),
            logged_poll_permission: std::cell::Cell::new(None),
        }
    }

    /// Applies the effective CPU ceiling from the resource budget
    ///: concurrency caps scale proportionally to the ceiling
    /// relative to the default `resourceLimits.cpuPercent`. Interacts
    /// with throttle caps via MIN semantics: a `Suspended` zero cap
    /// stays zero under any ceiling, and lowering the ceiling lowers
    /// caps for in-flight admission immediately (running work yields at
    /// its next slice checkpoint).
    pub fn apply_resource_cpu_ceiling(&mut self, ceiling_percent: u8, now: SystemTime) {
        if self.resource_cpu_ceiling_percent == Some(ceiling_percent) {
            return;
        }
        self.resource_cpu_ceiling_percent = Some(ceiling_percent);
        self.refresh_workgate_caps(now);
    }

    /// Applies the explicit `resourceLimits.concurrentTransfers`
    /// ceiling (already clamped by the budget resolve). A hard user
    /// limit: it caps every throttle tier, including idle boost.
    pub fn set_concurrent_transfers(&mut self, ceiling: Option<usize>) {
        if self.concurrent_transfers == ceiling {
            return;
        }
        self.concurrent_transfers = ceiling;
        self.refresh_workgate_caps(SystemTime::now());
    }

    fn lock_workgate(&self) -> std::sync::MutexGuard<'_, ThrottleWorkgate> {
        // Poison recovery: a panicking profile must not wedge the
        // shared workgate for its healthy siblings. Counts are
        // rebuilt by the next reconfigure; a leaked permit slot is
        // bounded and self-corrects as caps refresh.
        self.workgate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn snapshot(&self) -> &StatusSnapshot {
        &self.snapshot
    }

    pub fn provider_name(&self) -> &'static str {
        self.provider.name()
    }

    /// The injected provider. The staged executor and the remote poller
    /// drive uploads / downloads / deletes / change polls through this
    /// trait boundary: engine code never names a concrete provider.
    /// Shared handle for provider calls that run on another thread.
    pub fn provider_handle(&self) -> Arc<dyn Provider> {
        self.provider.clone()
    }

    pub fn provider(&self) -> &dyn Provider {
        self.provider.as_ref()
    }

    /// Shared handle for provider-job dispatch (workers hold the
    /// provider across ticks).
    pub(crate) fn provider_arc(&self) -> Arc<dyn Provider> {
        self.provider.clone()
    }

    #[cfg(test)]
    pub(crate) fn replace_provider_for_testing(&mut self, provider: Box<dyn Provider>) {
        self.provider = Arc::from(provider);
    }

    pub fn throttle_decision(&self) -> Option<&ThrottleDecision> {
        self.last_throttle_decision.as_ref()
    }

    pub fn retry_slowdown_until(&self) -> Option<SystemTime> {
        self.retry_slowdown_until
    }

    pub fn set_run_state(&mut self, run_state: RunState, reason: impl Into<String>) {
        let reason = reason.into();
        logging::info(
            "Updated run state",
            &[
                ("run_state", format!("{:?}", run_state)),
                ("reason", reason.clone()),
            ],
        );
        self.snapshot.run_state = run_state;
        self.snapshot.reason = reason;
    }

    pub fn set_throttle_state(&mut self, throttle_state: ThrottleState, reason: impl Into<String>) {
        let reason = reason.into();
        if self.snapshot.throttle_state == throttle_state && self.snapshot.throttle_reason == reason
        {
            return;
        }

        // Routine transitions are INFO; Suspended stops uploads and
        // hashing outright, which is worth a WARNING. A reason that
        // changes while the state stays (the CPU load moves from 69%
        // to 71%) is no transition: it would be a line a second.
        let fields = [
            ("throttle_state", format!("{:?}", throttle_state)),
            ("reason", reason.clone()),
        ];
        if self.snapshot.throttle_state == throttle_state {
            logging::debug("Updated throttle reason", &fields);
        } else if throttle_state == ThrottleState::Suspended {
            logging::warning("Updated throttle state", &fields);
        } else {
            logging::info("Updated throttle state", &fields);
        }
        self.snapshot.throttle_state = throttle_state;
        self.snapshot.throttle_reason = reason;
        self.refresh_workgate_caps(SystemTime::now());
    }

    pub fn apply_throttle_inputs(&mut self, inputs: ThrottleInputs) -> ThrottleDecision {
        // The CPU ceiling is a measured limit: regulate first, so the
        // controller knows whether regulation still has room.
        let ceiling = self.cpu_ceiling_percent();
        let widest_cap = self.widest_unregulated_cap(SystemTime::now());
        let before = self.cpu_regulator;
        self.cpu_regulator
            .observe(inputs.vapor_cpu_load_percent, ceiling, widest_cap);
        let beyond_regulation =
            inputs.vapor_cpu_load_percent > ceiling && self.cpu_regulator.exhausted(widest_cap);
        self.throttle_controller
            .set_vapor_cpu_ceiling_exceeded(beyond_regulation.then_some(ceiling));
        if before != self.cpu_regulator {
            logging::debug(
                "Adjusted concurrency to hold the CPU ceiling",
                &[
                    (
                        "vapor_cpu_percent",
                        inputs.vapor_cpu_load_percent.to_string(),
                    ),
                    ("ceiling_percent", ceiling.to_string()),
                    (
                        "workers_removed",
                        self.cpu_regulator.steps_down().to_string(),
                    ),
                ],
            );
        }

        let decision = self.throttle_controller.evaluate(inputs);
        let caps_changed = self
            .last_throttle_decision
            .as_ref()
            .is_none_or(|last| last.caps != decision.caps);
        self.last_throttle_decision = Some(decision.clone());
        self.set_throttle_state(decision.state, decision.reason.clone());
        if caps_changed || before != self.cpu_regulator {
            self.refresh_workgate_caps(SystemTime::now());
        }
        decision
    }

    /// The CPU share the daemon holds itself to: the resource budget's
    /// effective ceiling (idle boost included), or the default.
    fn cpu_ceiling_percent(&self) -> u8 {
        self.resource_cpu_ceiling_percent
            .unwrap_or(vapor_shared::constants::resource_limits::DEFAULT_CPU_PERCENT)
    }

    /// What the link carries in its slower direction, as the daemon
    /// knows it now; the throttle is gentler on a slow link.
    pub fn set_link_capacity_kbps(&mut self, kbps: Option<u32>) {
        self.throttle_controller.set_link_capacity_kbps(kbps);
    }

    /// How many workers the CPU regulator has taken from each cap.
    pub fn cpu_regulation_steps(&self) -> usize {
        self.cpu_regulator.steps_down()
    }

    pub fn throttle_caps(&self) -> ThrottleCaps {
        self.effective_throttle_caps(SystemTime::now())
    }

    pub fn workgate_snapshot(&self) -> WorkgateSnapshot {
        self.lock_workgate().snapshot()
    }

    pub fn try_acquire_work(&mut self, class: WorkClass) -> Result<WorkPermit, WorkPermitDenied> {
        self.refresh_workgate_caps(SystemTime::now());
        self.lock_workgate().try_acquire(class)
    }

    pub fn release_work(&mut self, permit: WorkPermit) -> bool {
        self.lock_workgate().release(permit)
    }

    pub fn release_ready_deferred_reconciles(
        &mut self,
        maps: &mut BoundedEventIntentMaps,
        scheduler: &mut KeyedSupersedingScheduler,
        now: SystemTime,
    ) -> usize {
        self.reconcile_controller.release_ready_deferred_reconciles(
            maps,
            scheduler,
            self.snapshot.throttle_state,
            now,
        )
    }

    pub fn try_start_reconcile(
        &mut self,
        scheduler: &mut KeyedSupersedingScheduler,
        now: SystemTime,
    ) -> Result<Option<std::path::PathBuf>, WorkPermitDenied> {
        let workgate = self.workgate.clone();
        let mut workgate = workgate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.reconcile_controller.try_start_next(
            scheduler,
            &mut workgate,
            self.snapshot.throttle_state,
            now,
        )
    }

    pub fn checkpoint_reconcile(
        &mut self,
        scheduler: &mut KeyedSupersedingScheduler,
        now: SystemTime,
    ) -> Option<ReconcilePause> {
        let workgate = self.workgate.clone();
        let mut workgate = workgate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.reconcile_controller.checkpoint(
            scheduler,
            &mut workgate,
            self.snapshot.throttle_state,
            now,
        )
    }

    /// Root of the reconcile currently holding the reconcile permit.
    pub fn running_reconcile_root(&self) -> Option<std::path::PathBuf> {
        self.reconcile_controller.running_root().cloned()
    }

    /// Whether the current throttle state lets a reconcile start.
    pub fn reconcile_admissible(&self) -> bool {
        ReconcileController::admits(self.snapshot.throttle_state)
    }

    /// Aborts the running reconcile after a comparison-walk failure.
    pub fn abort_reconcile(
        &mut self,
        scheduler: &mut KeyedSupersedingScheduler,
        now: SystemTime,
    ) -> Option<std::path::PathBuf> {
        let workgate = self.workgate.clone();
        let mut workgate = workgate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.reconcile_controller
            .abort_running(scheduler, &mut workgate, now)
    }

    pub fn complete_reconcile(
        &mut self,
        maps: &mut BoundedEventIntentMaps,
        scheduler: &mut KeyedSupersedingScheduler,
    ) -> Option<ReconcileCompletion> {
        let workgate = self.workgate.clone();
        let mut workgate = workgate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.reconcile_controller
            .complete_success(maps, scheduler, &mut workgate)
    }

    pub fn schedule_retry(
        &mut self,
        state_db: &mut DurableStateDb,
        id: i64,
        failure_kind: RetryFailureKind,
        last_error: &str,
        now: SystemTime,
    ) -> Result<ScheduledRetryRecord, StateDbError> {
        let scheduled = state_db
            .schedule_retry(id, failure_kind, last_error, now)?
            .ok_or_else(|| {
                StateDbError::InvalidIntentState(format!(
                    "retry scheduling for intent {id} produced no retry record"
                ))
            })?;
        self.apply_retry_decision(&scheduled.decision, now);
        Ok(scheduled)
    }

    pub fn restore_retry_slowdown(
        &mut self,
        state_db: &mut DurableStateDb,
        now: SystemTime,
    ) -> Result<Option<SystemTime>, StateDbError> {
        self.retry_slowdown_until = state_db.take_active_retry_slowdown_until(now)?;
        self.refresh_workgate_caps(now);
        Ok(self.retry_slowdown_until)
    }

    pub fn finalize_failure(
        &mut self,
        state_db: &mut DurableStateDb,
        id: i64,
        failure_kind: RetryFailureKind,
        last_error: &str,
        now: SystemTime,
    ) -> Result<DurableFailedIntentRecord, StateDbError> {
        state_db.finalize_leased_failure(id, failure_kind, last_error, now)
    }

    pub fn remote_poll_allowed(&self) -> bool {
        let allowed = self.provider.poll_allowed(self.snapshot.throttle_state);
        if self.logged_poll_permission.replace(Some(allowed)) != Some(allowed) {
            logging::debug(
                "Remote polling permission changed",
                &[
                    (
                        "throttle_state",
                        format!("{:?}", self.snapshot.throttle_state),
                    ),
                    ("allowed", allowed.to_string()),
                ],
            );
        }
        allowed
    }

    /// Ensures the provider-side sync root exists. Returns the
    /// actionable error when it cannot; the runtime blocks regular sync
    /// work until a later attempt succeeds: intents keep
    /// accumulating durably, they are never dropped.
    pub fn ensure_cloud_root(
        &self,
        cloud_root: &str,
    ) -> Result<(), vapor_providers::ProviderError> {
        match self.provider.ensure_cloud_root(cloud_root) {
            Ok(_) => {
                logging::info(
                    "Cloud root is ready",
                    &[("cloud_root", cloud_root.to_string())],
                );
                Ok(())
            }
            Err(error) => {
                logging::error(
                    "Failed to ensure cloud root",
                    &[
                        ("cloud_root", cloud_root.to_string()),
                        ("failure_kind", error.kind.label().to_string()),
                        ("error", error.message.clone()),
                    ],
                );
                Err(error)
            }
        }
    }

    /// The caps of the current state. The last decision carries what
    /// the inputs added to them (a full disk holds downloads).
    fn base_throttle_caps(&self) -> ThrottleCaps {
        match &self.last_throttle_decision {
            Some(decision) if decision.state == self.snapshot.throttle_state => decision.caps,
            _ => self
                .throttle_controller
                .caps_for(self.snapshot.throttle_state),
        }
    }

    /// The largest concurrency cap before the CPU regulator applies.
    fn widest_unregulated_cap(&self, now: SystemTime) -> usize {
        let caps = self.unregulated_throttle_caps(now);
        caps.planner_workers
            .max(caps.hash_workers)
            .max(caps.upload_concurrency)
            .max(caps.download_concurrency)
    }

    fn effective_throttle_caps(&self, now: SystemTime) -> ThrottleCaps {
        let mut caps = self.unregulated_throttle_caps(now);
        caps.planner_workers = self.cpu_regulator.apply(caps.planner_workers);
        caps.hash_workers = self.cpu_regulator.apply(caps.hash_workers);
        caps.read_tokens = self.cpu_regulator.apply(caps.read_tokens);
        caps.upload_concurrency = self.cpu_regulator.apply(caps.upload_concurrency);
        caps.download_concurrency = self.cpu_regulator.apply(caps.download_concurrency);
        caps
    }

    fn unregulated_throttle_caps(&self, now: SystemTime) -> ThrottleCaps {
        let mut caps = self.base_throttle_caps();
        let is_idle = self.snapshot.throttle_state == vapor_shared::ThrottleState::Idle;
        if let Some(ceiling) = self.resource_cpu_ceiling_percent {
            let scale = f64::from(ceiling)
                / f64::from(vapor_shared::constants::resource_limits::DEFAULT_CPU_PERCENT);
            let scale_cap = |cap: usize| -> usize {
                if cap == 0 {
                    // A throttle-imposed zero (Suspended) is never
                    // relaxed by a user ceiling.
                    return 0;
                }
                let scaled = (((cap as f64) * scale).round() as usize).max(1);
                // Ceilings only ever *lower* what the throttle ladder
                // allows, except under Idle, where idle boost is
                // defined. Outside Idle a raised base budget must not
                // lift the Light/Throttled tiers above their compiled
                // values (that would defeat the throttle ladder in exactly
                // the states where it matters most).
                if is_idle { scaled } else { scaled.min(cap) }
            };
            caps.planner_workers = scale_cap(caps.planner_workers);
            caps.hash_workers = scale_cap(caps.hash_workers);
            caps.read_tokens = scale_cap(caps.read_tokens);
            caps.upload_concurrency = scale_cap(caps.upload_concurrency);
            caps.download_concurrency = scale_cap(caps.download_concurrency);
        }
        if let Some(ceiling) = self.concurrent_transfers {
            // A hard user ceiling: applied last so neither the CPU
            // scale nor idle boost can exceed it. A throttle-imposed
            // zero (Suspended) stays zero.
            if caps.upload_concurrency > 0 {
                caps.upload_concurrency = caps.upload_concurrency.min(ceiling);
            }
            if caps.download_concurrency > 0 {
                caps.download_concurrency = caps.download_concurrency.min(ceiling);
            }
        }
        // The rate-limit slowdown: the provider asked for fewer requests,
        // and it counts every kind of request against one quota, so
        // each lane that calls the provider (uploads, downloads, the
        // planner's probes) narrows to one. Applied after scaling, so
        // no ceiling factor multiplies the clamp back up.
        if self.retry_slowdown_active(now) {
            caps.upload_concurrency = caps.upload_concurrency.min(1);
            caps.download_concurrency = caps.download_concurrency.min(1);
            caps.planner_workers = caps.planner_workers.min(1);
        }
        caps
    }

    fn retry_slowdown_active(&self, now: SystemTime) -> bool {
        self.retry_slowdown_until
            .map(|until| until > now)
            .unwrap_or(false)
    }

    fn apply_retry_decision(&mut self, decision: &RetryDecision, now: SystemTime) {
        if let Some(slowdown_until) = decision.slowdown_until {
            self.retry_slowdown_until = Some(
                self.retry_slowdown_until
                    .map(|existing| existing.max(slowdown_until))
                    .unwrap_or(slowdown_until),
            );
        }
        self.refresh_workgate_caps(now);
    }

    fn refresh_workgate_caps(&mut self, now: SystemTime) {
        if !self.retry_slowdown_active(now) {
            self.retry_slowdown_until = None;
        }

        let throttle_caps = self.effective_throttle_caps(now);
        self.lock_workgate()
            .reconfigure(self.snapshot.throttle_state, throttle_caps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_intents::{BoundedEventIntentMaps, EventIntentLimits};
    use crate::scheduler::KeyedSupersedingScheduler;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn daemon_defaults_to_pre_ga_filesystem_stub_provider() {
        let app = DaemonApp::default();
        assert_eq!(app.provider_name(), "filesystem_stub");
    }

    #[test]
    fn explicit_transfer_ceiling_caps_every_throttle_tier() {
        let mut app = DaemonApp::default();
        // Idle default inputs: the core-derived tier applies.
        app.apply_throttle_inputs(ThrottleInputs::default());
        let unlimited = app.workgate_snapshot().caps;
        assert!(unlimited.upload_concurrency >= 4);

        app.set_concurrent_transfers(Some(1));
        app.apply_throttle_inputs(ThrottleInputs::default());
        let capped = app.workgate_snapshot().caps;
        assert_eq!(capped.upload_concurrency, 1);
        assert_eq!(capped.download_concurrency, 1);
        // Non-transfer caps are untouched by the transfer ceiling.
        assert_eq!(capped.planner_workers, unlimited.planner_workers);

        // A throttle-imposed zero (Suspended) stays zero under any
        // explicit ceiling.
        app.apply_throttle_inputs(ThrottleInputs {
            thermal_pressure: crate::throttle::ThermalPressure::Critical,
            ..ThrottleInputs::default()
        });
        let suspended = app.workgate_snapshot().caps;
        assert_eq!(suspended.upload_concurrency, 0);
        assert_eq!(suspended.download_concurrency, 0);
    }

    #[test]
    fn suspended_throttle_disables_remote_polling() {
        let mut app = DaemonApp::default();
        app.set_throttle_state(ThrottleState::Suspended, "thermal pressure");
        assert!(!app.remote_poll_allowed());
    }

    #[test]
    fn run_state_updates_reason() {
        let mut app = DaemonApp::default();
        app.set_run_state(RunState::Running, "daemon ready");
        assert_eq!(app.snapshot().run_state, RunState::Running);
        assert_eq!(app.snapshot().reason, "daemon ready");
    }

    #[test]
    fn applying_throttle_inputs_updates_snapshot_reason_and_caps() {
        let mut app = DaemonApp::default();
        let decision = app.apply_throttle_inputs(ThrottleInputs {
            user_active: true,
            ..ThrottleInputs::default()
        });

        assert_eq!(decision.state, ThrottleState::Throttled);
        assert_eq!(decision.cause, crate::throttle::ThrottleCause::UserActivity);
        assert_eq!(app.snapshot().throttle_state, ThrottleState::Throttled);
        assert_eq!(app.snapshot().throttle_reason, "user activity is active");
        assert_eq!(app.throttle_decision().unwrap().cause, decision.cause);

        let caps = app.throttle_caps();
        assert_eq!(caps.planner_workers, 1);
        assert_eq!(caps.upload_concurrency, 1);
        assert!(caps.allow_reconcile);
        assert_eq!(app.workgate_snapshot().caps, caps);
    }

    #[test]
    fn own_cpu_above_the_ceiling_takes_workers_before_the_state_changes() {
        let mut app = DaemonApp::default();
        app.apply_throttle_inputs(ThrottleInputs::default());
        let full = app.workgate_snapshot().caps;
        let widest = full
            .planner_workers
            .max(full.hash_workers)
            .max(full.upload_concurrency)
            .max(full.download_concurrency);
        let ceiling = vapor_shared::constants::resource_limits::DEFAULT_CPU_PERCENT;
        let busy = ThrottleInputs {
            vapor_cpu_load_percent: ceiling + 20,
            ..ThrottleInputs::default()
        };

        // One sample above the ceiling: one worker less, same state.
        let decision = app.apply_throttle_inputs(busy);
        assert_eq!(decision.state, ThrottleState::Idle);
        let caps = app.workgate_snapshot().caps;
        assert_eq!(caps.upload_concurrency, full.upload_concurrency - 1);
        assert_eq!(caps.planner_workers, full.planner_workers - 1);
        assert_eq!(app.cpu_regulation_steps(), 1);

        // Still above with every cap at one: only then the state steps
        // down, and the reason names the ceiling.
        let mut decision = decision;
        for _ in 0..widest {
            decision = app.apply_throttle_inputs(busy);
        }
        let caps = app.workgate_snapshot().caps;
        assert_eq!(caps.upload_concurrency, 1);
        assert_eq!(caps.planner_workers, 1);
        assert_eq!(decision.state, ThrottleState::Throttled);
        assert_eq!(
            decision.reason,
            format!(
                "Vapor CPU load is {}%, above its ceiling of {ceiling}%",
                ceiling + 20
            )
        );

        // Clearly below the ceiling: the workers come back one a sample.
        let before = app.cpu_regulation_steps();
        app.apply_throttle_inputs(ThrottleInputs::default());
        assert_eq!(app.cpu_regulation_steps(), before - 1);
    }

    #[test]
    fn throttled_workgate_limits_new_uploads_to_single_concurrency() {
        let mut app = DaemonApp::default();
        app.apply_throttle_inputs(ThrottleInputs {
            user_active: true,
            ..ThrottleInputs::default()
        });

        let first = app
            .try_acquire_work(WorkClass::Upload)
            .expect("first throttled upload should be allowed");
        let denied = app
            .try_acquire_work(WorkClass::Upload)
            .expect_err("second throttled upload should be blocked");

        assert_eq!(
            denied.reason,
            crate::workgate::WorkPermitDeniedReason::UploadConcurrencyExhausted
        );
        assert_eq!(app.workgate_snapshot().active_uploads, 1);
        assert!(app.release_work(first));
    }

    #[test]
    fn suspended_workgate_blocks_uploads_and_hashing() {
        let mut app = DaemonApp::default();
        app.apply_throttle_inputs(ThrottleInputs {
            thermal_pressure: crate::throttle::ThermalPressure::Critical,
            ..ThrottleInputs::default()
        });

        let upload_denied = app
            .try_acquire_work(WorkClass::Upload)
            .expect_err("uploads should be blocked when suspended");
        let hash_denied = app
            .try_acquire_work(WorkClass::Hash)
            .expect_err("hashing should be blocked when suspended");

        assert_eq!(
            upload_denied.reason,
            crate::workgate::WorkPermitDeniedReason::UploadsDisabled
        );
        assert_eq!(
            hash_denied.reason,
            crate::workgate::WorkPermitDeniedReason::HashingDisabled
        );
    }

    #[test]
    fn ensure_cloud_root_does_not_panic() {
        let app = DaemonApp::default();
        // The stub provider treats the cloud root as always present.
        assert!(app.ensure_cloud_root("/Vapor").is_ok());
    }

    #[test]
    fn build_info_is_populated() {
        assert!(!build_info::VERSION.is_empty());
        assert!(!build_info::GIT_COMMIT_SHORT.is_empty());
    }

    #[test]
    fn a_rate_limit_narrows_every_provider_lane_until_the_slowdown_expires() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut state_db = DurableStateDb::open(&database_path).expect("open durable state db");
        let mut app = DaemonApp::default();
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");
        let now = SystemTime::now();
        let baseline = app.throttle_caps();
        assert!(
            baseline.download_concurrency > 1 && baseline.planner_workers > 1,
            "the baseline has width to lose: {baseline:?}"
        );

        let queued = state_db
            .enqueue_intent(&path, crate::event_intents::PendingIntentKind::Upload, now)
            .expect("enqueue upload intent");
        let leased = state_db
            .lease_next_ready(now)
            .expect("lease next ready")
            .expect("leased record");
        assert_eq!(leased.id, queued.id);

        let scheduled = app
            .schedule_retry(
                &mut state_db,
                leased.id,
                RetryFailureKind::RateLimited {
                    retry_after: Some(Duration::from_secs(30)),
                },
                "429 rate limited",
                now,
            )
            .expect("schedule rate-limited retry");

        assert_eq!(
            app.retry_slowdown_until(),
            scheduled.decision.slowdown_until
        );
        let slowed = app.throttle_caps();
        assert_eq!(
            (
                slowed.upload_concurrency,
                slowed.download_concurrency,
                slowed.planner_workers
            ),
            (1, 1, 1),
            "the provider counts uploads, downloads, and probes against one quota"
        );

        app.refresh_workgate_caps(
            scheduled.decision.available_at.unwrap() + Duration::from_secs(1),
        );
        assert!(app.retry_slowdown_until().is_none());
        let restored = app.throttle_caps();
        assert_eq!(
            (
                restored.upload_concurrency,
                restored.download_concurrency,
                restored.planner_workers
            ),
            (
                baseline.upload_concurrency,
                baseline.download_concurrency,
                baseline.planner_workers
            )
        );
    }

    #[test]
    fn retry_slowdown_clamp_survives_a_high_idle_boost_ceiling() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let mut state_db =
            DurableStateDb::open(temp_dir.path().join("state/vapor.sqlite")).expect("open db");
        let mut app = DaemonApp::default();
        let now = SystemTime::now();
        // Idle boost publishes a 50% ceiling (scale 50/15 ≈ 3.3). Without
        // ordering the slowdown after scaling, upload_concurrency 1 would
        // be multiplied back up during the rate-limit window.
        app.set_throttle_state(vapor_shared::ThrottleState::Idle, "idle");
        app.apply_resource_cpu_ceiling(50, now);
        let path = PathBuf::from("/tmp/vapor-root/f.txt");
        state_db
            .enqueue_intent(&path, crate::event_intents::PendingIntentKind::Upload, now)
            .expect("enqueue");
        let leased = state_db
            .lease_next_ready(now)
            .expect("lease")
            .expect("leased");
        app.schedule_retry(
            &mut state_db,
            leased.id,
            RetryFailureKind::RateLimited {
                retry_after: Some(Duration::from_secs(30)),
            },
            "429",
            now,
        )
        .expect("schedule");
        assert_eq!(app.throttle_caps().upload_concurrency, 1);
    }

    #[test]
    fn cpu_ceiling_never_raises_caps_above_the_throttled_tier() {
        let mut app = DaemonApp::default();
        let now = SystemTime::now();
        app.set_throttle_state(vapor_shared::ThrottleState::Throttled, "user active");
        let base = app.throttle_caps();
        // A large ceiling (scale 100/15 ≈ 6.7) must not lift the Throttled
        // tier: ceilings only relax under Idle.
        app.apply_resource_cpu_ceiling(100, now);
        let scaled = app.throttle_caps();
        assert!(scaled.planner_workers <= base.planner_workers);
        assert!(scaled.hash_workers <= base.hash_workers);
        assert!(scaled.upload_concurrency <= base.upload_concurrency);
        assert!(scaled.download_concurrency <= base.download_concurrency);
    }

    #[test]
    fn restored_retry_slowdown_reapplies_upload_clamp_after_reopen() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");
        let now = SystemTime::now();

        {
            let mut state_db = DurableStateDb::open(&database_path).expect("open durable state db");
            state_db
                .enqueue_intent(&path, crate::event_intents::PendingIntentKind::Upload, now)
                .expect("enqueue upload intent");
            let leased = state_db
                .lease_next_ready(now)
                .expect("lease next ready")
                .expect("leased record");
            let mut app = DaemonApp::default();
            app.schedule_retry(
                &mut state_db,
                leased.id,
                RetryFailureKind::RateLimited {
                    retry_after: Some(Duration::from_secs(30)),
                },
                "429 rate limited",
                now,
            )
            .expect("schedule rate-limited retry");
        }

        let mut reopened_state_db =
            DurableStateDb::open(&database_path).expect("reopen durable state db");
        let mut reopened_app = DaemonApp::default();
        let restored = reopened_app
            .restore_retry_slowdown(&mut reopened_state_db, now + Duration::from_secs(5))
            .expect("restore retry slowdown")
            .expect("active slowdown marker");

        assert!(restored > now);
        assert_eq!(reopened_app.throttle_caps().upload_concurrency, 1);
    }

    #[test]
    fn app_reconcile_flow_waits_out_suspension_and_opens_the_folder_at_release() {
        let subtree_root = PathBuf::from("/tmp/vapor-root/project/sub");
        let mut maps = stormed_maps(&subtree_root);
        let mut scheduler = KeyedSupersedingScheduler::default();
        let mut app = DaemonApp::default();

        app.set_throttle_state(ThrottleState::Suspended, "about to overheat");
        assert_eq!(
            app.release_ready_deferred_reconciles(&mut maps, &mut scheduler, timestamp(32)),
            0
        );
        assert!(
            app.try_start_reconcile(&mut scheduler, timestamp(32))
                .expect("reconcile start should not fail")
                .is_none()
        );

        app.set_throttle_state(ThrottleState::Light, "on battery");
        assert_eq!(
            app.release_ready_deferred_reconciles(&mut maps, &mut scheduler, timestamp(32)),
            1
        );
        assert_eq!(
            app.try_start_reconcile(&mut scheduler, timestamp(32))
                .expect("reconcile start should succeed"),
            Some(subtree_root.clone())
        );

        assert!(maps.compacted_subtree(&subtree_root).is_none());
        app.complete_reconcile(&mut maps, &mut scheduler)
            .expect("complete reconcile");
    }

    #[test]
    fn app_reconcile_checkpoint_interrupts_when_throttle_changes() {
        let subtree_root = PathBuf::from("/tmp/vapor-root/project/sub");
        let mut maps = stormed_maps(&subtree_root);
        let mut scheduler = KeyedSupersedingScheduler::default();
        let mut app = DaemonApp::default();

        app.set_throttle_state(ThrottleState::Idle, "idle drain");
        app.release_ready_deferred_reconciles(&mut maps, &mut scheduler, timestamp(32));
        app.try_start_reconcile(&mut scheduler, timestamp(32))
            .expect("start reconcile");
        app.set_throttle_state(ThrottleState::Light, "load spike");
        assert!(
            app.checkpoint_reconcile(&mut scheduler, timestamp(33))
                .is_none(),
            "a slower state keeps the scan running"
        );
        app.set_throttle_state(ThrottleState::Suspended, "about to overheat");

        let pause = app
            .checkpoint_reconcile(&mut scheduler, timestamp(33))
            .expect("reconcile should pause");
        assert_eq!(
            pause.reason,
            crate::reconcile::ReconcilePauseReason::ThrottleSuspended
        );
        assert_eq!(scheduler.pending_count(), 1);
    }

    fn stormed_maps(subtree_root: &std::path::Path) -> BoundedEventIntentMaps {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let mut maps = BoundedEventIntentMaps::with_limits_and_storm_thresholds(
            watch_root,
            EventIntentLimits::new(100, 100),
            crate::storm::StormThresholds {
                window: Duration::from_secs(2),
                directory_unique_paths_threshold: 2,
                directory_event_count_threshold: 99,
                global_pending_event_count_threshold: 99,
                deferred_reconcile_delay: Duration::from_secs(30),
                absorbed_busy_event_threshold: 1,
            },
        );

        maps.record_event(crate::fs_events::FsEventRecord {
            path: subtree_root.join("a.txt"),
            kind: crate::fs_events::FsEventKind::Modified,
            observed_at: timestamp(1),
        });
        maps.record_event(crate::fs_events::FsEventRecord {
            path: subtree_root.join("b.txt"),
            kind: crate::fs_events::FsEventKind::Modified,
            observed_at: timestamp(2),
        });
        maps
    }

    fn timestamp(seconds: u64) -> SystemTime {
        std::time::UNIX_EPOCH + Duration::from_secs(seconds)
    }
}
