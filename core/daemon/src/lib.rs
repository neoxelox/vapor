#![forbid(unsafe_code)]

use vapor_providers::{GoogleDriveProvider, Provider};
use vapor_shared::{RunState, StatusSnapshot, ThrottleState};

pub mod fs_events;
pub mod logging;
pub mod path_filter;
pub mod sync_directories;

#[derive(Debug)]
pub struct DaemonApp {
    snapshot: StatusSnapshot,
    provider: GoogleDriveProvider,
}

impl Default for DaemonApp {
    fn default() -> Self {
        logging::info("Initialized daemon app state", &[]);
        Self {
            snapshot: StatusSnapshot::default(),
            provider: GoogleDriveProvider,
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
    fn ensure_cloud_sync_directory_does_not_panic() {
        let app = DaemonApp::default();
        app.ensure_cloud_sync_directory("/Vapor");
    }
}
