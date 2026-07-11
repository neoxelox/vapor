//! Per-OS service installer trait + macOS native impl.
//!
//! See `docs/architecture/platform-abstractions.md` §`ServiceInstaller`.

use std::error::Error;
use std::fmt::{self, Display};
use std::path::PathBuf;

mod fake;
pub use fake::InMemoryServiceInstaller;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::NativeServiceInstaller;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::NativeServiceInstaller;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::NativeServiceInstaller;

/// Per-OS daemon descriptor. Captures the bits every installer needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceDescriptor {
    /// Reverse-DNS service identifier, e.g. `sh.arn.vapor.daemon`.
    pub label: String,
    /// Absolute path to the daemon binary. Per AGENTS.md §7.2 this is
    /// always the bundled sibling `Contents/MacOS/vapord` on macOS.
    pub executable_path: PathBuf,
    /// Arguments passed to the daemon at start.
    pub arguments: Vec<String>,
    /// Environment variables to forward to the daemon process.
    pub environment: Vec<(String, String)>,
    /// Where the daemon's stdout should land (per the launchagent policy).
    pub stdout_path: Option<PathBuf>,
    /// Where the daemon's stderr should land.
    pub stderr_path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceStatus {
    /// Service definition is not registered with the OS.
    NotInstalled,
    /// Service is registered but not currently running.
    Stopped,
    /// Service is registered and running.
    Running,
    /// Service is registered, stopped, and `core/lifecycle::CrashLoopGuard`
    /// has paused auto-restart pending user acknowledgement.
    CrashLoopPaused,
}

#[derive(Debug)]
pub enum ServiceInstallError {
    /// The daemon binary referenced by [`ServiceDescriptor::executable_path`]
    /// is missing or not executable.
    InvalidExecutable { path: PathBuf, reason: String },
    /// The OS-native installer call returned a non-zero exit code or
    /// otherwise failed.
    Backend(Box<dyn Error + Send + Sync>),
    /// Operation is not supported on the current OS yet (the native
    /// implementation has not landed).
    Unsupported(&'static str),
}

impl Display for ServiceInstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidExecutable { path, reason } => {
                write!(f, "invalid daemon executable {}: {reason}", path.display())
            }
            Self::Backend(error) => write!(f, "service installer backend failed: {error}"),
            Self::Unsupported(reason) => write!(f, "service installer not supported: {reason}"),
        }
    }
}

impl Error for ServiceInstallError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Backend(error) => Some(&**error),
            _ => None,
        }
    }
}

pub trait ServiceInstaller: Send + Sync {
    fn install_and_enable(&self) -> Result<(), ServiceInstallError>;
    fn disable_and_uninstall(&self) -> Result<(), ServiceInstallError>;
    fn start_daemon(&self) -> Result<(), ServiceInstallError>;
    fn stop_daemon(&self) -> Result<(), ServiceInstallError>;
    fn status(&self) -> Result<ServiceStatus, ServiceInstallError>;
}
