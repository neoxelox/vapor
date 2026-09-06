//! Daemon-side `vapor_ipc::Service` implementation.
//!
//! Adapts the runtime's published snapshots into the schema-v2 wire
//! shapes: aggregate + per-profile status, per-intent
//! "why stuck" diagnostics, the bounded activity timeline
//!, and the control endpoints. The runtime publishes a
//! fresh snapshot every tick; IPC handler threads never reach into the
//! runtime itself.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use vapor_ipc::{
    AckResponse, DiagnosticsResponse, IntentDiagnostic, ProfileStatus, ResourceBudgetStatus,
    Service, StatusResponse, TimelineEntry, TimelineResponse, daemon_supported_versions,
};
use vapor_shared::ThrottleState;
use vapor_shared::constants;

use crate::DaemonApp;
use crate::runtime_control::RuntimeControl;
use crate::timeline::TimelineBuffer;

/// Snapshot of the runtime's view that the IPC server hands out.
/// Updated by the runtime tick loop so IPC reflects the latest
/// decision. Every v2 field defaults so single-profile composition
/// tests can publish the minimal shape.
#[derive(Clone, Debug)]
pub struct DaemonStatusSnapshot {
    pub run_state: String,
    pub throttle_state: ThrottleState,
    pub provider_name: String,
    pub throttle_reason: String,
    pub queue_depth: u64,
    pub failed_intents: u64,
    pub loop_prevention_suppressions: u64,
    pub conflicts: u64,
    pub mirror_reverts: u64,
    pub mirror_deletes: u64,
    pub dropped_incoming_events: u64,
    pub profiles: Vec<ProfileStatus>,
    pub intent_diagnostics: Vec<IntentDiagnostic>,
    pub diagnostics_truncated: bool,
    pub resource_budget: Option<ResourceBudgetStatus>,
    /// Set when a restart-required configuration key changed under the
    /// running daemon; names the keys and the command to run.
    pub config_restart_required: Option<String>,
}

impl Default for DaemonStatusSnapshot {
    fn default() -> Self {
        Self {
            run_state: String::new(),
            throttle_state: ThrottleState::Light,
            provider_name: String::new(),
            throttle_reason: String::new(),
            queue_depth: 0,
            failed_intents: 0,
            loop_prevention_suppressions: 0,
            conflicts: 0,
            mirror_reverts: 0,
            mirror_deletes: 0,
            dropped_incoming_events: 0,
            profiles: Vec::new(),
            intent_diagnostics: Vec::new(),
            diagnostics_truncated: false,
            resource_budget: None,
            config_restart_required: None,
        }
    }
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
            ..Self::default()
        }
    }
}

/// Receives the runtime's latest snapshot at the end of each tick.
pub trait StatusPublisher: Send + Sync {
    fn publish(&self, snapshot: DaemonStatusSnapshot);
}

/// `vapor_ipc::Service` implementation that returns the latest
/// published snapshot plus the shared timeline buffer.
pub struct DaemonIpcService {
    snapshot: Mutex<DaemonStatusSnapshot>,
    control: Arc<RuntimeControl>,
    timeline: Option<Arc<TimelineBuffer>>,
}

impl std::fmt::Debug for DaemonIpcService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonIpcService").finish()
    }
}

impl DaemonIpcService {
    pub fn new(initial: DaemonStatusSnapshot, control: Arc<RuntimeControl>) -> Self {
        Self {
            snapshot: Mutex::new(initial),
            control,
            timeline: None,
        }
    }

    pub fn with_timeline(
        initial: DaemonStatusSnapshot,
        control: Arc<RuntimeControl>,
        timeline: Arc<TimelineBuffer>,
    ) -> Self {
        Self {
            snapshot: Mutex::new(initial),
            control,
            timeline: Some(timeline),
        }
    }

    pub fn publish(&self, snapshot: DaemonStatusSnapshot) {
        *self
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = snapshot;
    }

