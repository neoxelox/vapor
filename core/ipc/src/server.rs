//! Server-side dispatch for the IPC channel.
//!
//! The daemon owns a [`Service`] implementation that knows how to
//! answer each [`Method`]. [`serve_connection`] owns one client
//! session: it performs the handshake, then loops reading framed
//! requests and writing framed responses until the peer disconnects or
//! an unrecoverable framing error occurs.

use std::error::Error;
use std::fmt::{self, Display};
use std::io::{Read, Write};

use crate::framing::{FrameError, read_frame, write_frame};
use crate::protocol::{
    AckResponse, ErrorBody, Hello, HelloAck, IncompatibleVersion, Method, Request, Response,
    ResponseBody, StatusResponse, TimelineResponse, daemon_supported_versions,
};

/// Implemented by the daemon to provide the data each method exposes.
/// Wave 7 grows the surface to cover the L3 IPC-driven CLI commands.
/// Every non-Status method has a default implementation returning an
/// `unsupported` ack so older daemon binaries that pre-date a method
/// can still answer the request without code changes.
pub trait Service: Send + Sync {
    fn status(&self) -> StatusResponse;

    fn pause(&self) -> AckResponse {
        unsupported_ack("pause")
    }

    fn resume(&self) -> AckResponse {
        unsupported_ack("resume")
    }

    fn flush_now(&self) -> AckResponse {
        unsupported_ack("flush_now")
    }

    fn reconcile(&self) -> AckResponse {
        unsupported_ack("reconcile")
    }

    fn timeline(&self) -> TimelineResponse {
        TimelineResponse {
            schema_version: daemon_supported_versions().0,
            entries: Vec::new(),
        }
    }
}

fn unsupported_ack(method: &str) -> AckResponse {
    AckResponse {
        schema_version: daemon_supported_versions().0,
        accepted: false,
        note: format!("{method} not implemented in this daemon build"),
    }
}

#[derive(Debug)]
pub enum ServeError {
    /// The peer sent something other than a `Hello` as its first frame.
    HandshakeMissing,
    /// The peer's version is outside the supported window.
    HandshakeIncompatible(IncompatibleVersion),
    /// Frame-level read / write error.
    Frame(FrameError),
    /// JSON parse failure on a request payload.
    Parse(String),
}

impl Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HandshakeMissing => {
                write!(
                    f,
                    "client sent a non-Hello frame before completing the handshake"
                )
            }
            Self::HandshakeIncompatible(version) => {
                write!(
                    f,
                    "client at version {} is outside the daemon support window (>= {})",
                    version.peer_version, version.required_min
                )
            }
            Self::Frame(error) => write!(f, "frame error: {error}"),
            Self::Parse(reason) => write!(f, "parse error: {reason}"),
        }
    }
}

impl Error for ServeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frame(error) => Some(error),
            _ => None,
        }
    }
}

impl From<FrameError> for ServeError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

/// Drives a single client connection to completion. Returns
/// `Ok(())` when the client cleanly disconnects (or sends EOF after a
/// clean response). Returns an error when the framing or handshake
/// layer fails — callers typically log the error and move on to the
/// next connection rather than tear the whole server down.
pub fn serve_connection<R, W>(
    reader: &mut R,
    writer: &mut W,
    service: &dyn Service,
) -> Result<(), ServeError>
where
    R: Read,
    W: Write,
{
    // 1. Handshake.
    let Some(first_frame) = read_next_frame(reader, writer)? else {
        // The client connected and left without speaking — clean close.
        return Ok(());
    };
    let request: Request = serde_json::from_slice(&first_frame)
        .map_err(|error| ServeError::Parse(error.to_string()))?;
    let Request::Hello(hello) = request else {
        let _ = send_response(
            writer,
            Response::Err(ErrorBody::HandshakeRequired(
                "first frame must be Hello".to_string(),
            )),
        );
        return Err(ServeError::HandshakeMissing);
    };

    let (current, min) = daemon_supported_versions();
    if let Some(violation) = check_skew(hello, current, min) {
        let response = Response::Err(ErrorBody::IncompatibleVersion(violation.clone()));
        let _ = send_response(writer, response);
        return Err(ServeError::HandshakeIncompatible(violation));
    }

    let ack = HelloAck {
        schema_version: current,
        supported_min_version: min,
        server_id: format!("vapord/{current}"),
    };
    send_response(writer, Response::Ok(ResponseBody::HelloAck(ack)))?;

    // 2. Per-method loop.
    loop {
        let Some(next) = read_next_frame(reader, writer)? else {
            // Clean EOF — the client closed at a frame boundary.
            return Ok(());
        };
        let request: Request = match serde_json::from_slice(&next) {
            Ok(value) => value,
            Err(error) => {
                let response =
                    Response::Err(ErrorBody::Backend(format!("invalid request: {error}")));
                send_response(writer, response)?;
                continue;
            }
        };
        match request {
            Request::Hello(_) => {
                let response = Response::Err(ErrorBody::Backend(
                    "Hello received after handshake completed".to_string(),
                ));
                send_response(writer, response)?;
            }
            Request::Call { method } => {
                let response = dispatch_method(service, method);
                send_response(writer, response)?;
            }
        }
    }
}

