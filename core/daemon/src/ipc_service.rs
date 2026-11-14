//! Daemon-side `vapor_ipc::Service` implementation.
//!
//! Adapts the runtime's published snapshots into the wire shapes of
//! the current IPC schema: aggregate and per-profile status,
//! per-intent "why stuck" diagnostics, the bounded activity timeline,
//! and the control endpoints. The runtime publishes a fresh snapshot
//! when something observable changed; IPC handler threads never reach
//! into the runtime itself.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use vapor_ipc::{
    AckResponse, DiagnosticsResponse, MirrorCounts, ProfileStatus, ReconcileStatus,
    ResourcesStatus, Service, StateReason, StatusResponse, TimelineEntry, TimelineResponse,
    WorkItem, daemon_supported_versions,
};
use vapor_shared::ThrottleState;
use vapor_shared::constants;

use crate::DaemonApp;
use crate::runtime_control::RuntimeControl;
use crate::timeline::TimelineBuffer;

/// Snapshot of the runtime's view that the IPC server hands out.
/// Updated by the runtime tick loop so IPC reflects the latest
/// decision. Every field defaults so single-profile composition tests
/// can publish the minimal shape.
#[derive(Clone, Debug)]
pub struct DaemonStatusSnapshot {
    pub run_state: String,
    pub run_reason: String,
    pub throttle_state: ThrottleState,
    pub provider: String,
    pub throttle_reason: String,
    pub queued: u64,
    pub failed: u64,
    pub decisions: u64,
    pub suppressed_echoes: u64,
    pub reconcile: ReconcileStatus,
    pub mirror: MirrorCounts,
    pub dropped_events: u64,
    pub profiles: Vec<ProfileStatus>,
    pub work_items: Vec<WorkItem>,
    pub diagnostics_truncated: bool,
    pub resources: Option<ResourcesStatus>,
    /// Set when a restart-required configuration key changed under the
    /// running daemon; names the keys and the command to run.
    pub restart_required: Option<String>,
}

