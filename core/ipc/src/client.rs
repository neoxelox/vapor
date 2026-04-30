//! High-level client used by the `vapor` CLI and (eventually) by the
//! macOS / Windows / Linux apps.
//!
//! The Wave 6 phase 2 surface is intentionally minimal: a handshake +
//! a `status()` call. Wave 7 grows the trait once the IPC-driven
//! commands (`pause`, `resume`, `flush-now`, etc.) land.

use std::error::Error;
use std::fmt::{self, Display};
use std::path::Path;

use crate::framing::{FrameError, read_frame, write_frame};
use crate::protocol::{
    AckResponse, ErrorBody, Hello, IncompatibleVersion, Method, Request, Response, ResponseBody,
    StatusResponse, TimelineResponse, daemon_supported_versions,
};
use crate::transport::{StreamHandle, TransportError, connect_to_socket};

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
    /// handshake and returns a ready session. Pre-Wave-12 the Windows
    /// transport is unsupported and returns
    /// [`TransportError::Unsupported`].
    pub fn connect(socket_path: &Path, client_id: &str) -> Result<Self, ClientError> {
        let stream = connect_to_socket(socket_path)?;
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

        let frame = read_frame(&mut client.stream)?;
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

        let frame = read_frame(&mut self.stream)?;
        let response: Response = serde_json::from_slice(&frame)
            .map_err(|error| ClientError::Parse(error.to_string()))?;
        match response {
            Response::Ok(body) => Ok(body),
            Response::Err(error) => Err(ClientError::Server(error)),
        }
    }
}
