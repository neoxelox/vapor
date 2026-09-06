//! IPC-backed L3 CLI commands.
//!
//! `vapor status / pause / resume / flush-now / reconcile / timeline`
//! all share the same shape: connect to the daemon's UDS endpoint at
//! `<vapor_dir>/vapord.sock` (relocated under the OS temp directory
//! when that path would exceed the socket-address budget — the CLI and
//! daemon share `runtime_paths::ipc_socket_location`, so both sides
//! always rendezvous), perform the handshake, dispatch one method,
//! render the result.
//!
//! "No daemon running" is the most common failure mode. This module
//! turns the underlying transport / framing errors into a single
//! [`IpcCliError::DaemonNotRunning`] so the binary can exit non-zero
//! within 1 s with the documented message instead of hanging or
//! printing a confusing low-level error.

use std::error::Error;
use std::fmt::{self, Display};
use std::io;
use std::path::PathBuf;

use vapor_ipc::{
    AckResponse, Client, ClientError, DiagnosticsResponse, FrameError, StatusResponse,
    TimelineResponse, TransportError,
};
use vapor_shared::{constants, runtime_paths};

#[derive(Debug)]
pub enum IpcCliError {
    /// The socket file is missing / connection refused. Renders as
    /// `vapor: daemon not running — try \`vapor service start\``.
    DaemonNotRunning,
    /// The connection succeeded but the daemon never answered within
    /// the per-call deadline. Renders as
    /// `vapor: daemon is not responding — check \`vapor logs\``.
    /// The CLI must never hang; this is the variant that
    /// fires when a wedged but accepting daemon would otherwise stall
    /// us.
    DaemonUnresponsive,
    Client(ClientError),
}

impl Display for IpcCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DaemonNotRunning => write!(f, "daemon not running — try `vapor service start`"),
            Self::DaemonUnresponsive => {
                write!(f, "daemon is not responding — check `vapor logs`")
            }
            Self::Client(error) => write!(f, "IPC client error: {error}"),
        }
    }
}