    fn snapshot(&self) -> DaemonStatusSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Read-modify-write of one `vapor.json` key, preserving every
    /// other key (same discipline as the CLI and the Swift store).
    fn write_config_key(key: &str, value: serde_json::Value) -> Result<(), String> {
        let path = vapor_shared::runtime_paths::vapor_directory()
            .join(constants::runtime::CONFIGURATION_FILE_NAME);
        // The whole read-modify-write must be atomic against every other
        // surface (CLI, app, and the 32 concurrent IPC connections). Without
        // the cross-process lock two writers each read the same document and
        // the last rename silently drops the other's key.
        vapor_shared::runtime_paths::with_config_lock::<(), std::io::Error>(&path, || {
            let mut document: serde_json::Value = match std::fs::read_to_string(&path) {
                Ok(contents) if contents.trim().is_empty() => {
                    serde_json::Value::Object(Default::default())
                }
                Ok(contents) => serde_json::from_str(&contents).map_err(|error| {
                    std::io::Error::other(format!("cannot parse {}: {error}", path.display()))
                })?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    serde_json::Value::Object(Default::default())
                }
                Err(error) => return Err(error),
            };
            document
                .as_object_mut()
                .ok_or_else(|| std::io::Error::other("configuration root is not an object"))?
                .insert(key.to_string(), value);
            vapor_shared::runtime_paths::write_config_document(&path, &document)?;
            Ok(())
        })
        .map_err(|error| format!("cannot update {}: {error}", path.display()))
    }

    fn ack(accepted: bool, note: impl Into<String>) -> AckResponse {
        let (current, _) = daemon_supported_versions();
        AckResponse {
            schema_version: current,
            accepted,
            note: note.into(),
        }
    }
}

impl StatusPublisher for DaemonIpcService {
    fn publish(&self, snapshot: DaemonStatusSnapshot) {
        DaemonIpcService::publish(self, snapshot);
    }
}

impl Service for DaemonIpcService {
    fn status(&self) -> StatusResponse {
        let (current, _min) = daemon_supported_versions();
        let snapshot = self.snapshot();
        StatusResponse {
            schema_version: current,
            run_state: snapshot.run_state.clone(),
            throttle_state: format!("{:?}", snapshot.throttle_state),
            provider_name: snapshot.provider_name.clone(),
            throttle_reason: snapshot.throttle_reason.clone(),
            daemon_id: format!("vapord/{}", crate::build_info::VERSION),
            queue_depth: snapshot.queue_depth,
            failed_intents: snapshot.failed_intents,
            loop_prevention_suppressions: snapshot.loop_prevention_suppressions,
            conflicts: snapshot.conflicts,
            mirror_reverts: snapshot.mirror_reverts,
            mirror_deletes: snapshot.mirror_deletes,
            profiles: snapshot.profiles,
            resource_budget: snapshot.resource_budget,
            config_restart_required: snapshot.config_restart_required,
        }
    }

    fn pause(&self) -> AckResponse {
        self.control.request_pause();
        Self::ack(true, "pause requested; runtime will apply on next tick")
    }

    fn resume(&self) -> AckResponse {
        self.control.request_resume();
        Self::ack(true, "resume requested; runtime will apply on next tick")
    }

    fn flush_now(&self) -> AckResponse {
        self.control.request_flush();
        Self::ack(
            true,
            "flush hint recorded; runtime drains opportunistically already",
        )
    }

    fn reconcile(&self) -> AckResponse {
        self.control.request_reconcile();
        Self::ack(
            true,
            "reconcile requested; runtime will enqueue on next tick",
        )
    }

    fn diagnostics(&self) -> DiagnosticsResponse {
        let (current, _) = daemon_supported_versions();
        let snapshot = self.snapshot();
        DiagnosticsResponse {
            schema_version: current,
            intents: snapshot.intent_diagnostics,
            truncated: snapshot.diagnostics_truncated,
            dropped_incoming_events: snapshot.dropped_incoming_events,
        }
    }

    fn set_auto_launch(&self, enabled: bool) -> AckResponse {
        match Self::write_config_key(
            constants::config::KEY_AUTO_LAUNCH,
            serde_json::Value::Bool(enabled),
        ) {
            Ok(()) => Self::ack(
                true,
                format!(
                    "autoLaunch set to {enabled}; service registration follows via `vapor service`"
                ),
            ),
            Err(error) => Self::ack(false, error),
        }
    }

    fn update_excludes(
        &self,
        pre_ignore_rules: Option<String>,
        post_ignore_rules: Option<String>,
    ) -> AckResponse {
        if pre_ignore_rules.is_none() && post_ignore_rules.is_none() {
            return Self::ack(false, "no exclude rules supplied");
        }
        if let Some(rules) = pre_ignore_rules
            && let Err(error) = Self::write_config_key(
                constants::config::KEY_PRE_IGNORE_RULES,
                serde_json::Value::String(rules),
            )
        {
            return Self::ack(false, error);
        }
        if let Some(rules) = post_ignore_rules
            && let Err(error) = Self::write_config_key(
                constants::config::KEY_POST_IGNORE_RULES,
                serde_json::Value::String(rules),
            )
        {
            return Self::ack(false, error);
        }
        Self::ack(
            true,
            "exclude rules persisted; the running daemon applies them within a few seconds",
        )
    }