/// Reads the next frame; an oversized declaration is answered with the
/// contract's `PayloadTooLarge` error before the session is torn down, so
/// well-behaved clients see a typed error instead of a bare disconnect.
fn read_next_frame<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
) -> Result<Option<Vec<u8>>, ServeError> {
    match read_frame(reader) {
        Ok(frame) => Ok(frame),
        Err(FrameError::OversizedFrame { declared, max }) => {
            let _ = send_response(writer, Response::Err(ErrorBody::PayloadTooLarge(declared)));
            Err(ServeError::Frame(FrameError::OversizedFrame {
                declared,
                max,
            }))
        }
        Err(error) => Err(error.into()),
    }
}

fn dispatch_method(service: &dyn Service, method: Method) -> Response {
    match method {
        Method::Status => Response::Ok(ResponseBody::Status(service.status())),
        Method::Pause => Response::Ok(ResponseBody::Ack(service.pause())),
        Method::Resume => Response::Ok(ResponseBody::Ack(service.resume())),
        Method::FlushNow => Response::Ok(ResponseBody::Ack(service.flush_now())),
        Method::Reconcile => Response::Ok(ResponseBody::Ack(service.reconcile())),
        Method::Timeline => Response::Ok(ResponseBody::Timeline(service.timeline())),
    }
}

fn send_response<W: Write>(writer: &mut W, response: Response) -> Result<(), FrameError> {
    let bytes = serde_json::to_vec(&response).expect("Response always serializes");
    write_frame(writer, &bytes)
}

