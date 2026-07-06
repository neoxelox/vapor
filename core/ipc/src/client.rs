//! High-level client used by the `vapor` CLI and (eventually) by the
//! macOS / Windows / Linux apps.
//!
//! The Wave 6 phase 2 surface is intentionally minimal: a handshake +
//! a `status()` call. Wave 7 grows the trait once the IPC-driven
//! commands (`pause`, `resume`, `flush-now`, etc.) land.

use std::error::Error;
use std::fmt::{self, Display};
use std::path::Path;
use std::time::Duration;

use crate::framing::{FrameError, read_frame, write_frame};
use crate::protocol::{
    AckResponse, DiagnosticsResponse, ErrorBody, Hello, IncompatibleVersion, Method, Request,
    Response, ResponseBody, StatusResponse, TimelineResponse, daemon_supported_versions,
};
use crate::transport::{StreamHandle, TransportError, connect_to_socket};

/// Default per-call deadline applied by [`Client::connect`]. Bounds the
/// CLI's "never hang" guarantee from `cli.md` L3-7: even when the
/// daemon's accept loop is wedged (e.g., a connection-handler thread
/// died and the listener is no longer servicing) the client returns
/// within this window with a `WouldBlock` framing error that the CLI
/// classifies as "daemon not running" / "daemon unresponsive".
///
/// Conservative enough to absorb real daemon latency under load, tight
/// enough to satisfy the L3-7 1 s exit goal in the common case where
/// the daemon is genuinely down (the OS returns `ECONNREFUSED` /
/// `ENOENT` immediately so we never hit this window in that path).
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub enum ClientError {
    Transport(TransportError),
    Frame(FrameError),
    Parse(String),
    /// The server returned an error response for the call.
    Server(ErrorBody),
    /// Handshake failed because the peer's version is outside our
    /// support window.
    IncompatibleVersion(IncompatibleVersion),
    /// We received a response shape that doesn't match the request.
    UnexpectedResponse(String),
}

impl Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "IPC transport: {error}"),
            Self::Frame(error) => write!(f, "IPC frame: {error}"),
            Self::Parse(reason) => write!(f, "IPC parse: {reason}"),
            Self::Server(body) => write!(f, "IPC server: {body:?}"),
            Self::IncompatibleVersion(version) => write!(
                f,
                "IPC incompatible version: peer={} required_min={}",
                version.peer_version, version.required_min
            ),
            Self::UnexpectedResponse(reason) => write!(f, "IPC unexpected response: {reason}"),
        }
    }
}

impl Error for ClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Frame(error) => Some(error),
            _ => None,
        }
    }
}

impl From<TransportError> for ClientError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

impl From<FrameError> for ClientError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

/// Owns one open IPC session. Performs the handshake on construction
/// so callers can rely on the connection being valid by the time the
/// constructor returns.
pub struct Client {
    stream: StreamHandle,
}

impl Client {
    /// Connects to the daemon listening at `socket_path`. Performs the
    /// handshake and returns a ready session. Applies
    /// [`DEFAULT_CALL_TIMEOUT`] to reads and writes so the client
    /// never hangs on a wedged daemon — see L3-7 in
    /// `docs/tasks/cli.md`. Pre-Wave-12 the Windows transport is
    /// unsupported and returns [`TransportError::Unsupported`].
    pub fn connect(socket_path: &Path, client_id: &str) -> Result<Self, ClientError> {
        Self::connect_with_timeout(socket_path, client_id, DEFAULT_CALL_TIMEOUT)
    }

    /// Like [`Client::connect`] but uses an explicit per-call timeout.
    /// Pass `Duration::MAX` to opt out (e.g., a long-running streaming
    /// client). The deadline bounds each individual `read` / `write`
    /// call rather than the whole session, but for the single-call
    /// CLI shape that is enough to honor L3-7.
    pub fn connect_with_timeout(
        socket_path: &Path,
        client_id: &str,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        let stream = connect_to_socket(socket_path)?;
        apply_stream_timeout(&stream, timeout)?;
        Self::handshake(stream, client_id)
    }

    /// Lower-level constructor that takes an already-open stream
    /// (useful for in-process tests against an `mpsc`-driven fake).
    pub fn handshake(stream: StreamHandle, client_id: &str) -> Result<Self, ClientError> {
        let mut client = Self { stream };
        let (current, min) = daemon_supported_versions();
        let hello = Hello {
            schema_version: current,
            supported_min_version: min,
            client_id: client_id.to_string(),
        };
        let payload = serde_json::to_vec(&Request::Hello(hello))
            .map_err(|error| ClientError::Parse(error.to_string()))?;
        write_frame(&mut client.stream, &payload)?;

        let frame =
            read_frame(&mut client.stream)?.ok_or(ClientError::Frame(FrameError::UnexpectedEof))?;
        let response: Response = serde_json::from_slice(&frame)
            .map_err(|error| ClientError::Parse(error.to_string()))?;
        match response {
            Response::Ok(ResponseBody::HelloAck(_)) => Ok(client),
            Response::Err(ErrorBody::IncompatibleVersion(version)) => {
                Err(ClientError::IncompatibleVersion(version))
            }
            Response::Err(other) => Err(ClientError::Server(other)),
            other => Err(ClientError::UnexpectedResponse(format!("{other:?}"))),
        }
    }

