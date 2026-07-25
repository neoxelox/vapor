//! High-level client used by the `vapor` CLI and (eventually) by the
//! macOS / Windows / Linux apps.
//!
//! The surface is intentionally minimal: a handshake, a `status()`
//! call, and the IPC-driven control commands (`pause`, `resume`,
//! `flush-now`, etc.).

use std::error::Error;
use std::fmt::{self, Display};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::framing::{FrameError, read_frame, write_frame};
use crate::protocol::{
    AckResponse, DiagnosticsResponse, ErrorBody, Hello, IncompatibleVersion, Method, Request,
    Response, ResponseBody, StatusResponse, TimelineResponse, daemon_supported_versions,
};
use crate::transport::{StreamHandle, TransportError, connect_to_socket_with_timeout};

/// Default per-call deadline applied by [`Client::connect`]. Bounds the
/// CLI's "never hang" guarantee: even when the
/// daemon's accept loop is wedged (e.g., a connection-handler thread
/// died and the listener is no longer servicing) the client returns
/// within this window with a `WouldBlock` framing error that the CLI
/// classifies as "daemon not running" / "daemon unresponsive".
///
/// Conservative enough to absorb real daemon latency under load, tight
/// enough to satisfy the 1 s exit goal in the common case where
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
    /// Per-call budget. Applied as an *absolute* deadline that shrinks
    /// across every read/write of a single call (not a per-syscall
    /// timeout that re-arms on each partial read), so a peer trickling
    /// one byte per timeout window cannot keep a call alive unboundedly.
    timeout: Duration,
}

impl Client {
    /// Connects to the daemon listening at `socket_path`. Performs the
    /// handshake and returns a ready session. Applies
    /// [`DEFAULT_CALL_TIMEOUT`] to reads and writes so the client
    /// never hangs on a wedged daemon. The Windows transport is
    /// currently unsupported and returns [`TransportError::Unsupported`].
    pub fn connect(socket_path: &Path, client_id: &str) -> Result<Self, ClientError> {
        Self::connect_with_timeout(socket_path, client_id, DEFAULT_CALL_TIMEOUT)
    }

    /// Like [`Client::connect`] but uses an explicit per-call timeout.
    /// Pass `Duration::MAX` to opt out (e.g., a long-running streaming
    /// client). The timeout is an *absolute* deadline over each whole
    /// request/response, shrinking across every read/write, so a peer
    /// trickling bytes cannot keep a call alive past the budget.
    pub fn connect_with_timeout(
        socket_path: &Path,
        client_id: &str,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        // The deadline covers the connect phase too: with a wedged
        // accept loop and a full kernel backlog, a blocking AF_UNIX
        // connect never returns on Linux, and the read/write timeouts
        // below would never get the chance to arm.
        let connect_timeout = (timeout != Duration::MAX).then_some(timeout);
        let stream = connect_to_socket_with_timeout(socket_path, connect_timeout)?;
        apply_stream_timeout(&stream, timeout)?;
        let mut client = Self::handshake(stream, client_id)?;
        client.timeout = timeout;
        Ok(client)
    }

    /// Lower-level constructor that takes an already-open stream
    /// (useful for in-process tests against an `mpsc`-driven fake).
    pub fn handshake(stream: StreamHandle, client_id: &str) -> Result<Self, ClientError> {
        let mut client = Self {
            stream,
            timeout: DEFAULT_CALL_TIMEOUT,
        };
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
        // One absolute deadline for the whole request/response so a large
        // (up to MAX_PAYLOAD_BYTES) reply from a byte-trickling daemon
        // cannot keep the call alive past the budget.
        let mut framed = DeadlineStream::new(&self.stream, self.timeout);
        write_frame(&mut framed, &payload)?;

        let frame =
            read_frame(&mut framed)?.ok_or(ClientError::Frame(FrameError::UnexpectedEof))?;
        let response: Response = serde_json::from_slice(&frame)
            .map_err(|error| ClientError::Parse(error.to_string()))?;
        match response {
            Response::Ok(body) => Ok(body),
            Response::Err(error) => Err(ClientError::Server(error)),
        }
    }
}

/// Wraps a stream so each `read`/`write` re-applies the *remaining* time
/// until an absolute deadline as the socket timeout, and fails once the
/// budget is spent. This turns the per-syscall `SO_RCVTIMEO` (which
/// re-arms on every partial read) into a bound on the whole call.
struct DeadlineStream<'a> {
    stream: &'a StreamHandle,
    /// `None` opts out of any deadline (`Duration::MAX`).
    deadline: Option<Instant>,
}

impl<'a> DeadlineStream<'a> {
    fn new(stream: &'a StreamHandle, timeout: Duration) -> Self {
        let deadline = (timeout != Duration::MAX).then(|| Instant::now() + timeout);
        Self { stream, deadline }
    }

    /// Arms the socket with the time left, or fails if the deadline passed.
    fn arm(&self) -> io::Result<()> {
        let Some(deadline) = self.deadline else {
            return Ok(());
        };
        match deadline.checked_duration_since(Instant::now()) {
            Some(remaining) if !remaining.is_zero() => {
                self.stream.set_read_timeout(Some(remaining))?;
                self.stream.set_write_timeout(Some(remaining))?;
                Ok(())
            }
            _ => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "IPC call exceeded its deadline",
            )),
        }
    }
}

impl io::Read for DeadlineStream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.arm()?;
        (&*self.stream).read(buf)
    }
}

impl io::Write for DeadlineStream<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.arm()?;
        (&*self.stream).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self.stream).flush()
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
    // Windows IPC transport is stubbed. The future named-pipe
    // implementation will set per-call timeouts via the same
    // signature; until then the connect path returns `Unsupported`
    // before we reach this helper.
    Ok(())
}

// The only test here exercises the Unix-domain-socket connect path, so the
// whole module is Unix-only; on Windows it would otherwise leave `use super::*`
// unused, which `-D warnings` rejects. The future named-pipe transport will
// add its own `#[cfg(windows)]` tests.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn deadline_stream_fails_once_the_budget_is_spent_instead_of_blocking() {
        use std::io::Read;
        use std::os::unix::net::UnixStream;

        let (peer, _other_end) = UnixStream::pair().expect("socket pair");
        // A deadline already in the past: a read must fail TimedOut rather
        // than block on the (never-written) peer end.
        let mut framed = DeadlineStream {
            stream: &peer,
            deadline: Some(Instant::now() - Duration::from_secs(1)),
        };
        let mut buf = [0u8; 4];
        let error = framed
            .read(&mut buf)
            .expect_err("expired deadline must fail");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn connect_with_timeout_returns_promptly_against_wedged_peer() {
        // Invariant: the CLI must never hang against a daemon
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
        // 1 s budget is fine.
        assert!(
            elapsed < Duration::from_millis(900),
            "client took {elapsed:?} to give up against wedged peer; expected < 900 ms",
        );
    }
}
