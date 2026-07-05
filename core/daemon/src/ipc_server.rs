//! Daemon-side IPC server runner.
//!
//! Binds the Unix-domain-socket listener at `<vapor_dir>/vapord.sock`,
//! accepts connections in a background thread, and dispatches each
//! session to `vapor_ipc::serve_connection`. The daemon's main thread
//! keeps a [`IpcServerHandle`] so the listener (and its socket file)
//! drop on shutdown.
//!
//! Closes `core.md` C5-2.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use vapor_ipc::Service;
// The connection handler is only wired up by the Unix serve loop below; the
// non-Unix `spawn` stub returns an error before serving anything.
#[cfg(unix)]
use vapor_ipc::serve_connection;
use vapor_shared::{constants, runtime_paths};

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

/// Resolve the canonical IPC socket path for the current `vapor_dir`.
pub fn resolve_socket_path() -> PathBuf {
    runtime_paths::vapor_directory().join(constants::ipc::SOCKET_FILE_NAME)
}

#[cfg(unix)]
pub fn spawn(service: Arc<dyn Service>) -> std::io::Result<IpcServerHandle> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let socket_path = resolve_socket_path();
    if let Some(parent) = socket_path.parent() {
        runtime_paths::ensure_private_directory(parent)?;
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
            for stream in listener_clone.incoming() {
                let Ok(stream) = stream else {
                    continue;
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
                let service = service.clone();
                let connection_counter = active_connections.clone();
                connection_counter.fetch_add(1, Ordering::AcqRel);
                let spawned = thread::Builder::new()
                    .name("vapor-ipc-conn".to_string())
                    .spawn({
                        let connection_counter = connection_counter.clone();
                        move || {
                            let mut reader = match stream.try_clone() {
                                Ok(reader) => reader,
                                Err(_) => {
                                    connection_counter.fetch_sub(1, Ordering::AcqRel);
                                    return;
                                }
                            };
                            let mut writer = stream;
                            let _ = serve_connection(&mut reader, &mut writer, service.as_ref());
                            connection_counter.fetch_sub(1, Ordering::AcqRel);
                        }
                    });
                if spawned.is_err() {
                    connection_counter.fetch_sub(1, Ordering::AcqRel);
                }
            }
        })?;
    Ok(IpcServerHandle {
        socket_path,
        _listener: listener,
        _join: Some(join),
    })
}

#[cfg(not(unix))]
pub fn spawn(_service: Arc<dyn Service>) -> std::io::Result<IpcServerHandle> {
    Err(std::io::Error::other(
        "IPC server is not supported on this OS yet (Wave 12 / C6 named-pipe transport)",
    ))
}
