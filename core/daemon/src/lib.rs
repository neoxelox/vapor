#![forbid(unsafe_code)]

use std::time::SystemTime;

use vapor_providers::{GoogleDriveProvider, Provider};
use vapor_shared::{RunState, StatusSnapshot, ThrottleState};

use crate::retry::{RetryDecision, RetryFailureKind};
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

pub mod debounce;
pub mod event_intents;
pub mod fs_events;
pub mod logging;
pub mod path_filter;
pub mod retry;
pub mod scheduler;
pub mod state_db;
pub mod storm;
pub mod sync_directories;
pub mod throttle;
pub mod workgate;

#[derive(Debug)]
pub struct DaemonApp {
    snapshot: StatusSnapshot,
    provider: GoogleDriveProvider,
    throttle_controller: ThrottleController,
    last_throttle_decision: Option<ThrottleDecision>,
    retry_slowdown_until: Option<SystemTime>,
    workgate: ThrottleWorkgate,
}

impl Default for DaemonApp {
    fn default() -> Self {
        logging::info("Initialized daemon app state", &[]);
        let snapshot = StatusSnapshot::default();
        let initial_throttle_state = snapshot.throttle_state;
        let throttle_controller = ThrottleController::default();
        let throttle_caps = throttle_controller.caps_for(initial_throttle_state);
        Self {
            snapshot,
            provider: GoogleDriveProvider,
            throttle_controller,
            last_throttle_decision: None,
            retry_slowdown_until: None,
            workgate: ThrottleWorkgate::new(initial_throttle_state, throttle_caps),
        }
    }
}

impl DaemonApp {
    pub fn snapshot(&self) -> &StatusSnapshot {
        &self.snapshot
    }

    pub fn provider_name(&self) -> &'static str {
        self.provider.name()
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
        if self.snapshot.throttle_state == throttle_state && self.snapshot.reason == reason {
            return;
        }

        logging::warning(
            "Updated throttle state",
            &[
                ("throttle_state", format!("{:?}", throttle_state)),
                ("reason", reason.clone()),
            ],
        );
        self.snapshot.throttle_state = throttle_state;
        self.snapshot.reason = reason;
        self.refresh_workgate_caps(SystemTime::now());
    }

    pub fn apply_throttle_inputs(&mut self, inputs: ThrottleInputs) -> ThrottleDecision {
        let decision = self.throttle_controller.evaluate(inputs);
        self.set_throttle_state(decision.state, decision.reason.clone());
        self.last_throttle_decision = Some(decision.clone());
        decision
    }

    pub fn throttle_caps(&self) -> ThrottleCaps {
        self.effective_throttle_caps(SystemTime::now())
    }

    pub fn workgate_snapshot(&self) -> WorkgateSnapshot {
        self.workgate.snapshot()
    }

    pub fn try_acquire_work(&mut self, class: WorkClass) -> Result<WorkPermit, WorkPermitDenied> {
        self.refresh_workgate_caps(SystemTime::now());
        self.workgate.try_acquire(class)
    }

    pub fn release_work(&mut self, permit: WorkPermit) -> bool {
        self.workgate.release(permit)
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
        logging::debug(
            "Evaluated remote polling permission",
            &[
                (
                    "throttle_state",
                    format!("{:?}", self.snapshot.throttle_state),
                ),
                ("allowed", allowed.to_string()),
            ],
        );
        allowed
    }

    pub fn ensure_cloud_sync_directory(&self, cloud_sync_directory: &str) {
        match self
            .provider
            .ensure_cloud_sync_directory(cloud_sync_directory)
        {
            Ok(_) => logging::info(
                "Cloud sync directory is ready",
                &[("cloud_sync_directory", cloud_sync_directory.to_string())],
            ),
            Err(error) => logging::error(
                "Failed to ensure cloud sync directory",
                &[
                    ("cloud_sync_directory", cloud_sync_directory.to_string()),
                    ("error", error),
                ],
            ),
        }
    }

    fn base_throttle_caps(&self) -> ThrottleCaps {
        self.throttle_controller
            .caps_for(self.snapshot.throttle_state)
    }

    fn effective_throttle_caps(&self, now: SystemTime) -> ThrottleCaps {
        let mut caps = self.base_throttle_caps();
        if self.retry_slowdown_active(now) {
            caps.upload_concurrency = caps.upload_concurrency.min(1);
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
        self.workgate
            .reconfigure(self.snapshot.throttle_state, throttle_caps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn daemon_defaults_to_google_drive_provider() {
        let app = DaemonApp::default();
        assert_eq!(app.provider_name(), "google_drive");
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
        assert_eq!(app.snapshot().reason, "user activity is active");
        assert_eq!(app.throttle_decision().unwrap().cause, decision.cause);

        let caps = app.throttle_caps();
        assert_eq!(caps.planner_workers, 1);
        assert_eq!(caps.upload_concurrency, 1);
        assert!(!caps.allow_reconcile);
        assert_eq!(app.workgate_snapshot().caps, caps);
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
            low_power_mode: true,
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
    fn ensure_cloud_sync_directory_does_not_panic() {
        let app = DaemonApp::default();
        app.ensure_cloud_sync_directory("/Vapor");
    }

    #[test]
    fn build_info_is_populated() {
        assert!(!build_info::VERSION.is_empty());
        assert!(!build_info::GIT_COMMIT_SHORT.is_empty());
    }

    #[test]
    fn rate_limited_retry_clamps_upload_concurrency_until_slowdown_expires() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut state_db = DurableStateDb::open(&database_path).expect("open durable state db");
        let mut app = DaemonApp::default();
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");
        let now = SystemTime::now();
        let baseline_upload_concurrency = app.throttle_caps().upload_concurrency;

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
        assert_eq!(app.throttle_caps().upload_concurrency, 1);

        app.refresh_workgate_caps(
            scheduled.decision.available_at.unwrap() + Duration::from_secs(1),
        );
        assert!(app.retry_slowdown_until().is_none());
        assert_eq!(
            app.throttle_caps().upload_concurrency,
            baseline_upload_concurrency
        );
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
}
