//! Daemon-side `vapor_ipc::Service` implementation.
//!
//! Adapts the live `DaemonApp` snapshot into the wire-format
//! `StatusResponse` so the IPC server can answer `Method::Status`
//! requests.
//!
//! The server runner spawned in `runtime.rs` shares this type through
//! an `Arc`. Wave 7 grows the trait surface with `pause`, `resume`,
//! `flush_now`, `reconcile`, etc.

use std::sync::{Arc, Mutex};

use vapor_ipc::{
    AckResponse, Service, StatusResponse, TimelineResponse, daemon_supported_versions,
};
use vapor_shared::ThrottleState;

use crate::DaemonApp;
use crate::runtime_control::RuntimeControl;

/// Snapshot of the runtime's view that the IPC server hands out.
/// Updated by the runtime tick loop after each `apply_throttle_inputs`
/// call so the IPC reflects the latest decision.
#[derive(Clone, Debug)]
pub struct DaemonStatusSnapshot {
    pub run_state: String,
    pub throttle_state: ThrottleState,
    pub provider_name: String,
    pub throttle_reason: String,
}

impl DaemonStatusSnapshot {
    pub fn from_app(app: &DaemonApp) -> Self {
        let snapshot = app.snapshot();
        let throttle_reason = app
            .throttle_decision()
            .map(|decision| decision.reason.clone())
            .unwrap_or_default();
        Self {
            run_state: format!("{:?}", snapshot.run_state),
            throttle_state: snapshot.throttle_state,
            provider_name: app.provider_name().to_string(),
            throttle_reason,
        }
    }
}

/// `vapor_ipc::Service` implementation that returns the latest
/// snapshot the runtime has published. The runtime calls
/// [`DaemonIpcService::publish`] after every tick.
#[derive(Debug)]
pub struct DaemonIpcService {
    snapshot: Mutex<DaemonStatusSnapshot>,
    control: Arc<RuntimeControl>,
}

impl DaemonIpcService {
    pub fn new(initial: DaemonStatusSnapshot, control: Arc<RuntimeControl>) -> Self {
        Self {
            snapshot: Mutex::new(initial),
            control,
        }
    }

    pub fn publish(&self, snapshot: DaemonStatusSnapshot) {
        *self
            .snapshot
            .lock()
            .expect("DaemonIpcService mutex poisoned") = snapshot;
    }
}

impl Service for DaemonIpcService {
    fn status(&self) -> StatusResponse {
        let (current, _min) = daemon_supported_versions();
        let snapshot = self
            .snapshot
            .lock()
            .expect("DaemonIpcService mutex poisoned");
        StatusResponse {
            schema_version: current,
            run_state: snapshot.run_state.clone(),
            throttle_state: format!("{:?}", snapshot.throttle_state),
            provider_name: snapshot.provider_name.clone(),
            throttle_reason: snapshot.throttle_reason.clone(),
            daemon_id: format!("vapord/{}", crate::build_info::VERSION),
        }
    }

    fn pause(&self) -> AckResponse {
        self.control.request_pause();
        let (current, _) = daemon_supported_versions();
        AckResponse {
            schema_version: current,
            accepted: true,
            note: "pause requested; runtime will apply on next tick".to_string(),
        }
    }

    fn resume(&self) -> AckResponse {
        self.control.request_resume();
        let (current, _) = daemon_supported_versions();
        AckResponse {
            schema_version: current,
            accepted: true,
            note: "resume requested; runtime will apply on next tick".to_string(),
        }
    }

    fn flush_now(&self) -> AckResponse {
        self.control.request_flush();
        let (current, _) = daemon_supported_versions();
        AckResponse {
            schema_version: current,
            accepted: true,
            note: "flush hint recorded; runtime drains opportunistically already".to_string(),
        }
    }

    fn reconcile(&self) -> AckResponse {
        self.control.request_reconcile();
        let (current, _) = daemon_supported_versions();
        AckResponse {
            schema_version: current,
            accepted: true,
            note: "reconcile requested; runtime will enqueue on next tick".to_string(),
        }
    }

