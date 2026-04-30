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
    use std::os::unix::net::{UnixListener, UnixStream};

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

        pub fn into_inner(self) -> UnixListener {
            // SAFETY equivalent (no `unsafe` in this crate): we move
            // the listener out before the Drop runs by reassigning the
            // socket_path lifecycle into a no-op; the simplest path is
            // to clone the Listener via `UnixListener::try_clone()`.
            self.listener.try_clone().expect("clone listener")
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
        // launchctl + crash recovery may leave a stale socket file
        // around. Remove it before binding so the daemon can come up
        // again without a manual `rm`.
        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
        }
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(&socket_path)?;
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
