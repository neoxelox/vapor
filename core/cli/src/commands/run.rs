//! `vapor run [--foreground]` — start the daemon in-process.
//!
//! Delegates to `vapor_daemon::bootstrap::run_daemon`, the same
//! composition the `vapord` binary uses (singleton lock → configuration
//! → durable state DB → runtime → IPC → tick loop), so the two entry
//! points cannot drift. Exits non-zero when another daemon already
//! holds this `VAPOR_DIR`'s lock.
//!
//! Closes `cli.md` L1-1.

use std::error::Error;
use std::fmt::{self, Display};

use vapor_daemon::bootstrap::{self, BootstrapError};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RunOptions {
    /// The `--foreground` flag is currently a no-op marker — the daemon
    /// always runs in-process here. Reserved for the day we add a true
    /// `--background` mode that double-forks.
    pub foreground: bool,
}

#[derive(Debug)]
pub enum RunError {
    Bootstrap(BootstrapError),
}

impl Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bootstrap(error) => write!(f, "{error}"),
        }
    }
}

impl Error for RunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Bootstrap(error) => Some(error),
        }
    }
}

pub fn run(_options: RunOptions) -> Result<(), RunError> {
    bootstrap::run_daemon().map_err(RunError::Bootstrap)
}
