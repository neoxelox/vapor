//! Daemon-side IPC server runner.
//!
//! Binds the Unix-domain-socket listener at `<vapor_dir>/vapord.sock`
//! (relocated under the OS temp directory when that path would exceed
//! the socket-address budget, see `runtime_paths::ipc_socket_location`),
//! accepts connections in a background thread, and dispatches each
//! session to `vapor_ipc::serve_connection`. The daemon's main thread
//! keeps a [`IpcServerHandle`] so the listener (and its socket file)
//! drop on shutdown.
//!

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use vapor_ipc::Service;
// The connection handler is only wired up by the Unix serve loop below; the
// non-Unix `spawn` stub returns an error before serving anything.
#[cfg(unix)]
use vapor_ipc::serve_connection;
// `constants` is consumed only by the Unix `spawn` path (socket budget,
// connection caps); the non-Unix stub resolves paths via `runtime_paths`
// alone, so an unconditional import fails `-D warnings` on Windows.
#[cfg(unix)]
use vapor_shared::constants;
use vapor_shared::runtime_paths;

#[cfg(unix)]
use vapor_ipc::transport::{ListenerHandle, bind_listener};

/// Owns the listener thread and the bound socket. Drops the socket
/// file on shutdown.
pub struct IpcServerHandle {
    socket_path: PathBuf,
    #[cfg(unix)]
    _listener: ListenerHandle,
    _join: Option<thread::JoinHandle<()>>,
}

impl std::fmt::Debug for IpcServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcServerHandle")
            .field("socket_path", &self.socket_path)
            .finish()
    }
}

impl IpcServerHandle {
    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }
}

/// Resolve the IPC socket path for the current `vapor_dir`. Canonically
/// `<vapor_dir>/vapord.sock`; deterministically relocated under the OS
/// temp directory when that path exceeds the Unix socket-address budget
/// (see `runtime_paths::ipc_socket_location`).
pub fn resolve_socket_path() -> PathBuf {
    runtime_paths::ipc_socket_location().path
}

