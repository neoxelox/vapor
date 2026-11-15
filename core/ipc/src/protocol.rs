//! Wire-format types for the Vapor IPC channel.
//!
//! All payloads are JSON with camelCase keys and kebab-case values, the
//! same shape `vapor … --json` prints. The envelope is the exception:
//! its tags, the method names, and the error codes are the Rust variant
//! names (`Hello`, `Call`, `Status`, `Ok`, `MethodNotFound`). Top-level
//! types carry an explicit `schemaVersion` per
//! `docs/architecture/ipc-contracts.md`. We use
//! `serde(default)` on every optional field so peer N-1 can omit fields
//! peer N introduced without forcing a hard parse error: the "field
//! omission tolerance" rule from the contracts doc.

use serde::{Deserialize, Serialize};

use vapor_shared::constants;

/// Convenience tuple returned by [`daemon_supported_versions`].
pub fn daemon_supported_versions() -> (u32, u32) {
    (
        constants::ipc::SCHEMA_VERSION_CURRENT,
        constants::ipc::SCHEMA_VERSION_MIN_SUPPORTED,
    )
}

/// First message every client sends. The server matches the version
/// against its own support window and responds with [`HelloAck`].
///
/// The handshake is what lets two builds find out they cannot talk, so
/// its frames also accept the snake_case keys schema 3 and earlier
/// sent: an older peer gets `IncompatibleVersion`, not a parse error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    #[serde(alias = "schema_version")]
    pub schema_version: u32,
    #[serde(alias = "supported_min_version")]
    pub supported_min_version: u32,
    /// Free-form identifier, e.g. `"vapor-cli/0.2.0-alpha.3"`. Not
    /// part of the contract; logged on the server side for diagnostics.
    #[serde(default, alias = "client_id")]
    pub client_id: String,
}

/// Server's reply to [`Hello`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloAck {
    #[serde(alias = "schema_version")]
    pub schema_version: u32,
    #[serde(alias = "supported_min_version")]
    pub supported_min_version: u32,
    #[serde(default, alias = "server_id")]
    pub server_id: String,
}

/// Returned in lieu of [`HelloAck`] when the version skew exceeds the
/// supported window. Mirrors the `IncompatibleVersion` error in the
/// contracts doc.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncompatibleVersion {
    #[serde(alias = "peer_version")]
    pub peer_version: u32,
    #[serde(alias = "required_min")]
    pub required_min: u32,
    #[serde(alias = "local_version")]
    pub local_version: u32,
}

/// Methods exposed by the daemon. Each variant is the name that goes
/// on the wire. Unknown method names are rejected with
/// `ErrorBody::MethodNotFound`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Method {
    /// Read the current daemon snapshot.
    Status,
    /// Flip `RunState` to `Paused`: idempotent.
    Pause,
    /// Flip `RunState` back to `Running`: idempotent.
    Resume,
    /// The user's on-demand sync: a whole-scope reconcile admitted
    /// under any throttle state but `Suspended`, and deferred work
    /// released for a short window so what the scan finds moves at once.
    Reconcile,
    /// Read the bounded daemon activity timeline.
    Timeline,
    /// Per-intent "why stuck" diagnostics.
    Diagnostics,
}

/// Top-level request envelope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum Request {
    Hello(Hello),
    Call { method: Method },
}

