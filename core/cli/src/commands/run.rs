//! `vapor run [--foreground]` — start the daemon in-process.
//!
//! Loads the durable state DB and the sync scope from the current
//! environment, then drives `DaemonRuntime::run_forever`. Exits non-
//! zero if another daemon is already attached to this `VAPOR_DIR`
//! (detected via the SQLite database's WAL lock).
//!
//! Closes `cli.md` L1-1.

use std::error::Error;
use std::fmt::{self, Display};

use vapor_daemon::{
    runtime::{DaemonRuntime, DaemonRuntimeError},
    state_db::{DurableStateDb, StateDbError},
    sync_directories,
};
use vapor_providers::default_provider;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RunOptions {
    /// The `--foreground` flag is currently a no-op marker — the daemon
    /// always runs in-process here. Reserved for the day we add a true
    /// `--background` mode that double-forks.
    pub foreground: bool,
}

#[derive(Debug)]
pub enum RunError {
    StateDb(StateDbError),
    Runtime(DaemonRuntimeError),
}

impl Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StateDb(error) => write!(f, "failed to open durable state DB: {error}"),
            Self::Runtime(error) => write!(f, "daemon runtime error: {error:?}"),
        }
    }
}

impl Error for RunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::StateDb(error) => Some(error),
            Self::Runtime(_) => None,
        }
    }
}

impl From<StateDbError> for RunError {
    fn from(error: StateDbError) -> Self {
        Self::StateDb(error)
    }
}

impl From<DaemonRuntimeError> for RunError {
    fn from(error: DaemonRuntimeError) -> Self {
        Self::Runtime(error)
    }
}

pub fn run(_options: RunOptions) -> Result<(), RunError> {
    let state_db = DurableStateDb::open_default()?;
    let sync_scope = sync_directories::resolve_from_process_environment();
    let mut runtime = DaemonRuntime::start(sync_scope, state_db, default_provider())?;
    runtime.run_forever()?;
    Ok(())
}