impl Default for DaemonStatusSnapshot {
    fn default() -> Self {
        Self {
            run_state: String::new(),
            run_reason: String::new(),
            throttle_state: ThrottleState::Light,
            provider: String::new(),
            throttle_reason: String::new(),
            queued: 0,
            failed: 0,
            decisions: 0,
            suppressed_echoes: 0,
            reconcile: ReconcileStatus {
                state: constants::reconcile_state::IDLE.to_string(),
                detail: String::new(),
            },
            mirror: MirrorCounts::default(),
            dropped_events: 0,
            profiles: Vec::new(),
            work_items: Vec::new(),
            diagnostics_truncated: false,
            resources: None,
            restart_required: None,
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
            run_state: snapshot.run_state.wire_name().to_string(),
            run_reason: snapshot.reason.clone(),
            throttle_state: snapshot.throttle_state,
            provider: app.provider_name().to_string(),
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
            daemon: format!("vapord/{}", crate::build_info::VERSION),
            provider: snapshot.provider.clone(),
            run: StateReason {
                state: snapshot.run_state.clone(),
                reason: snapshot.run_reason.clone(),
            },
            throttle: StateReason {
                state: snapshot.throttle_state.wire_name().to_string(),
                reason: snapshot.throttle_reason.clone(),
            },
            reconcile: snapshot.reconcile,
            queued: snapshot.queued,
            failed: snapshot.failed,
            decisions: snapshot.decisions,
            mirror: snapshot.mirror,
            restart_required: snapshot.restart_required,
            resources: snapshot.resources,
            profiles: snapshot.profiles,
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

    fn reconcile(&self) -> AckResponse {
        self.control.request_reconcile();
        Self::ack(
            true,
            "reconcile requested; the scan runs under any throttle state but suspended and deferred work is released",
        )
    }

    fn diagnostics(&self) -> DiagnosticsResponse {
        let (current, _) = daemon_supported_versions();
        let snapshot = self.snapshot();
        DiagnosticsResponse {
            schema_version: current,
            items: snapshot.work_items,
            truncated: snapshot.diagnostics_truncated,
            dropped_events: snapshot.dropped_events,
            suppressed_echoes: snapshot.suppressed_echoes,
        }
    }

    fn timeline(&self) -> TimelineResponse {
        let (current, _) = daemon_supported_versions();
        let mut entries: Vec<TimelineEntry> = self
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
        let truncated = keep_newest_that_fit(&mut entries, TIMELINE_RESPONSE_BUDGET_BYTES);
        TimelineResponse {
            schema_version: current,
            entries,
            truncated,
        }
    }
}

/// The room a timeline response may take: half of one IPC frame, which
/// leaves the rest for the envelope and for the escapes JSON adds.
const TIMELINE_RESPONSE_BUDGET_BYTES: usize = vapor_shared::constants::ipc::MAX_PAYLOAD_BYTES / 2;

/// Drops the oldest entries until the rest fit in `budget` bytes of
/// JSON, by a generous estimate. A response larger than a frame cannot
/// be written at all: the session would close and the client would see
/// an end-of-file. Returns whether anything was dropped.
fn keep_newest_that_fit(entries: &mut Vec<TimelineEntry>, budget: usize) -> bool {
    // Field names, quotes, the timestamp, and separators.
    const PER_ENTRY_OVERHEAD: usize = 96;
    let mut used = 0usize;
    let mut kept = 0usize;
    for entry in entries.iter().rev() {
        // An escape can double a character, so text counts twice.
        let size = PER_ENTRY_OVERHEAD
            + 2 * (entry.kind.len() + entry.message.len() + entry.profile_id.len());
        if used + size > budget {
            break;
        }
        used += size;
        kept += 1;
    }
    let dropped = entries.len() - kept;
    entries.drain(..dropped);
    dropped > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(run_state: &str, throttle: ThrottleState, reason: &str) -> DaemonStatusSnapshot {
        DaemonStatusSnapshot {
            run_state: run_state.to_string(),
            throttle_state: throttle,
            provider: "filesystem".to_string(),
            throttle_reason: reason.to_string(),
            ..DaemonStatusSnapshot::default()
        }
    }

    #[test]
    fn status_response_uses_published_snapshot() {
        let service = DaemonIpcService::new(
            snapshot("running", ThrottleState::Idle, "idle, plugged in, and cool"),
            Arc::new(RuntimeControl::new()),
        );
        let status = service.status();
        assert_eq!(status.run.state, "running");
        assert_eq!(status.throttle.state, "idle");
        assert!(status.daemon.starts_with("vapord/"));
    }

    #[test]
    fn pause_records_pause_request_on_runtime_control() {
        let control = Arc::new(RuntimeControl::new());
        let service = DaemonIpcService::new(
            snapshot("running", ThrottleState::Idle, "idle"),
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
            snapshot("paused", ThrottleState::Idle, "user pause"),
            control.clone(),
        );
        let ack = service.resume();
        assert!(ack.accepted);
        assert_eq!(control.take_pause_request(), Some(false));
    }

    #[test]
    fn reconcile_sets_the_reconcile_flag() {
        let control = Arc::new(RuntimeControl::new());
        let service = DaemonIpcService::new(
            snapshot("running", ThrottleState::Idle, "idle"),
            control.clone(),
        );
        let _ = service.reconcile();
        assert!(control.take_reconcile_request());
    }

    #[test]
    fn a_timeline_that_would_not_fit_one_frame_keeps_its_newest_entries() {
        let entry = |index: usize| TimelineEntry {
            timestamp_ms: index as u64,
            kind: "cloud-delete".to_string(),
            message: format!("{index} {}", "x".repeat(400)),
            profile_id: "default".to_string(),
        };
        let mut entries: Vec<TimelineEntry> = (0..20_000).map(entry).collect();
        assert!(keep_newest_that_fit(
            &mut entries,
            TIMELINE_RESPONSE_BUDGET_BYTES
        ));
        assert!(
            entries.len() < 20_000 && entries.len() > 1_000,
            "{}",
            entries.len()
        );
        assert_eq!(entries.last().expect("newest").timestamp_ms, 19_999);
        let response = TimelineResponse {
            schema_version: 4,
            entries,
            truncated: true,
        };
        let frame = serde_json::to_vec(&response).expect("serialize");
        assert!(
            frame.len() < vapor_shared::constants::ipc::MAX_PAYLOAD_BYTES,
            "the response fits one frame: {} bytes",
            frame.len()
        );

        // A timeline of the default size is never cut.
        let mut small: Vec<TimelineEntry> = (0..1_000).map(entry).collect();
        assert!(!keep_newest_that_fit(
            &mut small,
            TIMELINE_RESPONSE_BUDGET_BYTES
        ));
        assert_eq!(small.len(), 1_000);
    }

    #[test]
    fn timeline_serves_the_shared_buffer_in_order() {
        let timeline = TimelineBuffer::new(10);
        timeline.push(
            "run-state",
            "default",
            "running",
            SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_000),
        );
        timeline.push(
            "conflict",
            "default",
            "kept both versions of docs/a.txt",
            SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(2_000),
        );
        let service = DaemonIpcService::with_timeline(
            snapshot("running", ThrottleState::Idle, "idle"),
            Arc::new(RuntimeControl::new()),
            timeline,
        );
        let response = service.timeline();
        assert_eq!(response.entries.len(), 2);
        assert_eq!(response.entries[0].kind, "run-state");
        assert_eq!(response.entries[1].kind, "conflict");
        assert!(response.entries[0].timestamp_ms < response.entries[1].timestamp_ms);
        assert_eq!(response.entries[0].profile_id, "default");
    }

    #[test]
    fn diagnostics_serves_the_published_work_items() {
        let mut published = snapshot("running", ThrottleState::Idle, "idle");
        published.work_items = vec![WorkItem {
            id: 7,
            profile_id: "default".to_string(),
            path: "/tmp/x.txt".to_string(),
            action: "upload".to_string(),
            stage: "retrying".to_string(),
            elapsed_ms: 1_500,
            attempts: 3,
            error: "transient provider failure".to_string(),
            reason: "retry backoff active".to_string(),
        }];
        published.dropped_events = 4;
        published.suppressed_echoes = 2;
        let service = DaemonIpcService::new(published, Arc::new(RuntimeControl::new()));
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.items.len(), 1);
        assert_eq!(diagnostics.items[0].stage, "retrying");
        assert_eq!(diagnostics.dropped_events, 4);
        assert_eq!(diagnostics.suppressed_echoes, 2);
    }

    #[test]
    fn publish_updates_subsequent_status_calls() {
        let service = DaemonIpcService::new(
            snapshot("running", ThrottleState::Idle, "idle"),
            Arc::new(RuntimeControl::new()),
        );
        service.publish(snapshot(
            "paused",
            ThrottleState::Throttled,
            "user activity is active",
        ));
        let status = service.status();
        assert_eq!(status.run.state, "paused");
        assert_eq!(status.throttle.state, "throttled");
    }
}
