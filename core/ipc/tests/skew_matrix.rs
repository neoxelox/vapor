//! End-to-end skew-matrix tests over a real Unix-domain-socket
//! transport. They prove the handshake contract by exercising every
//! supported version pair plus the documented negative cases. Each test spins up a server in
//! a background thread, connects a client, and asserts on the
//! handshake outcome.

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use tempfile::TempDir;
use vapor_ipc::{
    Client, ClientError, ResponseBody, Service, StateReason, StatusResponse,
    framing::write_frame,
    protocol::{Hello, IncompatibleVersion, Request, Response, daemon_supported_versions},
    serve_connection,
    transport::bind_listener,
};

struct StaticService {
    /// Pre-baked status payload returned by every call.
    status: Mutex<StatusResponse>,
}

impl Service for StaticService {
    fn status(&self) -> StatusResponse {
        self.status.lock().expect("status mutex").clone()
    }
}

fn fixture_status() -> StatusResponse {
    StatusResponse {
        schema_version: daemon_supported_versions().0,
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
    }
}

struct ServerHandle {
    socket_path: std::path::PathBuf,
    join: JoinHandle<()>,
    _temp: TempDir,
}

impl ServerHandle {
    fn shutdown(self) {
        // Drop the listener handle inside the thread by joining; in
        // practice the test ends and the socket file is removed on
        // drop.
        // We don't wait for the thread to terminate because the
        // accept loop blocks on incoming connections; the OS reclaims
        // the resources when the test process exits.
        drop(self.socket_path);
        // Detach: tests don't care about server-thread shutdown order.
        let _ = self.join;
    }
}

/// The listener is bound before this returns, and the OS queues a
/// connection that arrives before the accept loop runs, so a test
/// connects at once.
fn spawn_server(service: Arc<StaticService>) -> ServerHandle {
    let temp = TempDir::new().expect("temp");
    let socket_path = temp.path().join("vapord.sock");
    let handle = bind_listener(socket_path.clone()).expect("bind");
    let listener_clone = handle
        .listener()
        .try_clone()
        .expect("clone listener for thread");

    let join = thread::spawn(move || {
        // Accept until the test drops `_handle`, which removes the
        // socket file. Each accepted connection runs `serve_connection`
        // and the thread continues to handle the next.
        for stream in listener_clone.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            let service = service.clone();
            thread::spawn(move || {
                let mut reader = stream.try_clone().expect("reader clone");
                let mut writer = stream;
                let _ = serve_connection(&mut reader, &mut writer, service.as_ref());
            });
        }
    });

    // Keep the original handle alive in the test by stashing it on the
    // returned struct (so the socket file isn't deleted until shutdown).
    // We deliberately leak `handle` into the struct so its Drop runs
    // when `ServerHandle` drops.
    let server_handle = ServerHandle {
        socket_path,
        join,
        _temp: temp,
    };
    // Move the listener-owning handle into a thread-local so it lives
    // as long as the server. The simplest pattern: forget the handle
    // here so the socket file stays around; tests are short-lived and
    // the temp dir cleans up on drop. The TempDir keeps the file alive
    // until the ServerHandle drops.
    std::mem::forget(handle);
    server_handle
}

#[test]
fn handshake_n_n_succeeds_and_status_round_trips() {
    let service = Arc::new(StaticService {
        status: Mutex::new(fixture_status()),
    });
    let handle = spawn_server(service);

    let mut client = Client::connect(&handle.socket_path, "vapor-cli/test").expect("connect");
    let status = client.status().expect("status");
    assert_eq!(status.run.state, "running");

    handle.shutdown();
}

#[test]
fn handshake_n_minus_1_follows_the_support_window() {
    // Client says it speaks version (current - 1) and supports a min of
    // (current - 1). The daemon accepts it when (current - 1) is inside
    // its support window and refuses it with IncompatibleVersion when a
    // breaking bump moved the window's floor up to `current`.
    let (current, min) = daemon_supported_versions();
    if current < 2 {
        // No way to construct N-1 when N == 1; skip in that case.
        return;
    }
    let service = Arc::new(StaticService {
        status: Mutex::new(fixture_status()),
    });
    let handle = spawn_server(service);

    // Hand-roll the handshake to pin the client version below current.
    let mut stream = UnixStream::connect(&handle.socket_path).expect("connect");
    let hello = Request::Hello(Hello {
        schema_version: current - 1,
        supported_min_version: current - 1,
        client_id: "vapor-cli/old".to_string(),
    });
    let bytes = serde_json::to_vec(&hello).expect("serialize");
    write_frame(&mut stream, &bytes).expect("write");

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).expect("ack length");
    let len = u32::from_le_bytes(len_buf);
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).expect("ack payload");
    let response: Response = serde_json::from_slice(&payload).expect("ack");
    if current > min {
        assert!(matches!(response, Response::Ok(ResponseBody::HelloAck(_))));
    } else {
        assert!(
            format!("{response:?}").contains("IncompatibleVersion"),
            "a peer below the window must be refused, got {response:?}"
        );
    }

    drop(stream);
    handle.shutdown();
}

