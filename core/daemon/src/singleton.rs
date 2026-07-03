//! Single-instance enforcement for the daemon.
//!
//! Exactly one daemon may serve a given `vapor_dir`: a second instance
//! would race the first on the durable queue and silently steal the IPC
//! socket (`bind_listener` removes a pre-existing socket file). The lock
//! is an OS advisory file lock (`flock` on Unix, `LockFileEx` on
//! Windows) on `<vapor_dir>/vapord.lock`, held for the lifetime of the
//! returned guard — it releases automatically when the process exits,
//! including on crash, so there is no stale-lock recovery to get wrong.

use std::error::Error;
use std::fmt::{self, Display};
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use vapor_shared::{constants, runtime_paths};

#[derive(Debug)]
pub enum SingletonLockError {
    /// Another live process holds the lock — a daemon is already
    /// serving this `vapor_dir`.
    AlreadyRunning(PathBuf),
    Io(io::Error),
}

impl Display for SingletonLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning(path) => write!(
                f,
                "another daemon already holds the lock at {}",
                path.display()
            ),
            Self::Io(error) => write!(f, "singleton lock I/O error: {error}"),
        }
    }
}

impl Error for SingletonLockError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for SingletonLockError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Holds the exclusive daemon lock while alive. Keep it in scope for the
/// whole daemon lifetime; dropping it releases the lock.
#[derive(Debug)]
pub struct SingletonLock {
    // Held purely for its OS-level lock; never read.
    _file: File,
    path: PathBuf,
}

impl SingletonLock {
    /// Acquires the canonical daemon lock for the current `vapor_dir`.
    pub fn acquire_for_current_vapor_dir() -> Result<Self, SingletonLockError> {
        let vapor_dir = runtime_paths::vapor_directory();
        runtime_paths::ensure_private_directory(&vapor_dir)?;
        Self::acquire(vapor_dir.join(constants::runtime::DAEMON_LOCK_FILE_NAME))
    }

    /// Acquires an exclusive lock on `path`, creating the file if needed.
    pub fn acquire(path: impl Into<PathBuf>) -> Result<Self, SingletonLockError> {
        let path = path.into();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(TryLockError::WouldBlock) => Err(SingletonLockError::AlreadyRunning(path)),
            Err(TryLockError::Error(error)) => Err(SingletonLockError::Io(error)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn second_acquire_on_same_path_is_rejected_while_first_is_held() {
        let temp = TempDir::new().expect("temp dir");
        let lock_path = temp.path().join("vapord.lock");

        let first = SingletonLock::acquire(&lock_path).expect("first lock");
        let second = SingletonLock::acquire(&lock_path)
            .expect_err("second lock must be rejected while first is held");
        assert!(matches!(second, SingletonLockError::AlreadyRunning(_)));

        drop(first);
        let reacquired = SingletonLock::acquire(&lock_path).expect("reacquire after release");
        assert_eq!(reacquired.path(), lock_path.as_path());
    }

    #[test]
    fn lock_file_is_created_when_missing() {
        let temp = TempDir::new().expect("temp dir");
        let lock_path = temp.path().join("nested-does-not-exist.lock");
        let lock = SingletonLock::acquire(&lock_path).expect("acquire creates the file");
        assert!(lock.path().exists());
    }
}
