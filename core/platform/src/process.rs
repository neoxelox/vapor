//! Process supervisor trait + per-OS native implementation.
//!
//! See `docs/architecture/platform-abstractions.md` §`ProcessSupervisor`.
//! On Unix the native impl uses `signal-hook` to translate `SIGTERM`
//! and `SIGINT` into the existing `SHUTDOWN_REQUESTED` flag-flip
//! pattern. The Windows impl is intentionally `unimplemented!()` until
//! Windows becomes a shipping surface.

use std::error::Error;
use std::fmt::{self, Display};

#[derive(Debug)]
pub enum ProcessSupervisorError {
    /// The handler installer call failed.
    Backend(Box<dyn Error + Send + Sync>),
    /// Operation is not supported on the current OS yet.
    Unsupported(&'static str),
}

impl Display for ProcessSupervisorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(error) => write!(f, "process supervisor backend failed: {error}"),
            Self::Unsupported(reason) => write!(f, "process supervisor not supported: {reason}"),
        }
    }
}

impl Error for ProcessSupervisorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Backend(error) => Some(&**error),
            _ => None,
        }
    }
}

/// Registers a graceful-shutdown handler.
///
/// The handler argument is invoked when the OS asks the daemon to stop:
/// `SIGTERM` / `SIGINT` on Unix, `SetConsoleCtrlHandler` events /
/// `SERVICE_STOP` / `WM_ENDSESSION` on Windows. Implementations must
/// keep the handler body minimal: typical use is "set the
/// `SHUTDOWN_REQUESTED` atomic and return". The tick loop observes the
/// flag at the next tick boundary and exits cleanly.
pub trait ProcessSupervisor: Send + Sync {
    fn register_shutdown_handler<F>(&self, handler: F) -> Result<(), ProcessSupervisorError>
    where
        F: Fn() + Send + Sync + 'static;
}

/// Test fake / pre-bridge default. Records the handler but never
/// invokes it.
#[derive(Debug, Default)]
pub struct NoopProcessSupervisor;

impl ProcessSupervisor for NoopProcessSupervisor {
    fn register_shutdown_handler<F>(&self, _handler: F) -> Result<(), ProcessSupervisorError>
    where
        F: Fn() + Send + Sync + 'static,
    {
        Ok(())
    }
}

#[cfg(unix)]
mod unix_impl {
    use super::{ProcessSupervisor, ProcessSupervisorError};
    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;
    use std::sync::Arc;

    /// Unix-native supervisor backed by `signal-hook`. Spawns a
    /// dedicated thread that waits on the signal iterator and invokes
    /// the registered handler on each `SIGTERM` / `SIGINT`.
    #[derive(Debug, Default)]
    pub struct NativeProcessSupervisor;

    impl NativeProcessSupervisor {
        pub fn new() -> Self {
            Self
        }
    }

    impl ProcessSupervisor for NativeProcessSupervisor {
        fn register_shutdown_handler<F>(&self, handler: F) -> Result<(), ProcessSupervisorError>
        where
            F: Fn() + Send + Sync + 'static,
        {
            let mut signals = Signals::new([SIGINT, SIGTERM])
                .map_err(|error| ProcessSupervisorError::Backend(Box::new(error)))?;
            let handler = Arc::new(handler);
            std::thread::Builder::new()
                .name("vapor-shutdown-signal".to_string())
                .spawn(move || {
                    for _signal in signals.forever() {
                        handler();
                    }
                })
                .map_err(|error| ProcessSupervisorError::Backend(Box::new(error)))?;
            Ok(())
        }
    }
}

#[cfg(unix)]
pub use unix_impl::NativeProcessSupervisor;

#[cfg(windows)]
mod windows_impl {
    use super::{ProcessSupervisor, ProcessSupervisorError};

    /// Windows-native supervisor stub. The real `SetConsoleCtrlHandler`,
    /// `SERVICE_STOP`, and `WM_ENDSESSION` integration lands when
    /// Windows becomes a shipping surface.
    #[derive(Debug, Default)]
    pub struct NativeProcessSupervisor;

    impl NativeProcessSupervisor {
        pub fn new() -> Self {
            Self
        }
    }

    impl ProcessSupervisor for NativeProcessSupervisor {
        fn register_shutdown_handler<F>(&self, _handler: F) -> Result<(), ProcessSupervisorError>
        where
            F: Fn() + Send + Sync + 'static,
        {
            Err(ProcessSupervisorError::Unsupported(
                "Windows ProcessSupervisor is not implemented yet",
            ))
        }
    }
}

#[cfg(windows)]
pub use windows_impl::NativeProcessSupervisor;

#[cfg(test)]
mod tests {
    use super::*;

    /// What every implementation holds to: a handler is taken, or the
    /// platform says that it has no way to call one yet. The contract
    /// raises no signal: that would reach every other test of the
    /// binary.
    fn exercise_contract(supervisor: &impl ProcessSupervisor) {
        match supervisor.register_shutdown_handler(|| {}) {
            Ok(()) | Err(ProcessSupervisorError::Unsupported(_)) => {}
            Err(error) => panic!("the handler was refused: {error}"),
        }
    }

    #[test]
    fn the_fake_supervisor_satisfies_the_contract() {
        exercise_contract(&NoopProcessSupervisor);
    }

    #[test]
    fn the_native_supervisor_satisfies_the_contract() {
        exercise_contract(&NativeProcessSupervisor::new());
    }
}