/// Successful response payloads, indexed by which method they answer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
#[allow(clippy::large_enum_variant)] // wire envelope; boxing would change the JSON shape reasoning for zero wins
pub enum ResponseBody {
    HelloAck(HelloAck),
    IncompatibleVersion(IncompatibleVersion),
    Status(StatusResponse),
    /// Generic acknowledgement for fire-and-forget control endpoints
    /// (Pause / Resume / Reconcile). The `accepted` flag
    /// reflects whether the request actually changed any state: the
    /// daemon currently always returns `true` once the request reaches
    /// the runtime, but
    /// future versions may return `false` for no-op cases (e.g.
    /// pausing an already-paused daemon).
    Ack(AckResponse),
    Timeline(TimelineResponse),
    Diagnostics(DiagnosticsResponse),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AckResponse {
    pub schema_version: u32,
    pub accepted: bool,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEntry {
    pub timestamp_ms: u64,
    pub kind: String,
    pub message: String,
    /// Profile the event belongs to.
    #[serde(default)]
    pub profile_id: String,
}

/// One queued or in-flight piece of work with the reason it is not
/// moving yet: the rows of `vapor diagnostics`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItem {
    pub id: i64,
    #[serde(default)]
    pub profile_id: String,
    pub path: String,
    /// What the item does: one of `constants::intent::ACTION_*`.
    pub action: String,
    /// Where it is: one of `constants::intent::STAGE_*`.
    pub stage: String,
    /// How long it has been in `stage`.
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default)]
    pub attempts: u32,
    /// The last error, when an attempt failed.
    #[serde(default)]
    pub error: String,
    /// Why the item is not progressing right now.
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsResponse {
    pub schema_version: u32,
    #[serde(default)]
    pub items: Vec<WorkItem>,
    /// True when the daemon truncated the list to its per-response cap.
    #[serde(default)]
    pub truncated: bool,
    /// Events dropped at the bounded fs-event ingest boundary since
    /// startup (callback-vs-runtime backpressure signal).
    #[serde(default)]
    pub dropped_events: u64,
    /// The daemon's own writes seen again by the watcher or the cloud
    /// feed and skipped, since startup (loop prevention).
    #[serde(default)]
    pub suppressed_echoes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineResponse {
    pub schema_version: u32,
    #[serde(default)]
    pub entries: Vec<TimelineEntry>,
    /// True when the daemon left the oldest entries out so that the
    /// response fits one frame.
    #[serde(default)]
    pub truncated: bool,
}

/// Wrapper response sent over the wire. Distinguishes success
/// (`Ok(body)`) from protocol-level failures (`Err(error)`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "value")]
#[allow(clippy::large_enum_variant)] // short-lived wire envelope
pub enum Response {
    Ok(ResponseBody),
    Err(ErrorBody),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", content = "message")]
pub enum ErrorBody {
    /// The handshake hasn't completed yet (or failed) on this session.
    HandshakeRequired(String),
    /// The handshake found peers outside the supported skew window.
    IncompatibleVersion(IncompatibleVersion),
    /// Method name was unknown on the server side.
    MethodNotFound(String),
    /// The frame's declared length was over [`vapor_shared::constants::ipc::MAX_PAYLOAD_BYTES`].
    PayloadTooLarge(u32),
    /// Generic backend error while servicing a request.
    Backend(String),
}

/// A state and why it holds.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateReason {
    pub state: String,
    #[serde(default)]
    pub reason: String,
}

/// What the whole-scope scan is doing: `state` is one of
/// `constants::reconcile_state::*`; `detail` says what it waits for or
/// what it scans, and is empty when idle.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileStatus {
    pub state: String,
    #[serde(default)]
    pub detail: String,
}

/// What a one-way mirror changed to match its source, since startup.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorCounts {
    #[serde(default)]
    pub reverts: u64,
    #[serde(default)]
    pub deletes: u64,
}

/// `Status` method response: the daemon as a whole, then one row per
/// profile. The daemon-wide run state is the most severe profile's.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusResponse {
    pub schema_version: u32,
    /// Which daemon build answered (`vapord/<version>`).
    #[serde(default)]
    pub daemon: String,
    /// The provider of the profile the daemon-wide state comes from
    /// (`filesystem`, `gdrive`); `none` when no profile is enabled.
    pub provider: String,
    /// `state` is one of `constants::run_state::*`; `reason` is the
    /// profile's own explanation (a suspension first, otherwise the
    /// first profile in that state that gives one).
    pub run: StateReason,
    /// `state` is one of `constants::throttle_state::*`.
    pub throttle: StateReason,
    #[serde(default)]
    pub reconcile: ReconcileStatus,
    /// Work waiting in the durable queue, across profiles.
    #[serde(default)]
    pub queued: u64,
    /// Work that failed for good, across profiles.
    #[serde(default)]
    pub failed: u64,
    /// Open decisions across profiles (`vapor decisions list`),
    /// conflicts included.
    #[serde(default)]
    pub decisions: u64,
    #[serde(default)]
    pub mirror: MirrorCounts,
    /// Set when a restart-required key of `vapor.json` changed under the
    /// running daemon; names the keys and the command to run.
    #[serde(default)]
    pub restart_required: Option<String>,
    /// Ceilings, measured use, and idle boost. `None` until the daemon
    /// has sampled once.
    #[serde(default)]
    pub resources: Option<ResourcesStatus>,
    #[serde(default)]
    pub profiles: Vec<ProfileStatus>,
}

/// One profile's row inside [`StatusResponse`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProfileStatus {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider: String,
    /// `two-way` / `pull-only` / `push-only`.
    #[serde(default)]
    pub sync_mode: String,
    /// What is watched, what blocks sync, or which decision it waits on.
    #[serde(default)]
    pub run: StateReason,
    #[serde(default)]
    pub reconcile: ReconcileStatus,
    #[serde(default)]
    pub queued: u64,
    #[serde(default)]
    pub failed: u64,
    #[serde(default)]
    pub decisions: u64,
    #[serde(default)]
    pub mirror: MirrorCounts,
    /// The profile was stopped by its configuration or by repeated
    /// failures; `run.reason` says why. The other profiles keep syncing.
    #[serde(default)]
    pub suspended: bool,
    /// The provider refused this profile's sign-in (expired, revoked, or
    /// never stored). Sync is on hold until the user runs
    /// `vapor auth login --profile <id>`; queued work is kept.
    #[serde(default)]
    pub sign_in_required: bool,
    /// How many entries on this device the ignore rules keep out of
    /// sync, as the last whole-scope scan counted them: a file, or a
    /// folder with everything under it, is one. Absent until a scan
    /// has finished. `vapor ignored` lists them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignored_paths: Option<u64>,
}