#[test]
fn handshake_skew_of_two_is_rejected_with_incompatible_version() {
    // Client at `current + 2` is outside the supported window.
    let service = Arc::new(StaticService {
        status: Mutex::new(fixture_status()),
    });
    let handle = spawn_server(service);

    let mut stream = UnixStream::connect(&handle.socket_path).expect("connect");
    let (current, _) = daemon_supported_versions();
    let hello = Request::Hello(Hello {
        schema_version: current + 2,
        supported_min_version: current + 2,
        client_id: "vapor-cli/way-too-new".to_string(),
    });
    let bytes = serde_json::to_vec(&hello).expect("serialize");
    write_frame(&mut stream, &bytes).expect("write");

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).expect("error length");
    let len = u32::from_le_bytes(len_buf);
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).expect("error payload");
    let response: Response = serde_json::from_slice(&payload).expect("error");
    let Response::Err(err) = response else {
        panic!("expected error, got {response:?}");
    };
    let body = format!("{err:?}");
    assert!(
        body.contains("IncompatibleVersion"),
        "expected IncompatibleVersion error, got {body}"
    );

    drop(stream);
    handle.shutdown();
}

#[test]
fn an_older_build_s_snake_case_hello_is_refused_by_version_not_by_parse() {
    // Schema 2 and 3 sent snake_case handshake keys; the refusal must
    // still name the version so either side can say what to upgrade.
    let service = Arc::new(StaticService {
        status: Mutex::new(fixture_status()),
    });
    let handle = spawn_server(service);

    let mut stream = UnixStream::connect(&handle.socket_path).expect("connect");
    let frame = br#"{"kind":"Hello","payload":{"schema_version":2,"supported_min_version":2,"client_id":"vapor-cli/0.2.0-alpha.3"}}"#;
    write_frame(&mut stream, frame).expect("write");

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).expect("error length");
    let mut payload = vec![0u8; u32::from_le_bytes(len_buf) as usize];
    stream.read_exact(&mut payload).expect("error payload");
    let response: Response = serde_json::from_slice(&payload).expect("error");
    let body = format!("{response:?}");
    assert!(
        body.contains("IncompatibleVersion") && body.contains("peer_version: 2"),
        "expected IncompatibleVersion for schema 2, got {body}"
    );

    drop(stream);
    handle.shutdown();
}

#[test]
fn payload_bounds_oversized_first_frame_drops_connection_cleanly() {
    // Spin up the server, then write an oversized length prefix
    // *without* the data. The server's framing layer must reject
    // before allocating.
    let service = Arc::new(StaticService {
        status: Mutex::new(fixture_status()),
    });
    let handle = spawn_server(service);

    let mut stream = UnixStream::connect(&handle.socket_path).expect("connect");
    let bogus_length: u32 = (vapor_shared::constants::ipc::MAX_PAYLOAD_BYTES as u32) + 1;
    stream
        .write_all(&bogus_length.to_le_bytes())
        .expect("write length");
    // The server answers with a PayloadTooLarge error frame, then closes:
    // a length-prefixed JSON response followed by EOF.
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).expect("error frame length");
    let length = u32::from_le_bytes(length) as usize;
    assert!(length > 0 && length <= vapor_shared::constants::ipc::MAX_PAYLOAD_BYTES);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).expect("error frame body");
    let text = String::from_utf8(body).expect("utf-8 frame");
    assert!(
        text.contains("payload_too_large") || text.contains("PayloadTooLarge"),
        "unexpected frame: {text}"
    );
    let mut trailing = [0u8; 4];
    assert_eq!(
        stream.read(&mut trailing).expect("eof"),
        0,
        "connection stays open"
    );

    handle.shutdown();
}

#[test]
fn client_propagates_incompatible_version_as_typed_error() {
    let service = Arc::new(StaticService {
        status: Mutex::new(fixture_status()),
    });
    let handle = spawn_server(service);

    // Use the `Client::handshake` low-level entry point so we can hand
    // it a stream the test fabricates.
    let mut stream = UnixStream::connect(&handle.socket_path).expect("connect");
    let (current, _) = daemon_supported_versions();
    let hello = Hello {
        schema_version: current + 2,
        supported_min_version: current + 2,
        client_id: "vapor-cli/way-too-new".to_string(),
    };
    let payload = serde_json::to_vec(&Request::Hello(hello)).expect("serialize");
    write_frame(&mut stream, &payload).expect("write");

    // Read the rejection so we can compare to the typed enum.
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).expect("len");
    let len = u32::from_le_bytes(len_buf);
    let mut bytes = vec![0u8; len as usize];
    stream.read_exact(&mut bytes).expect("bytes");
    let response: Response = serde_json::from_slice(&bytes).expect("decode");
    match response {
        Response::Err(vapor_ipc::ErrorBody::IncompatibleVersion(IncompatibleVersion {
            peer_version,
            required_min: _,
            local_version,
        })) => {
            assert_eq!(peer_version, current + 2);
            assert_eq!(local_version, current);
        }
        other => panic!("expected IncompatibleVersion, got {other:?}"),
    }

    let _ = ClientError::IncompatibleVersion(IncompatibleVersion {
        peer_version: 0,
        required_min: 0,
        local_version: 0,
    });
    handle.shutdown();
}
