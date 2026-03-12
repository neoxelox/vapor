#![forbid(unsafe_code)]

use vapor_providers::{GoogleDriveProvider, Provider};
use vapor_shared::{RunState, StatusSnapshot, ThrottleState};

use crate::throttle::{ThrottleCaps, ThrottleController, ThrottleDecision, ThrottleInputs};

pub mod build_info {
    include!(concat!(env!("OUT_DIR"), "/vapor_build_info.rs"));
}

pub mod debounce;
pub mod event_intents;
pub mod fs_events;
pub mod logging;
pub mod path_filter;
pub mod scheduler;
pub mod sync_directories;
pub mod throttle;

#[derive(Debug)]
pub struct DaemonApp {
    snapshot: StatusSnapshot,
    provider: GoogleDriveProvider,
    throttle_controller: ThrottleController,
    last_throttle_decision: Option<ThrottleDecision>,
}

impl Default for DaemonApp {
    fn default() -> Self {
        logging::info("Initialized daemon app state", &[]);
        Self {
            snapshot: StatusSnapshot::default(),
            provider: GoogleDriveProvider,
            throttle_controller: ThrottleController::default(),
            last_throttle_decision: None,
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
    }

    pub fn apply_throttle_inputs(&mut self, inputs: ThrottleInputs) -> ThrottleDecision {
        let decision = self.throttle_controller.evaluate(inputs);
        self.set_throttle_state(decision.state, decision.reason.clone());
        self.last_throttle_decision = Some(decision.clone());
        decision
    }

    pub fn throttle_caps(&self) -> ThrottleCaps {
        self.last_throttle_decision
            .as_ref()
            .map(|decision| decision.caps)
            .unwrap_or_else(|| {
                self.throttle_controller
                    .caps_for(self.snapshot.throttle_state)
            })
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