/// A resource ceiling and how much of it is in use.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Ceiling {
    #[serde(default)]
    pub limit_percent: u8,
    #[serde(default)]
    pub used_percent: u8,
}

/// The bandwidth ceiling. `limitPercent` is a share of the link
/// capacity, which the daemon measures for each direction; `usedKbps`
/// is what it moved over the last seconds, both directions together.
/// A capacity is absent while nothing is known about that direction,
/// and then that direction has no limit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BandwidthCeiling {
    #[serde(default)]
    pub limit_percent: u8,
    #[serde(default)]
    pub used_kbps: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_capacity_kbps: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_capacity_kbps: Option<u64>,
}

/// The effective ceilings (raised by idle boost while it is active),
/// what the daemon uses of them, and idle boost's state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesStatus {
    #[serde(default)]
    pub cpu: Ceiling,
    #[serde(default)]
    pub memory: Ceiling,
    #[serde(default)]
    pub bandwidth: BandwidthCeiling,
    /// `state` is one of `constants::idle_boost_state::*`.
    #[serde(default)]
    pub idle_boost: StateReason,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trips_through_serde_with_optional_client_id() {
        let hello = Hello {
            schema_version: 1,
            supported_min_version: 1,
            client_id: "vapor-cli/test".to_string(),
        };
        let bytes = serde_json::to_vec(&hello).expect("serialize");
        let decoded: Hello = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded, hello);
    }

    #[test]
    fn hello_tolerates_missing_client_id_field() {
        let raw = r#"{"schemaVersion":3,"supportedMinVersion":3}"#;
        let decoded: Hello = serde_json::from_str(raw).expect("deserialize without client_id");
        assert_eq!(decoded.client_id, "");
    }

    #[test]
    fn unknown_field_in_hello_payload_is_ignored_not_rejected() {
        // Forward compatibility: a future field appended by peer N+1
        // must not break peer N's parser.
        let raw = r#"{
            "schemaVersion": 3,
            "supportedMinVersion": 3,
            "clientId": "vapor-cli/test",
            "futureField": ["data", 42]
        }"#;
        let decoded: Hello = serde_json::from_str(raw).expect("deserialize with future field");
        assert_eq!(decoded.schema_version, 3);
    }

    #[test]
    fn request_envelope_round_trips_status_call() {
        let req = Request::Call {
            method: Method::Status,
        };
        let bytes = serde_json::to_vec(&req).expect("serialize");
        let decoded: Request = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(decoded, req);
    }

    #[test]
    fn response_envelope_round_trips_ok_and_err_variants() {
        let ok = Response::Ok(ResponseBody::Status(StatusResponse {
            schema_version: 1,
            daemon: "vapord/test".to_string(),
            provider: "Filesystem (stub)".to_string(),
            run: StateReason {
                state: "running".to_string(),
                reason: String::new(),
            },
            throttle: StateReason {
                state: "idle".to_string(),
                reason: "idle, plugged in, and cool".to_string(),
            },
            ..StatusResponse::default()
        }));
        let err = Response::Err(ErrorBody::PayloadTooLarge(8_388_608));

        let ok_bytes = serde_json::to_vec(&ok).expect("serialize");
        let err_bytes = serde_json::to_vec(&err).expect("serialize");
        assert_eq!(
            serde_json::from_slice::<Response>(&ok_bytes).expect("ok"),
            ok
        );
        assert_eq!(
            serde_json::from_slice::<Response>(&err_bytes).expect("err"),
            err
        );
    }

    #[test]
    fn status_payload_missing_later_fields_parses_with_defaults() {
        // Field-omission tolerance: a peer that omits fields added later
        // is read with defaults instead of failing.
        let raw = r#"{
            "schemaVersion": 4,
            "provider": "filesystem",
            "run": {"state": "running"},
            "throttle": {"state": "idle", "reason": "idle"}
        }"#;
        let decoded: StatusResponse = serde_json::from_str(raw).expect("payload parses");
        assert_eq!(decoded.run.state, "running");
        assert_eq!(decoded.queued, 0);
        assert!(decoded.profiles.is_empty());
        assert!(decoded.resources.is_none());
    }

    #[test]
    fn daemon_supported_versions_pulls_from_shared_constants() {
        let (current, min) = daemon_supported_versions();
        assert_eq!(current, constants::ipc::SCHEMA_VERSION_CURRENT);
        assert_eq!(min, constants::ipc::SCHEMA_VERSION_MIN_SUPPORTED);
    }
}
