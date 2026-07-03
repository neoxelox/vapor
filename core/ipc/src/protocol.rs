//! Wire-format types for the Vapor IPC channel.
//!
//! All payloads are JSON. Top-level types carry an explicit
//! `schema_version: u32` per `docs/architecture/ipc-contracts.md`. We
//! use `serde(default)` on every optional field so peer N-1 can omit
//! fields peer N introduced without forcing a hard parse error — the
//! "field omission tolerance" rule from the contracts doc.

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
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub schema_version: u32,
    pub supported_min_version: u32,
    /// Free-form identifier — e.g. `"vapor-cli/0.2.0-alpha.3"`. Not
    /// part of the contract; logged on the server side for diagnostics.
    #[serde(default)]
    pub client_id: String,
}

/// Server's reply to [`Hello`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HelloAck {
    pub schema_version: u32,
    pub supported_min_version: u32,
    #[serde(default)]
    pub server_id: String,
}

/// Returned in lieu of [`HelloAck`] when the version skew exceeds the
/// supported window. Mirrors the `IncompatibleVersion` error in the
/// contracts doc.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IncompatibleVersion {
    pub peer_version: u32,
    pub required_min: u32,
    pub local_version: u32,
}

/// Methods exposed by the daemon. Each variant is the name that goes
/// on the wire. Unknown method names are rejected with
/// `ErrorBody::MethodNotFound`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Method {
    /// Read the current daemon snapshot.
    Status,
    /// Flip `RunState` to `Paused` — idempotent.
    Pause,
    /// Flip `RunState` back to `Running` — idempotent.
    Resume,
    /// Hint the runtime to drain pending work as fast as the throttle
    /// allows. The engine already drains opportunistically; Wave 7
    /// ships this as an acknowledgement.
    FlushNow,
    /// Request a fresh whole-scope reconcile against the local sync
    /// directory.
    Reconcile,
    /// Read the (Wave 7) diagnostics timeline. Returns an empty list
    /// until the C8-30 in-memory timeline buffer ships.
    Timeline,
}

/// Top-level request envelope. Wave 6 phase 2 ships only the `Status`
/// method; later waves grow this enum.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum Request {
    Hello(Hello),
    Call { method: Method },
}

/// Successful response payloads, indexed by which method they answer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum ResponseBody {
    HelloAck(HelloAck),
    IncompatibleVersion(IncompatibleVersion),
    Status(StatusResponse),
    /// Generic acknowledgement for fire-and-forget control endpoints
    /// (Pause / Resume / FlushNow / Reconcile). The `accepted` flag
    /// reflects whether the request actually changed any state — Wave 7
    /// always returns `true` once the request reaches the runtime, but
    /// future versions may return `false` for no-op cases (e.g.
    /// pausing an already-paused daemon).
    Ack(AckResponse),
    Timeline(TimelineResponse),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AckResponse {
    pub schema_version: u32,
    pub accepted: bool,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub timestamp_ms: u64,
    pub kind: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TimelineResponse {
    pub schema_version: u32,
    #[serde(default)]
    pub entries: Vec<TimelineEntry>,
}

/// Wrapper response sent over the wire. Distinguishes success
/// (`Ok(body)`) from protocol-level failures (`Err(error)`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "value")]
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

/// `Status` method response.
///
/// Wave 6 phase 2 keeps this minimal — the runtime's `RunState`,
/// throttle state, and the last decision reason. Wave 7 expands it
/// (queue depth, ceilings, idle-boost state, etc.) per the contracts
/// doc.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StatusResponse {
    pub schema_version: u32,
    pub run_state: String,
    pub throttle_state: String,
    pub provider_name: String,
    /// Latest throttle reason string (`"idle, plugged in, and cool"`,
    /// etc.). Empty when no decision has been recorded yet.
    #[serde(default)]
    pub throttle_reason: String,
    /// Daemon identity string (`"vapord/<product version>"`). Purely
    /// diagnostic — lets clients report which daemon build answered.
    #[serde(default)]
    pub daemon_id: String,
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
        let raw = r#"{"schema_version":1,"supported_min_version":1}"#;
        let decoded: Hello = serde_json::from_str(raw).expect("deserialize without client_id");
        assert_eq!(decoded.client_id, "");
    }

    #[test]
    fn unknown_field_in_hello_payload_is_ignored_not_rejected() {
        // C5 forward-compat rule: a future field appended by peer N+1
        // must not break peer N's parser.
        let raw = r#"{
            "schema_version": 1,
            "supported_min_version": 1,
            "client_id": "vapor-cli/test",
            "future_field": ["data", 42]
        }"#;
        let decoded: Hello = serde_json::from_str(raw).expect("deserialize with future field");
        assert_eq!(decoded.schema_version, 1);
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
            run_state: "Running".to_string(),
            throttle_state: "IdleDrain".to_string(),
            provider_name: "Filesystem (stub)".to_string(),
            throttle_reason: "idle, plugged in, and cool".to_string(),
            daemon_id: "vapord/test".to_string(),
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
    fn daemon_supported_versions_pulls_from_shared_constants() {
        let (current, min) = daemon_supported_versions();
        assert_eq!(current, constants::ipc::SCHEMA_VERSION_CURRENT);
        assert_eq!(min, constants::ipc::SCHEMA_VERSION_MIN_SUPPORTED);
    }
}