    pub fn status(&mut self) -> Result<StatusResponse, ClientError> {
        match self.call(Method::Status)? {
            ResponseBody::Status(value) => Ok(value),
            other => Err(ClientError::UnexpectedResponse(format!("{other:?}"))),
        }
    }

    pub fn pause(&mut self) -> Result<AckResponse, ClientError> {
        self.call_for_ack(Method::Pause)
    }

    pub fn resume(&mut self) -> Result<AckResponse, ClientError> {
        self.call_for_ack(Method::Resume)
    }

    pub fn flush_now(&mut self) -> Result<AckResponse, ClientError> {
        self.call_for_ack(Method::FlushNow)
    }

    pub fn reconcile(&mut self) -> Result<AckResponse, ClientError> {
        self.call_for_ack(Method::Reconcile)
    }

    pub fn diagnostics(&mut self) -> Result<DiagnosticsResponse, ClientError> {
        match self.call(Method::Diagnostics)? {
            ResponseBody::Diagnostics(diagnostics) => Ok(diagnostics),
            other => Err(ClientError::UnexpectedResponse(format!("{other:?}"))),
        }
    }

    pub fn set_auto_launch(&mut self, enabled: bool) -> Result<AckResponse, ClientError> {
        self.call_for_ack(Method::SetAutoLaunch { enabled })
    }

    pub fn update_excludes(
        &mut self,
        pre_ignore_rules: Option<String>,
        post_ignore_rules: Option<String>,
    ) -> Result<AckResponse, ClientError> {
        self.call_for_ack(Method::UpdateExcludes {
            pre_ignore_rules,
            post_ignore_rules,
        })
    }

    pub fn timeline(&mut self) -> Result<TimelineResponse, ClientError> {
        match self.call(Method::Timeline)? {
            ResponseBody::Timeline(value) => Ok(value),
            other => Err(ClientError::UnexpectedResponse(format!("{other:?}"))),
        }
    }

    fn call_for_ack(&mut self, method: Method) -> Result<AckResponse, ClientError> {
        match self.call(method)? {
            ResponseBody::Ack(value) => Ok(value),
            other => Err(ClientError::UnexpectedResponse(format!("{other:?}"))),
        }
    }

    fn call(&mut self, method: Method) -> Result<ResponseBody, ClientError> {
        let payload = serde_json::to_vec(&Request::Call { method })
            .map_err(|error| ClientError::Parse(error.to_string()))?;
        write_frame(&mut self.stream, &payload)?;

        let frame =
            read_frame(&mut self.stream)?.ok_or(ClientError::Frame(FrameError::UnexpectedEof))?;
        let response: Response = serde_json::from_slice(&frame)
            .map_err(|error| ClientError::Parse(error.to_string()))?;
        match response {
            Response::Ok(body) => Ok(body),
            Response::Err(error) => Err(ClientError::Server(error)),
        }
    }
}

#[cfg(unix)]
fn apply_stream_timeout(stream: &StreamHandle, timeout: Duration) -> Result<(), ClientError> {
    if timeout == Duration::MAX {
        return Ok(());
    }
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| ClientError::Transport(TransportError::Io(error)))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| ClientError::Transport(TransportError::Io(error)))?;
    Ok(())
}

#[cfg(windows)]
fn apply_stream_timeout(_stream: &StreamHandle, _timeout: Duration) -> Result<(), ClientError> {
    // Windows IPC transport is stubbed until Wave 12. The named-pipe
    // implementation will set per-call timeouts via the same
    // signature; until then the connect path returns `Unsupported`
    // before we reach this helper.
    Ok(())
}

// The only test here exercises the Unix-domain-socket connect path, so the
// whole module is Unix-only; on Windows it would otherwise leave `use super::*`
// unused, which `-D warnings` rejects. The named-pipe transport (Wave 12) will
// add its own `#[cfg(windows)]` tests.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn connect_with_timeout_returns_promptly_against_wedged_peer() {
        // L3-7 invariant: the CLI must never hang against a daemon
        // that accepted the connection but is not answering. We bind
        // a UDS, accept the client, then sit on the stream without
        // writing anything; `Client::connect_with_timeout` must
        // return inside a small window.
        use crate::transport::bind_listener;
        use std::time::Instant;
        use tempfile::TempDir;

        let temp = TempDir::new().expect("temp");
        let socket_path = temp.path().join("vapord.sock");
        let listener = bind_listener(socket_path.clone()).expect("bind");

        // Accept on a worker thread so the client's `connect` doesn't
        // race the listener. The accepted stream is held but never
        // written to, simulating a wedged daemon.
        let _accept = std::thread::spawn(move || {
            let _stream = listener.listener().accept().ok();
            std::thread::sleep(Duration::from_secs(2));
        });

        let started = Instant::now();
        let result = Client::connect_with_timeout(
            &socket_path,
            "vapor-cli/test",
            Duration::from_millis(200),
        );
        let elapsed = started.elapsed();

        assert!(
            result.is_err(),
            "expected timeout error, got Ok(_) — client did not honor deadline"
        );
        // Generous bound — the deadline is 200 ms, but CI can be slow
        // and the OS scheduler adds noise. Anything well under the
        // L3-7 1 s budget is fine.
        assert!(
            elapsed < Duration::from_millis(900),
            "client took {elapsed:?} to give up against wedged peer; expected < 900 ms",
        );
    }
}