/// Returns `Some(violation)` if the peer's announced version is
/// outside the local support window. Pre-GA we enforce
/// `|peer - local| <= 1`, matching the contracts doc.
fn check_skew(hello: Hello, local_current: u32, local_min: u32) -> Option<IncompatibleVersion> {
    let peer = hello.schema_version;
    let peer_min = hello.supported_min_version;

    if peer < local_min {
        return Some(IncompatibleVersion {
            peer_version: peer,
            required_min: local_min,
            local_version: local_current,
        });
    }
    if peer_min > local_current {
        return Some(IncompatibleVersion {
            peer_version: peer,
            required_min: peer_min,
            local_version: local_current,
        });
    }
    let skew = peer.abs_diff(local_current);
    if skew > 1 {
        return Some(IncompatibleVersion {
            peer_version: peer,
            required_min: local_min,
            local_version: local_current,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Hello;
    use std::io::Cursor;

    struct StaticService {
        status: StatusResponse,
    }

    impl Service for StaticService {
        fn status(&self) -> StatusResponse {
            self.status.clone()
        }
    }

    fn fixture_status() -> StatusResponse {
        StatusResponse {
            schema_version: 1,
            run_state: "Running".to_string(),
            throttle_state: "IdleDrain".to_string(),
            provider_name: "Filesystem (stub)".to_string(),
            throttle_reason: "idle, plugged in, and cool".to_string(),
            daemon_id: "vapord/1".to_string(),
        }
    }

    fn write_request_frame<W: Write>(writer: &mut W, request: &Request) {
        let bytes = serde_json::to_vec(request).expect("serialize request");
        write_frame(writer, &bytes).expect("write frame");
    }

    fn read_response_frame<R: Read>(reader: &mut R) -> Response {
        let frame = read_frame(reader)
            .expect("read frame")
            .expect("frame present");
        serde_json::from_slice(&frame).expect("deserialize response")
    }

    #[test]
    fn oversized_request_frame_is_answered_with_payload_too_large() {
        let service = StaticService {
            status: fixture_status(),
        };
        // Complete the handshake first, then declare an oversized frame.
        let mut request_buf = Vec::new();
        write_request_frame(
            &mut request_buf,
            &Request::Hello(Hello {
                schema_version: 1,
                supported_min_version: 1,
                client_id: "vapor-cli/test".to_string(),
            }),
        );
        let bogus_length = (vapor_shared::constants::ipc::MAX_PAYLOAD_BYTES as u32) + 1;
        request_buf.extend_from_slice(&bogus_length.to_le_bytes());

        let mut reader = Cursor::new(request_buf);
        let mut response_buf = Vec::new();
        let error = serve_connection(&mut reader, &mut response_buf, &service)
            .expect_err("oversized frame must fail the session");
        assert!(matches!(
            error,
            ServeError::Frame(FrameError::OversizedFrame { .. })
        ));

        let mut response_reader = Cursor::new(response_buf);
        let _ack = read_response_frame(&mut response_reader);
        let response = read_response_frame(&mut response_reader);
        assert!(matches!(
            response,
            Response::Err(ErrorBody::PayloadTooLarge(declared)) if declared == bogus_length
        ));
    }

    #[test]
    fn silent_client_disconnect_before_hello_is_a_clean_close() {
        let service = StaticService {
            status: fixture_status(),
        };
        let mut reader = Cursor::new(Vec::new());
        let mut response_buf = Vec::new();
        serve_connection(&mut reader, &mut response_buf, &service)
            .expect("empty session closes cleanly");
        assert!(response_buf.is_empty());
    }

    #[test]
    fn handshake_succeeds_at_matching_versions_and_serves_status() {
        let service = StaticService {
            status: fixture_status(),
        };
        let mut request_buf = Vec::new();
        write_request_frame(
            &mut request_buf,
            &Request::Hello(Hello {
                schema_version: 1,
                supported_min_version: 1,
                client_id: "vapor-cli/test".to_string(),
            }),
        );
        write_request_frame(
            &mut request_buf,
            &Request::Call {
                method: Method::Status,
            },
        );

        let mut reader = Cursor::new(request_buf);
        let mut response_buf = Vec::new();
        serve_connection(&mut reader, &mut response_buf, &service).expect("serve");

        let mut response_reader = Cursor::new(response_buf);
        let ack = read_response_frame(&mut response_reader);
        assert!(matches!(ack, Response::Ok(ResponseBody::HelloAck(_))));
        let status = read_response_frame(&mut response_reader);
        match status {
            Response::Ok(ResponseBody::Status(value)) => {
                assert_eq!(value.run_state, "Running");
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn handshake_rejects_peer_two_versions_ahead_with_incompatible_error() {
        let service = StaticService {
            status: fixture_status(),
        };
        let mut request_buf = Vec::new();
        write_request_frame(
            &mut request_buf,
            &Request::Hello(Hello {
                schema_version: 3,
                supported_min_version: 3,
                client_id: "vapor-cli/future".to_string(),
            }),
        );

        let mut reader = Cursor::new(request_buf);
        let mut response_buf = Vec::new();
        let error =
            serve_connection(&mut reader, &mut response_buf, &service).expect_err("incompatible");
        assert!(matches!(error, ServeError::HandshakeIncompatible(_)));

        let mut response_reader = Cursor::new(response_buf);
        let response = read_response_frame(&mut response_reader);
        assert!(matches!(
            response,
            Response::Err(ErrorBody::IncompatibleVersion(_))
        ));
    }

    #[test]
    fn handshake_rejects_peer_two_versions_behind_with_incompatible_error() {
        // Daemon at v1; peer sets supported_min_version=3 → it can't
        // talk to anything below 3, but the daemon is at 1. Skew check
        // catches it.
        let service = StaticService {
            status: fixture_status(),
        };
        let mut request_buf = Vec::new();
        write_request_frame(
            &mut request_buf,
            &Request::Hello(Hello {
                schema_version: 1,
                supported_min_version: 3,
                client_id: "vapor-cli/test".to_string(),
            }),
        );
        let mut reader = Cursor::new(request_buf);
        let mut response_buf = Vec::new();
        let error =
            serve_connection(&mut reader, &mut response_buf, &service).expect_err("incompatible");
        assert!(matches!(error, ServeError::HandshakeIncompatible(_)));
    }

    #[test]
    fn handshake_accepts_peer_one_version_ahead() {
        // Pre-GA tolerance is `|N - M| <= 1`. Daemon at 1, peer at 2
        // is allowed.
        let service = StaticService {
            status: fixture_status(),
        };
        let mut request_buf = Vec::new();
        write_request_frame(
            &mut request_buf,
            &Request::Hello(Hello {
                schema_version: 2,
                supported_min_version: 1,
                client_id: "vapor-cli/next".to_string(),
            }),
        );
        write_request_frame(
            &mut request_buf,
            &Request::Call {
                method: Method::Status,
            },
        );

        let mut reader = Cursor::new(request_buf);
        let mut response_buf = Vec::new();
        serve_connection(&mut reader, &mut response_buf, &service).expect("serve");

        let mut response_reader = Cursor::new(response_buf);
        let ack = read_response_frame(&mut response_reader);
        assert!(matches!(ack, Response::Ok(ResponseBody::HelloAck(_))));
        let status = read_response_frame(&mut response_reader);
        assert!(matches!(status, Response::Ok(ResponseBody::Status(_))));
    }

    #[test]
    fn non_hello_first_frame_returns_handshake_required_error() {
        let service = StaticService {
            status: fixture_status(),
        };
        let mut request_buf = Vec::new();
        write_request_frame(
            &mut request_buf,
            &Request::Call {
                method: Method::Status,
            },
        );

        let mut reader = Cursor::new(request_buf);
        let mut response_buf = Vec::new();
        let error = serve_connection(&mut reader, &mut response_buf, &service)
            .expect_err("handshake missing");
        assert!(matches!(error, ServeError::HandshakeMissing));

        let mut response_reader = Cursor::new(response_buf);
        let response = read_response_frame(&mut response_reader);
        assert!(matches!(
            response,
            Response::Err(ErrorBody::HandshakeRequired(_))
        ));
    }

    #[test]
    fn second_hello_after_handshake_is_reported_as_backend_error() {
        let service = StaticService {
            status: fixture_status(),
        };
        let mut request_buf = Vec::new();
        write_request_frame(
            &mut request_buf,
            &Request::Hello(Hello {
                schema_version: 1,
                supported_min_version: 1,
                client_id: "vapor-cli/test".to_string(),
            }),
        );
        write_request_frame(
            &mut request_buf,
            &Request::Hello(Hello {
                schema_version: 1,
                supported_min_version: 1,
                client_id: "vapor-cli/test".to_string(),
            }),
        );

        let mut reader = Cursor::new(request_buf);
        let mut response_buf = Vec::new();
        serve_connection(&mut reader, &mut response_buf, &service).expect("serve");

        let mut response_reader = Cursor::new(response_buf);
        let _ack = read_response_frame(&mut response_reader);
        let response = read_response_frame(&mut response_reader);
        assert!(matches!(response, Response::Err(ErrorBody::Backend(_))));
    }

    #[test]
    fn forward_compatibility_unknown_field_in_hello_does_not_break_handshake() {
        // Synthesize a Hello payload with an extra `future_field`.
        let payload = br#"{
            "kind": "Hello",
            "payload": {
                "schema_version": 1,
                "supported_min_version": 1,
                "client_id": "vapor-cli/future",
                "future_field": "not yet defined"
            }
        }"#;
        let mut request_buf = Vec::new();
        write_frame(&mut request_buf, payload).expect("write");
        write_request_frame(
            &mut request_buf,
            &Request::Call {
                method: Method::Status,
            },
        );

        let service = StaticService {
            status: fixture_status(),
        };
        let mut reader = Cursor::new(request_buf);
        let mut response_buf = Vec::new();
        serve_connection(&mut reader, &mut response_buf, &service).expect("serve");

        let mut response_reader = Cursor::new(response_buf);
        let ack = read_response_frame(&mut response_reader);
        assert!(matches!(ack, Response::Ok(ResponseBody::HelloAck(_))));
    }
}