    fn timeline(&self) -> TimelineResponse {
        let (current, _) = daemon_supported_versions();
        let entries = self
            .timeline
            .as_ref()
            .map(|timeline| {
                timeline
                    .snapshot(None)
                    .into_iter()
                    .map(|record| TimelineEntry {
                        timestamp_ms: record
                            .timestamp
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .map(|duration| duration.as_millis() as u64)
                            .unwrap_or(0),
                        kind: record.kind,
                        message: record.message,
                        profile_id: record.profile_id,
                    })
                    .collect()
            })
            .unwrap_or_default();
        TimelineResponse {
            schema_version: current,
            entries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(run_state: &str, throttle: ThrottleState, reason: &str) -> DaemonStatusSnapshot {
        DaemonStatusSnapshot {
            run_state: run_state.to_string(),
            throttle_state: throttle,
            provider_name: "filesystem".to_string(),
            throttle_reason: reason.to_string(),
            ..DaemonStatusSnapshot::default()
        }
    }

    #[test]
    fn status_response_uses_published_snapshot() {
        let service = DaemonIpcService::new(
            snapshot(
                "Running",
                ThrottleState::IdleDrain,
                "idle, plugged in, and cool",
            ),
            Arc::new(RuntimeControl::new()),
        );
        let status = service.status();
        assert_eq!(status.run_state, "Running");
        assert_eq!(status.throttle_state, "IdleDrain");
        assert!(status.daemon_id.starts_with("vapord/"));
    }

    #[test]
    fn pause_records_pause_request_on_runtime_control() {
        let control = Arc::new(RuntimeControl::new());
        let service = DaemonIpcService::new(
            snapshot("Running", ThrottleState::IdleDrain, "idle"),
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
            snapshot("Paused", ThrottleState::IdleDrain, "user pause"),
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
            snapshot("Running", ThrottleState::IdleDrain, "idle"),
            control.clone(),
        );
        let _ = service.flush_now();
        let _ = service.reconcile();
        assert!(control.take_flush_request());
        assert!(control.take_reconcile_request());
    }

    #[test]
    fn timeline_serves_the_shared_buffer_in_order() {
        let timeline = TimelineBuffer::new(10);
        timeline.push(
            "run_state",
            "default",
            "Running",
            SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_000),
        );
        timeline.push(
            "conflict",
            "default",
            "kept both versions of docs/a.txt",
            SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(2_000),
        );
        let service = DaemonIpcService::with_timeline(
            snapshot("Running", ThrottleState::IdleDrain, "idle"),
            Arc::new(RuntimeControl::new()),
            timeline,
        );
        let response = service.timeline();
        assert_eq!(response.entries.len(), 2);
        assert_eq!(response.entries[0].kind, "run_state");
        assert_eq!(response.entries[1].kind, "conflict");
        assert!(response.entries[0].timestamp_ms < response.entries[1].timestamp_ms);
        assert_eq!(response.entries[0].profile_id, "default");
    }

    #[test]
    fn diagnostics_serves_the_published_intent_rows() {
        let mut published = snapshot("Running", ThrottleState::IdleDrain, "idle");
        published.intent_diagnostics = vec![IntentDiagnostic {
            intent_id: 7,
            profile_id: "default".to_string(),
            path: "/tmp/x.txt".to_string(),
            action: "upload".to_string(),
            stage: "Retrying".to_string(),
            elapsed_in_stage_ms: 1_500,
            attempt_count: 3,
            last_error: "transient provider failure".to_string(),
            blocker_reason: "retry backoff active".to_string(),
        }];
        published.dropped_incoming_events = 4;
        let service = DaemonIpcService::new(published, Arc::new(RuntimeControl::new()));
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.intents.len(), 1);
        assert_eq!(diagnostics.intents[0].stage, "Retrying");
        assert_eq!(diagnostics.dropped_incoming_events, 4);
    }

    #[test]
    fn publish_updates_subsequent_status_calls() {
        let service = DaemonIpcService::new(
            snapshot("Running", ThrottleState::IdleDrain, "idle"),
            Arc::new(RuntimeControl::new()),
        );
        service.publish(snapshot(
            "Paused",
            ThrottleState::Throttled,
            "user activity is active",
        ));
        let status = service.status();
        assert_eq!(status.run_state, "Paused");
        assert_eq!(status.throttle_state, "Throttled");
    }
}
