//! Per-OS transport implementations.
//!
//! Unix uses `std::os::unix::net::{UnixListener, UnixStream}` against a
//! socket file at `<vapor_dir>/vapord.sock`. Windows is stubbed:
//! calling `bind_listener` / `connect_to_socket` returns
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
    /// The socket at the resolved path is not owned by the current user.
    /// On a multi-user host with a world-writable temp dir (the
    /// deterministic relocation target), another user could pre-create
    /// the socket and impersonate the daemon; refuse to talk to it.
    ForeignSocket {
        path: PathBuf,
        owner_uid: u32,
    },
    /// The connect phase itself exceeded the caller's deadline. Happens
    /// when the daemon's accept loop is wedged and the kernel backlog is
    /// full: on Linux a blocking `connect(2)` on AF_UNIX then blocks
    /// indefinitely (macOS fails fast with ECONNREFUSED), which would
    /// otherwise hang the client before its read/write timeouts even
    /// arm.
    ConnectTimeout {
        path: PathBuf,
        timeout: std::time::Duration,
    },
}

impl Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "IPC transport I/O: {error}"),
            Self::Unsupported(reason) => write!(f, "IPC transport unsupported: {reason}"),
            Self::ForeignSocket { path, owner_uid } => write!(
                f,
                "refusing to connect to IPC socket {} owned by uid {owner_uid}, not the current user",
                path.display()
            ),
            Self::ConnectTimeout { path, timeout } => write!(
                f,
                "connecting to IPC socket {} exceeded the {:?} deadline",
                path.display(),
                timeout
            ),
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
pub use unix_impl::{
    ListenerHandle, StreamHandle, bind_listener, connect_to_socket, connect_to_socket_with_timeout,
};

#[cfg(windows)]
pub use windows_impl::{
    ListenerHandle, StreamHandle, bind_listener, connect_to_socket, connect_to_socket_with_timeout,
};

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
        connect_to_socket_with_timeout(socket_path, None)
    }

    /// Like [`connect_to_socket`] but bounds the connect phase itself.
    /// A blocking AF_UNIX `connect(2)` is unbounded on Linux once the
    /// listener's accept backlog fills (a wedged daemon accept loop), so
    /// the deadline the caller applies to reads/writes must also cover
    /// the connect. The connect runs on a helper thread joined with the
    /// timeout; on expiry the thread is abandoned (it exits when the
    /// kernel eventually resolves the connect and the stream drops).
    pub fn connect_to_socket_with_timeout(
        socket_path: &Path,
        timeout: Option<std::time::Duration>,
    ) -> Result<UnixStream, TransportError> {
        // Verify the socket is ours before connecting. The server side is
        // protected by a 0o700 parent + 0o600 socket, but the client must
        // not blindly trust whatever sits at the resolved path: under the
        // deterministic temp-dir relocation, a foreign user on a
        // world-writable /tmp could pre-create the socket and impersonate
        // the daemon (forged status/acks, leaked UpdateExcludes contents).
        use std::os::unix::fs::MetadataExt;
        let owner_uid = fs::metadata(socket_path)?.uid();
        verify_socket_owner(socket_path, owner_uid, current_euid())?;
        let Some(timeout) = timeout else {
            return Ok(UnixStream::connect(socket_path)?);
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let connect_path = socket_path.to_path_buf();
        std::thread::spawn(move || {
            let _ = sender.send(UnixStream::connect(&connect_path));
        });
        match receiver.recv_timeout(timeout) {
            Ok(result) => Ok(result?),
            Err(_) => Err(TransportError::ConnectTimeout {
                path: socket_path.to_path_buf(),
                timeout,
            }),
        }
    }

    /// The ownership gate as a pure decision so both branches are
    /// testable without a privileged chown.
    pub(super) fn verify_socket_owner(
        socket_path: &Path,
        owner_uid: u32,
        current_uid: u32,
    ) -> Result<(), TransportError> {
        if owner_uid == current_uid {
            Ok(())
        } else {
            Err(TransportError::ForeignSocket {
                path: socket_path.to_path_buf(),
                owner_uid,
            })
        }
    }

    /// The current process's effective uid. Isolated so the crate's
    /// single FFI call has the narrowest possible unsafe surface.
    #[allow(unsafe_code)]
    fn current_euid() -> u32 {
        // SAFETY: `geteuid` takes no arguments, cannot fail, and has no
        // side effects.
        unsafe { libc::geteuid() }
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::{Path, PathBuf, TransportError};

    /// Stub. The named-pipe transport replaces this
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
            "the Windows named-pipe transport has not shipped yet",
        ))
    }

    pub fn connect_to_socket_with_timeout(
        _socket_path: &Path,
        _timeout: Option<std::time::Duration>,
    ) -> Result<StreamHandle, TransportError> {
        Err(TransportError::Unsupported(
            "the Windows named-pipe transport has not shipped yet",
        ))
    }

    pub fn connect_to_socket(_socket_path: &Path) -> Result<StreamHandle, TransportError> {
        Err(TransportError::Unsupported(
            "the Windows named-pipe transport has not shipped yet",
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

    #[cfg(unix)]
    #[test]
    fn owner_gate_accepts_self_and_rejects_a_foreign_socket() {
        use super::unix_impl::verify_socket_owner;
        use super::*;

        let path = Path::new("/tmp/vapor-abcd/vapord.sock");
        assert!(verify_socket_owner(path, 501, 501).is_ok());
        match verify_socket_owner(path, 502, 501) {
            Err(TransportError::ForeignSocket { owner_uid, .. }) => assert_eq!(owner_uid, 502),
            other => panic!("expected ForeignSocket, got {other:?}"),
        }
    }
}
