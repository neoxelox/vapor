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
    let socket_path = resolve_socket_path();
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = bind_listener(socket_path.clone()).map_err(std::io::Error::other)?;
    let listener_clone = listener
        .listener()
        .try_clone()
        .map_err(std::io::Error::other)?;
    let service = service.clone();
    let join = thread::Builder::new()
        .name("vapor-ipc".to_string())
        .spawn(move || {
            for stream in listener_clone.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                let service = service.clone();
                thread::Builder::new()
                    .name("vapor-ipc-conn".to_string())
                    .spawn(move || {
                        let mut reader = match stream.try_clone() {
                            Ok(reader) => reader,
                            Err(_) => return,
                        };
                        let mut writer = stream;
                        let _ = serve_connection(&mut reader, &mut writer, service.as_ref());
                    })
                    .ok();
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
    let socket_path = resolve_socket_path();
    Err(std::io::Error::other(
        "IPC server is not supported on this OS yet (Wave 12 / C6 named-pipe transport)",
    ))
}