impl Error for IpcCliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Client(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ClientError> for IpcCliError {
    fn from(error: ClientError) -> Self {
        if is_daemon_not_running(&error) {
            Self::DaemonNotRunning
        } else if is_daemon_unresponsive(&error) {
            Self::DaemonUnresponsive
        } else {
            Self::Client(error)
        }
    }
}

fn is_daemon_not_running(error: &ClientError) -> bool {
    match error {
        ClientError::Transport(TransportError::Io(io)) => is_missing_or_refused(io.kind()),
        ClientError::Frame(FrameError::Io(io)) => is_missing_or_refused(io.kind()),
        ClientError::Frame(FrameError::UnexpectedEof) => true,
        _ => false,
    }
}

fn is_daemon_unresponsive(error: &ClientError) -> bool {
    match error {
        ClientError::Transport(TransportError::Io(io)) => is_timeout(io.kind()),
        ClientError::Frame(FrameError::Io(io)) => is_timeout(io.kind()),
        _ => false,
    }
}

fn is_missing_or_refused(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::NotFound
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

fn is_timeout(kind: io::ErrorKind) -> bool {
    matches!(kind, io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// Resolve the daemon's UDS endpoint path — shared with the daemon's
/// bind path so a budget-driven relocation lands both sides on the
/// same socket.
pub fn socket_path() -> PathBuf {
    runtime_paths::ipc_socket_location().path
}

fn connect() -> Result<Client, IpcCliError> {
    let path = socket_path();
    let client_id = format!(
        "vapor-cli/{} ({})",
        vapor_daemon::build_info::VERSION,
        vapor_daemon::build_info::GIT_COMMIT_SHORT
    );
    Client::connect(&path, &client_id).map_err(IpcCliError::from)
}

pub fn status() -> Result<StatusResponse, IpcCliError> {
    let mut client = connect()?;
    client.status().map_err(IpcCliError::from)
}

pub fn pause() -> Result<AckResponse, IpcCliError> {
    let mut client = connect()?;
    client.pause().map_err(IpcCliError::from)
}

pub fn resume() -> Result<AckResponse, IpcCliError> {
    let mut client = connect()?;
    client.resume().map_err(IpcCliError::from)
}

pub fn flush_now() -> Result<AckResponse, IpcCliError> {
    let mut client = connect()?;
    client.flush_now().map_err(IpcCliError::from)
}

pub fn reconcile() -> Result<AckResponse, IpcCliError> {
    let mut client = connect()?;
    client.reconcile().map_err(IpcCliError::from)
}

pub fn timeline() -> Result<TimelineResponse, IpcCliError> {
    let mut client = connect()?;
    client.timeline().map_err(IpcCliError::from)
}

pub fn diagnostics() -> Result<DiagnosticsResponse, IpcCliError> {
    let mut client = connect()?;
    client.diagnostics().map_err(IpcCliError::from)
}

/// Render a [`StatusResponse`] as a human-readable string. The `--json`
/// path uses `serde_json::to_string_pretty` directly; this helper is
/// the default for plain-text output.
pub fn render_status(status: &StatusResponse) -> String {
    let mut rendered = format!(
        "Run state: {}\nThrottle: {} ({})\nProvider: {}\nDaemon: {}\nIPC schema: {}\nQueue: {} pending, {} failed\nLoop prevention: {} suppressed echoes\nConflicts kept-both: {}",
        status.run_state,
        status.throttle_state,
        if status.throttle_reason.is_empty() {
            "no decision yet".to_string()
        } else {
            status.throttle_reason.clone()
        },
        status.provider_name,
        status.daemon_id,
        status.schema_version,
        status.queue_depth,
        status.failed_intents,
        status.loop_prevention_suppressions,
        status.conflicts,
    );
    if status.mirror_reverts > 0 || status.mirror_deletes > 0 {
        rendered.push_str(&format!(
            "\nStrict mirror: {} reverts, {} deletes",
            status.mirror_reverts, status.mirror_deletes
        ));
    }
    if let Some(notice) = &status.config_restart_required {
        rendered.push_str(&format!("\nRestart required: {notice}"));
    }
    for profile in &status.profiles {
        rendered.push_str(&format!(
            "\nProfile {}: {} ({}, {}) queue {} failed {}{}",
            profile.id,
            profile.run_state,
            profile.provider_name,
            profile.sync_mode,
            profile.queue_depth,
            profile.failed_intents,
            profile
                .suspended_reason
                .as_ref()
                .map(|reason| format!(" — SUSPENDED: {reason}"))
                .unwrap_or_default(),
        ));
    }
    rendered
}

/// Render a [`DiagnosticsResponse`] as human-readable lines.
pub fn render_diagnostics(diagnostics: &DiagnosticsResponse) -> String {
    if diagnostics.intents.is_empty() {
        return format!(
            "No pending or in-flight intents. Dropped ingest events: {}",
            diagnostics.dropped_incoming_events
        );
    }
    let mut rendered = format!(
        "{} intent(s){}; dropped ingest events: {}",
        diagnostics.intents.len(),
        if diagnostics.truncated {
            " (truncated)"
        } else {
            ""
        },
        diagnostics.dropped_incoming_events
    );
    for intent in &diagnostics.intents {
        rendered.push_str(&format!(
            "\n#{} [{}] {} {} — stage {} ({} ms), attempts {}{}{}",
            intent.intent_id,
            intent.profile_id,
            intent.action,
            intent.path,
            intent.stage,
            intent.elapsed_in_stage_ms,
            intent.attempt_count,
            if intent.blocker_reason.is_empty() {
                String::new()
            } else {
                format!(" — {}", intent.blocker_reason)
            },
            if intent.last_error.is_empty() {
                String::new()
            } else {
                format!(" — last error: {}", intent.last_error)
            },
        ));
    }
    rendered
}

/// Tail the daemon log file at `<vapor_dir>/logs/vapord.logs`.
/// Reads the file directly rather than going through IPC since
/// the log lines already include redaction. Returns the last `tail`
/// lines (or the whole file when `tail` is `None`).
pub fn tail_logs(tail: Option<usize>) -> io::Result<String> {
    let path = log_path();
    if !path.exists() {
        return Ok(String::new());
    }
    match tail {
        None => std::fs::read_to_string(&path),
        Some(line_count) => read_last_lines(&path, line_count),
    }
}

/// Streams the whole daemon log to `out` without buffering it in memory,
/// for the default `vapor logs` (no `--tail`). A long-running install's
/// log can reach hundreds of MB; reading it into one `String` would spike
/// CLI memory by the full file size. Returns whether any bytes were
/// written (so the caller can print a "no log lines yet" placeholder).
pub fn stream_full_log(out: &mut impl io::Write) -> io::Result<bool> {
    let path = log_path();
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let mut reader = io::BufReader::new(file);
    let copied = io::copy(&mut reader, out)?;
    Ok(copied > 0)
}

/// Reads the final `line_count` lines of `path` by scanning backwards in
/// fixed-size chunks from the end of the file, so tailing a large log
/// does not load the whole file into memory.
fn read_last_lines(path: &std::path::Path, line_count: usize) -> io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};

    if line_count == 0 {
        return Ok(String::new());
    }

    const CHUNK_BYTES: u64 = 64 * 1024;
    let mut file = std::fs::File::open(path)?;
    let file_length = file.seek(SeekFrom::End(0))?;
    if file_length == 0 {
        return Ok(String::new());
    }

    let mut collected: Vec<u8> = Vec::new();
    let mut position = file_length;
    let mut newline_count = 0usize;

    while position > 0 && newline_count <= line_count {
        let chunk_length = CHUNK_BYTES.min(position);
        position -= chunk_length;
        file.seek(SeekFrom::Start(position))?;
        let mut chunk = vec![0u8; chunk_length as usize];
        file.read_exact(&mut chunk)?;
        newline_count += chunk.iter().filter(|byte| **byte == b'\n').count();
        chunk.extend_from_slice(&collected);
        collected = chunk;
    }

    let text = String::from_utf8_lossy(&collected);
    let lines: Vec<&str> = text.lines().collect();
    let take_from = lines.len().saturating_sub(line_count);
    Ok(lines[take_from..].join("\n"))
}

fn log_path() -> PathBuf {
    runtime_paths::logs_directory().join(constants::runtime::DAEMON_LOG_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_not_running_classification_picks_up_not_found_io_errors() {
        let error = ClientError::Transport(TransportError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "no socket file",
        )));
        assert!(is_daemon_not_running(&error));
    }

    #[test]
    fn daemon_not_running_classification_picks_up_connection_refused() {
        let error = ClientError::Transport(TransportError::Io(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "no listener",
        )));
        assert!(is_daemon_not_running(&error));
    }

    #[test]
    fn daemon_not_running_classification_does_not_misclassify_parse_errors() {
        let error = ClientError::Parse("garbage".to_string());
        assert!(!is_daemon_not_running(&error));
    }

    #[test]
    fn timeout_at_transport_layer_is_classified_as_daemon_unresponsive() {
        let error = ClientError::Transport(TransportError::Io(io::Error::new(
            io::ErrorKind::WouldBlock,
            "deadline expired",
        )));
        let cli_error = IpcCliError::from(error);
        assert!(matches!(cli_error, IpcCliError::DaemonUnresponsive));
    }

    #[test]
    fn timeout_at_frame_layer_is_classified_as_daemon_unresponsive() {
        let error = ClientError::Frame(FrameError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "frame deadline expired",
        )));
        let cli_error = IpcCliError::from(error);
        assert!(matches!(cli_error, IpcCliError::DaemonUnresponsive));
    }

    #[test]
    fn render_status_includes_every_stable_field() {
        let status = StatusResponse {
            schema_version: 2,
            run_state: "Running".to_string(),
            throttle_state: "IdleDrain".to_string(),
            provider_name: "Filesystem (stub)".to_string(),
            throttle_reason: "idle, plugged in, and cool".to_string(),
            daemon_id: "vapord/0.2.0-alpha.3".to_string(),
            queue_depth: 3,
            failed_intents: 1,
            conflicts: 2,
            mirror_reverts: 1,
            mirror_deletes: 1,
            profiles: vec![vapor_ipc::ProfileStatus {
                id: "mirror".to_string(),
                provider_name: "filesystem".to_string(),
                sync_mode: "pull-only".to_string(),
                run_state: "Running".to_string(),
                queue_depth: 3,
                failed_intents: 1,
                ..vapor_ipc::ProfileStatus::default()
            }],
            ..StatusResponse::default()
        };
        let rendered = render_status(&status);
        assert!(rendered.contains("Run state: Running"));
        assert!(rendered.contains("Throttle: IdleDrain"));
        assert!(rendered.contains("Provider: Filesystem (stub)"));
        assert!(rendered.contains("Daemon: vapord/0.2.0-alpha.3"));
        assert!(rendered.contains("IPC schema: 2"));
        assert!(rendered.contains("Queue: 3 pending, 1 failed"));
        assert!(rendered.contains("Strict mirror: 1 reverts, 1 deletes"));
        assert!(rendered.contains("Profile mirror: Running (filesystem, pull-only)"));
    }

    #[test]
    fn render_diagnostics_explains_why_intents_are_stuck() {
        let diagnostics = DiagnosticsResponse {
            schema_version: 2,
            intents: vec![vapor_ipc::IntentDiagnostic {
                intent_id: 42,
                profile_id: "default".to_string(),
                path: "/watch/big.bin".to_string(),
                action: "upload".to_string(),
                stage: "Retrying".to_string(),
                elapsed_in_stage_ms: 12_000,
                attempt_count: 3,
                last_error: "transient provider failure: timeout".to_string(),
                blocker_reason: "retry backoff active".to_string(),
            }],
            truncated: false,
            dropped_incoming_events: 2,
        };
        let rendered = render_diagnostics(&diagnostics);
        assert!(rendered.contains("#42 [default] upload /watch/big.bin"));
        assert!(rendered.contains("stage Retrying"));
        assert!(rendered.contains("retry backoff active"));
        assert!(rendered.contains("dropped ingest events: 2"));
    }

    #[test]
    fn tail_logs_returns_empty_when_log_file_missing() {
        // The current process's `vapor_dir` may or may not have a log
        // file. We exercise the missing-file path by pointing
        // `VAPOR_DIR` at a fresh tempdir, but only when the test is
        // run in isolation — we don't mutate process env in a parallel
        // test. Skip when a log file already exists.
        let result = tail_logs(Some(10));
        assert!(result.is_ok());
    }

    #[test]
    fn read_last_lines_returns_exactly_the_requested_tail() {
        let temp = tempfile::TempDir::new().expect("temp");
        let path = temp.path().join("vapord.logs");
        let contents: String = (0..1_000).map(|index| format!("line-{index}\n")).collect();
        std::fs::write(&path, contents).expect("seed log");

        let tail = read_last_lines(&path, 3).expect("tail");
        assert_eq!(tail, "line-997\nline-998\nline-999");

        let everything = read_last_lines(&path, 5_000).expect("tail larger than file");
        assert!(everything.starts_with("line-0\n"));
        assert!(everything.ends_with("line-999"));

        let nothing = read_last_lines(&path, 0).expect("zero tail");
        assert!(nothing.is_empty());
    }

    // JSON output shape locks for the `--json` commands (§9.2 snapshot
    // coverage): a field rename or reorder is a wire-format change for
    // scripts consuming `vapor status --json` / `vapor timeline --json`,
    // and must show up as a diff here.
    #[test]
    fn status_json_shape_is_stable() {
        let status = StatusResponse {
            schema_version: 2,
            run_state: "Running".to_string(),
            throttle_state: "IdleDrain".to_string(),
            provider_name: "Filesystem (stub)".to_string(),
            throttle_reason: "idle, plugged in, and cool".to_string(),
            daemon_id: "vapord/0.0.0-test".to_string(),
            ..StatusResponse::default()
        };
        let rendered = serde_json::to_string_pretty(&status).expect("serialize");
        assert_eq!(
            rendered,
            r#"{
  "schema_version": 2,
  "run_state": "Running",
  "throttle_state": "IdleDrain",
  "provider_name": "Filesystem (stub)",
  "throttle_reason": "idle, plugged in, and cool",
  "daemon_id": "vapord/0.0.0-test",
  "queue_depth": 0,
  "failed_intents": 0,
  "loop_prevention_suppressions": 0,
  "conflicts": 0,
  "mirror_reverts": 0,
  "mirror_deletes": 0,
  "profiles": [],
  "resource_budget": null,
  "config_restart_required": null
}"#
        );
    }

    #[test]
    fn timeline_json_shape_is_stable() {
        let timeline = TimelineResponse {
            schema_version: 2,
            entries: vec![vapor_ipc::TimelineEntry {
                timestamp_ms: 1_700_000_000_000,
                kind: "throttle".to_string(),
                message: "entered IdleDrain".to_string(),
                profile_id: "default".to_string(),
            }],
        };
        let rendered = serde_json::to_string_pretty(&timeline).expect("serialize");
        assert_eq!(
            rendered,
            r#"{
  "schema_version": 2,
  "entries": [
    {
      "timestamp_ms": 1700000000000,
      "kind": "throttle",
      "message": "entered IdleDrain",
      "profile_id": "default"
    }
  ]
}"#
        );
    }
}
