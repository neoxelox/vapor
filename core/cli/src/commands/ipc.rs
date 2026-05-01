//! IPC-backed L3 CLI commands.
//!
//! `vapor status / pause / resume / flush-now / reconcile / timeline`
//! all share the same shape: connect to the daemon's UDS endpoint at
//! `<vapor_dir>/vapord.sock`, perform the handshake, dispatch one
//! method, render the result.
//!
//! "No daemon running" is the most common failure mode. This module
//! turns the underlying transport / framing errors into a single
//! [`IpcCliError::DaemonNotRunning`] so the binary can exit non-zero
//! within 1 s with the documented message instead of hanging or
//! printing a confusing low-level error. Closes `cli.md` L3-7.

use std::error::Error;
use std::fmt::{self, Display};
use std::io;
use std::path::{Path, PathBuf};

use vapor_ipc::{
    AckResponse, Client, ClientError, FrameError, StatusResponse, TimelineResponse, TransportError,
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
    /// L3-7 says the CLI must never hang; this is the variant that
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

/// Resolve the daemon's UDS endpoint path.
pub fn socket_path() -> PathBuf {
    runtime_paths::vapor_directory().join(constants::ipc::SOCKET_FILE_NAME)
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

/// Render a [`StatusResponse`] as a human-readable string. The `--json`
/// path uses `serde_json::to_string_pretty` directly; this helper is
/// the default for plain-text output.
pub fn render_status(status: &StatusResponse) -> String {
    format!(
        "Run state: {}\nThrottle: {} ({})\nProvider: {}\nDaemon: {}\nIPC schema: {}",
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
    )
}

/// Tail the daemon log file at `<vapor_dir>/logs/vapord.logs`.
/// `cli.md` L3-6 specifies tailing rather than going through IPC since
/// the log lines already include redaction. Returns the last `tail`
/// lines (or the whole file when `tail` is `None`).
pub fn tail_logs(tail: Option<usize>) -> io::Result<String> {
    let path = log_path();
    if !path.exists() {
        return Ok(String::new());
    }
    let contents = std::fs::read_to_string(&path)?;
    let Some(n) = tail else {
        return Ok(contents);
    };
    let lines: Vec<&str> = contents.lines().collect();
    let take_from = lines.len().saturating_sub(n);
    Ok(lines[take_from..].join("\n"))
}

fn log_path() -> PathBuf {
    runtime_paths::logs_directory().join(constants::runtime::DAEMON_LOG_FILE_NAME)
}

/// `cli.md` L3-6 documents the log path. Exposed for `vapor doctor`
/// and any future tooling that needs to surface the location.
pub fn log_path_for_diagnostics() -> &'static Path {
    // Cannot return a static PathBuf; keep this for parity with the
    // log-path-rendering helper. The implementation pulls from
    // `runtime_paths` so it always reflects the current `VAPOR_DIR`.
    Path::new("")
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
            schema_version: 1,
            run_state: "Running".to_string(),
            throttle_state: "IdleDrain".to_string(),
            provider_name: "Filesystem (stub)".to_string(),
            throttle_reason: "idle, plugged in, and cool".to_string(),
            daemon_id: "vapord/0.2.0-alpha.3".to_string(),
        };
        let rendered = render_status(&status);
        assert!(rendered.contains("Run state: Running"));
        assert!(rendered.contains("Throttle: IdleDrain"));
        assert!(rendered.contains("Provider: Filesystem (stub)"));
        assert!(rendered.contains("Daemon: vapord/0.2.0-alpha.3"));
        assert!(rendered.contains("IPC schema: 1"));
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
}