    fn timeline(&self) -> TimelineResponse {
        // Wave 7 ships the IPC seam; the in-memory timeline buffer
        // lands alongside the C8-30 runtime work. Returning an empty
        // list keeps the wire shape stable for clients to consume.
        let (current, _) = daemon_supported_versions();
        TimelineResponse {
            schema_version: current,
            entries: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_response_uses_published_snapshot() {
        let initial = DaemonStatusSnapshot {
            run_state: "Running".to_string(),
            throttle_state: ThrottleState::IdleDrain,
            provider_name: "Filesystem (stub)".to_string(),
            throttle_reason: "idle, plugged in, and cool".to_string(),
        };
        let service = DaemonIpcService::new(initial, Arc::new(RuntimeControl::new()));
        let status = service.status();
        assert_eq!(status.run_state, "Running");
        assert_eq!(status.throttle_state, "IdleDrain");
        assert!(status.daemon_id.starts_with("vapord/"));
    }

    #[test]
    fn pause_records_pause_request_on_runtime_control() {
        let control = Arc::new(RuntimeControl::new());
        let service = DaemonIpcService::new(
            DaemonStatusSnapshot {
                run_state: "Running".to_string(),
                throttle_state: ThrottleState::IdleDrain,
                provider_name: "Filesystem (stub)".to_string(),
                throttle_reason: "idle".to_string(),
            },
            control.clone(),
        );
        let ack = service.pause();
        assert!(ack.accepted);
        assert_eq!(control.take_pause_request(), Some(true));
    }

    #[test]
    fn resume_records_resume_request_on_runtime_control() {
        let control = Arc::new(RuntimeControl::new());
        let service = DaemonIpcService::new(
            DaemonStatusSnapshot {
                run_state: "Paused".to_string(),
                throttle_state: ThrottleState::IdleDrain,
                provider_name: "Filesystem (stub)".to_string(),
                throttle_reason: "user pause".to_string(),
            },
            control.clone(),
        );
        let ack = service.resume();
        assert!(ack.accepted);
        assert_eq!(control.take_pause_request(), Some(false));
    }

    #[test]
    fn flush_now_and_reconcile_set_their_respective_flags() {
        let control = Arc::new(RuntimeControl::new());
        let service = DaemonIpcService::new(
            DaemonStatusSnapshot {
                run_state: "Running".to_string(),
                throttle_state: ThrottleState::IdleDrain,
                provider_name: "Filesystem (stub)".to_string(),
                throttle_reason: "idle".to_string(),
            },
            control.clone(),
        );
        let _ = service.flush_now();
        let _ = service.reconcile();
        assert!(control.take_flush_request());
        assert!(control.take_reconcile_request());
    }

    #[test]
    fn timeline_returns_empty_list_until_c8_30_lands() {
        let service = DaemonIpcService::new(
            DaemonStatusSnapshot {
                run_state: "Running".to_string(),
                throttle_state: ThrottleState::IdleDrain,
                provider_name: "Filesystem (stub)".to_string(),
                throttle_reason: "idle".to_string(),
            },
            Arc::new(RuntimeControl::new()),
        );
        let timeline = service.timeline();
        assert!(timeline.entries.is_empty());
    }

    #[test]
    fn publish_updates_subsequent_status_calls() {
        let initial = DaemonStatusSnapshot {
            run_state: "Running".to_string(),
            throttle_state: ThrottleState::IdleDrain,
            provider_name: "Filesystem (stub)".to_string(),
            throttle_reason: "idle".to_string(),
        };
        let service = DaemonIpcService::new(initial, Arc::new(RuntimeControl::new()));
        service.publish(DaemonStatusSnapshot {
            run_state: "Paused".to_string(),
            throttle_state: ThrottleState::Throttled,
            provider_name: "Filesystem (stub)".to_string(),
            throttle_reason: "user activity is active".to_string(),
        });
        let status = service.status();
        assert_eq!(status.run_state, "Paused");
        assert_eq!(status.throttle_state, "Throttled");
    }
}
