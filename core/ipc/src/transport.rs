//! Per-OS transport implementations.
//!
//! Unix uses `std::os::unix::net::{UnixListener, UnixStream}` against a
//! socket file at `<vapor_dir>/vapord.sock`. Windows is stubbed for
//! Wave 12; calling `bind_listener` / `connect_to_socket` returns
//! [`TransportError::Unsupported`] until the named-pipe transport
//! ships.

use std::error::Error;
use std::fmt::{self, Display};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum TransportError {
    Io(io::Error),
    /// The current OS doesn't have a transport implementation yet.
    Unsupported(&'static str),
}

impl Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "IPC transport I/O: {error}"),
            Self::Unsupported(reason) => write!(f, "IPC transport unsupported: {reason}"),
        }
    }
}

impl Error for TransportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for TransportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(unix)]
pub use unix_impl::{ListenerHandle, StreamHandle, bind_listener, connect_to_socket};

#[cfg(windows)]
pub use windows_impl::{ListenerHandle, StreamHandle, bind_listener, connect_to_socket};

#[cfg(unix)]
mod unix_impl {
    use super::{Path, PathBuf, TransportError};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};

    use vapor_shared::constants;

    /// Owns the bound listener socket and removes the socket file on
    /// drop, matching the launchd-managed daemon's expected lifecycle.
    #[derive(Debug)]
    pub struct ListenerHandle {
        listener: UnixListener,
        socket_path: PathBuf,
    }

    impl ListenerHandle {
        pub fn listener(&self) -> &UnixListener {
            &self.listener
        }

        pub fn socket_path(&self) -> &Path {
            &self.socket_path
        }
    }

    impl Drop for ListenerHandle {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.socket_path);
        }
    }

    /// Type alias for the active server-side connection. Uses the std
    /// `UnixStream` directly so callers can split it for read / write.
    pub type StreamHandle = UnixStream;

    pub fn bind_listener(socket_path: PathBuf) -> Result<ListenerHandle, TransportError> {
        // The parent directory is created 0o700 *before* the socket file
        // exists, which closes the bind-then-chmod window: even while the
        // freshly-bound socket briefly carries umask-default permissions,
        // no other local user can traverse into the directory to reach
        // it. The socket chmod below stays as defense in depth.
        if let Some(parent) = socket_path.parent() {
            vapor_shared::runtime_paths::ensure_private_directory(parent)?;
        }
        // launchctl + crash recovery may leave a stale socket file
        // around. Remove it before binding so the daemon can come up
        // again without a manual `rm`. Safe against a *live* daemon's
        // socket because the daemon singleton lock
        // (`core/daemon::singleton`) is acquired before any bind.
        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
        }
        let listener = UnixListener::bind(&socket_path)?;
        // Restrict the socket to the owning user. On a multi-user host
        // any local user could otherwise connect to the daemon's IPC
        // and issue control commands. Matches the policy stated in
        // `docs/architecture/macos/ipc-transport.md`.
        fs::set_permissions(
            &socket_path,
            fs::Permissions::from_mode(constants::runtime::PRIVATE_FILE_MODE),
        )?;
        Ok(ListenerHandle {
            listener,
            socket_path,
        })
    }

    pub fn connect_to_socket(socket_path: &Path) -> Result<UnixStream, TransportError> {
        Ok(UnixStream::connect(socket_path)?)
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::{Path, PathBuf, TransportError};

    /// Stub. Wave 12 (`core.md` C6` named-pipe transport) replaces this
    /// with a real `\\.\pipe\vapord-<user-sid>` listener.
    #[derive(Debug)]
    pub struct ListenerHandle {
        socket_path: PathBuf,
    }

    impl ListenerHandle {
        pub fn socket_path(&self) -> &Path {
            &self.socket_path
        }
    }

    pub type StreamHandle = std::net::TcpStream;

    pub fn bind_listener(_socket_path: PathBuf) -> Result<ListenerHandle, TransportError> {
        Err(TransportError::Unsupported(
            "Windows IPC named-pipe transport not implemented yet (Wave 12 / C6)",
        ))
    }

    pub fn connect_to_socket(_socket_path: &Path) -> Result<StreamHandle, TransportError> {
        Err(TransportError::Unsupported(
            "Windows IPC named-pipe transport not implemented yet (Wave 12 / C6)",
        ))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn bind_listener_restricts_socket_permissions_to_owner_only() {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::TempDir;
        use vapor_shared::constants;

        let temp = TempDir::new().expect("temp");
        let socket_path = temp.path().join("vapord.sock");
        let handle = bind_listener(socket_path.clone()).expect("bind");

        let mode = std::fs::metadata(handle.socket_path())
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, constants::runtime::PRIVATE_FILE_MODE);
    }

    #[cfg(unix)]
    #[test]
    fn unix_listener_round_trips_a_short_message_through_the_socket() {
        use super::*;
        use std::io::{Read, Write};
        use tempfile::TempDir;

        let temp = TempDir::new().expect("temp");
        let socket_path = temp.path().join("vapord.sock");
        let handle = bind_listener(socket_path.clone()).expect("bind");

        let join = std::thread::spawn({
            let socket_path = socket_path.clone();
            move || {
                let mut stream = connect_to_socket(&socket_path).expect("connect");
                stream.write_all(b"ping").expect("write");
                let mut buf = [0u8; 4];
                stream.read_exact(&mut buf).expect("read");
                buf
            }
        });

        let (mut server_stream, _) = handle.listener().accept().expect("accept");
        let mut buf = [0u8; 4];
        server_stream.read_exact(&mut buf).expect("read");
        assert_eq!(&buf, b"ping");
        server_stream.write_all(b"pong").expect("write");
        let response = join.join().expect("client thread");
        assert_eq!(&response, b"pong");
    }
}