#[cfg(unix)]
pub fn spawn(service: Arc<dyn Service>) -> std::io::Result<IpcServerHandle> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let location = runtime_paths::ipc_socket_location();
    if let Some(canonical) = &location.relocated_from {
        crate::logging::info(
            "IPC socket path exceeds the Unix socket-address budget; relocated under the OS temp directory",
            &[
                ("canonical_path", canonical.display().to_string()),
                ("socket_path", location.path.display().to_string()),
                (
                    "budget_bytes",
                    constants::ipc::MAX_SOCKET_PATH_BYTES.to_string(),
                ),
            ],
        );
    }
    let socket_path = location.path;
    if let Some(parent) = socket_path.parent() {
        runtime_paths::ensure_private_directory(parent)?;
        // `ensure_private_directory` proved we may chmod the directory
        // (owner or root), but it follows symlinks. Refuse a symlinked
        // parent so another local user cannot pre-plant a redirect at
        // the well-known relocation path under a shared temp dir.
        if std::fs::symlink_metadata(parent)?.file_type().is_symlink() {
            return Err(std::io::Error::other(format!(
                "IPC socket parent {} is a symlink; refusing to bind through it",
                parent.display()
            )));
        }
    }
    let listener = bind_listener(socket_path.clone()).map_err(std::io::Error::other)?;
    let listener_clone = listener
        .listener()
        .try_clone()
        .map_err(std::io::Error::other)?;
    let service = service.clone();
    // Bounded connection handling: a cap on concurrent sessions plus a
    // per-connection idle read timeout, so misbehaving local clients can
    // neither park unbounded daemon threads nor hold one forever.
    let active_connections = Arc::new(AtomicUsize::new(0));
    let idle_timeout = Duration::from_millis(constants::ipc::CONNECTION_IDLE_TIMEOUT_MILLIS);
    let join = thread::Builder::new()
        .name("vapor-ipc".to_string())
        .spawn(move || {
            let mut consecutive_accept_errors: u32 = 0;
            for stream in listener_clone.incoming() {
                let stream = match stream {
                    Ok(stream) => {
                        consecutive_accept_errors = 0;
                        stream
                    }
                    Err(error) => {
                        // A persistent accept error (EMFILE/ENFILE under fd
                        // pressure) would otherwise busy-loop this thread at
                        // ~100% CPU with no diagnostic. Back off (10ms → 1s)
                        // and log the first few occurrences.
                        if consecutive_accept_errors < 3 {
                            crate::logging::warning(
                                "IPC accept failed; backing off before retrying",
                                &[("error", error.to_string())],
                            );
                        }
                        let backoff_ms = (10_u64 << consecutive_accept_errors.min(7)).min(1_000);
                        consecutive_accept_errors = consecutive_accept_errors.saturating_add(1);
                        thread::sleep(Duration::from_millis(backoff_ms));
                        continue;
                    }
                };
                if active_connections.load(Ordering::Acquire)
                    >= constants::ipc::MAX_CONCURRENT_CONNECTIONS
                {
                    crate::logging::warning(
                        "Dropped IPC connection: concurrent connection cap reached",
                        &[(
                            "max_connections",
                            constants::ipc::MAX_CONCURRENT_CONNECTIONS.to_string(),
                        )],
                    );
                    continue;
                }
                let _ = stream.set_read_timeout(Some(idle_timeout));
                // A write timeout too: without it a client that completes
                // the handshake and then stops reading parks a handler
                // thread forever in write_all, permanently consuming one of
                // the bounded connection slots.
                let _ = stream.set_write_timeout(Some(idle_timeout));
                let service = service.clone();
                // The slot is given back when the guard drops: at the
                // end of the session, when the thread cannot start, and
                // when a handler panics.
                let slot = ConnectionSlot::take(&active_connections);
                let _ = thread::Builder::new()
                    .name("vapor-ipc-conn".to_string())
                    .spawn(move || {
                        let _slot = slot;
                        let Ok(mut reader) = stream.try_clone() else {
                            return;
                        };
                        let mut writer = stream;
                        log_session_end(serve_connection(
                            &mut reader,
                            &mut writer,
                            service.as_ref(),
                        ));
                    });
            }
        })?;
    Ok(IpcServerHandle {
        socket_path,
        _listener: listener,
        _join: Some(join),
    })
}

/// One of the bounded connection slots, given back on drop.
#[cfg(unix)]
struct ConnectionSlot(Arc<std::sync::atomic::AtomicUsize>);

#[cfg(unix)]
impl ConnectionSlot {
    fn take(active: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
        active.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self(active.clone())
    }
}

#[cfg(unix)]
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Says why a session ended when the reason helps someone: a client
/// whose schema version is outside the window is the one line that
/// explains "the app cannot talk to the daemon" after an update of one
/// of the two.
#[cfg(unix)]
fn log_session_end(result: Result<(), vapor_ipc::ServeError>) {
    match result {
        Ok(()) => {}
        Err(vapor_ipc::ServeError::HandshakeIncompatible(violation)) => crate::logging::info(
            "Refused an IPC client: its schema version is outside the window this daemon supports",
            &[
                ("client_version", violation.peer_version.to_string()),
                ("daemon_version", violation.local_version.to_string()),
                ("required_min", violation.required_min.to_string()),
            ],
        ),
        Err(error) => crate::logging::debug(
            "An IPC session ended with an error",
            &[("error", error.to_string())],
        ),
    }
}

#[cfg(not(unix))]
pub fn spawn(_service: Arc<dyn Service>) -> std::io::Result<IpcServerHandle> {
    Err(std::io::Error::other(
        "IPC server is not supported on this OS yet; the Windows named-pipe transport has not shipped",
    ))
}
