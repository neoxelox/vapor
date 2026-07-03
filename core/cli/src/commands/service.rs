//! `vapor service install|uninstall|start|stop|restart|status`.
//!
//! Drives `core/lifecycle::DaemonLifecycleManager` over the platform's
//! native `ServiceInstaller`. macOS today; Windows / Linux land in
//! Waves 12 / 13 respectively. Closes `cli.md` L2-1 … L2-4.
//!
//! The `restart` command is a stop-then-start sequence; both halves
//! tolerate the daemon already being in the target state.

use std::error::Error;
use std::fmt::{self, Display};
use std::path::PathBuf;
// `Arc` is only referenced by the macOS `build_native_macos` constructor and
// by the tests (which build `Arc<InMemory*>` fakes); it is unused on the
// non-macOS lib build, which `-D warnings` treats as an error.
#[cfg(any(target_os = "macos", test))]
use std::sync::Arc;
use std::time::Instant;

use vapor_lifecycle::{DaemonLifecycleError, DaemonLifecycleManager};
// The `AutoLaunchSettingStore` trait is needed by `build_native_macos` (macOS)
// and by the tests (its `read` method is called on the in-memory store); the
// concrete `JsonFileAutoLaunchSettingStore` is macOS-only.
#[cfg(any(target_os = "macos", test))]
use vapor_lifecycle::AutoLaunchSettingStore;
#[cfg(target_os = "macos")]
use vapor_lifecycle::JsonFileAutoLaunchSettingStore;
use vapor_platform::{ServiceInstallError, ServiceInstaller, ServiceStatus};
// The macOS installer type + descriptor are only used by `build_native_macos`.
#[cfg(target_os = "macos")]
use vapor_platform::{NativeServiceInstaller, ServiceDescriptor};
use vapor_shared::constants;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceCommand {
    Install,
    Uninstall,
    Start,
    Stop,
    Restart,
    Status,
}

#[derive(Debug)]
pub enum ServiceCommandError {
    /// The platform installer rejected the request (e.g. `launchctl`
    /// returned a non-zero exit code or the daemon binary was missing).
    Install(ServiceInstallError),
    /// Some other lifecycle layer (autolaunch settings persistence) failed.
    Lifecycle(DaemonLifecycleError),
    /// We couldn't locate the bundled daemon binary that the service
    /// definition needs to point at.
    DaemonBinaryMissing(PathBuf),
}

impl Display for ServiceCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Install(error) => write!(f, "service installer failed: {error}"),
            Self::Lifecycle(error) => write!(f, "lifecycle error: {error}"),
            Self::DaemonBinaryMissing(path) => {
                write!(f, "vapord binary not found (looked for {})", path.display())
            }
        }
    }
}

impl Error for ServiceCommandError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Install(error) => Some(error),
            Self::Lifecycle(error) => Some(error),
            Self::DaemonBinaryMissing(_) => None,
        }
    }
}

impl From<ServiceInstallError> for ServiceCommandError {
    fn from(error: ServiceInstallError) -> Self {
        Self::Install(error)
    }
}

impl From<DaemonLifecycleError> for ServiceCommandError {
    fn from(error: DaemonLifecycleError) -> Self {
        Self::Lifecycle(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceStatusReport {
    pub status: ServiceStatus,
    pub label: String,
}

/// Entry point for the binary's `clap` dispatch. The `manager`,
/// `installer`, and current-time hooks are split out so unit tests can
/// substitute fakes without touching real `launchctl`.
pub fn dispatch(
    command: ServiceCommand,
    manager: &DaemonLifecycleManager,
    installer: &dyn ServiceInstaller,
    now: Instant,
) -> Result<Option<ServiceStatusReport>, ServiceCommandError> {
    match command {
        ServiceCommand::Install => {
            manager.set_auto_launch_enabled(true, false, now)?;
            Ok(None)
        }
        ServiceCommand::Uninstall => {
            manager.set_auto_launch_enabled(false, true, now)?;
            Ok(None)
        }
        ServiceCommand::Start => {
            manager.start_daemon_if_allowed(now)?;
            Ok(None)
        }
        ServiceCommand::Stop => {
            manager.stop_daemon_for_termination()?;
            Ok(None)
        }
        ServiceCommand::Restart => {
            manager.stop_daemon_for_termination()?;
            manager.start_daemon_if_allowed(now)?;
            Ok(None)
        }
        ServiceCommand::Status => Ok(Some(ServiceStatusReport {
            status: installer.status()?,
            label: constants::service::DAEMON_LABEL.to_string(),
        })),
    }
}

/// Production-mode constructor for the macOS service surface. Looks
/// for the bundled daemon binary at `<cli-binary-parent>/vapord`,
/// falling back to `PATH` lookup. Returns the manager + installer pair
/// so the binary can call `dispatch` against them.
#[cfg(target_os = "macos")]
pub fn build_native_macos(
    config_path: PathBuf,
    daemon_binary: PathBuf,
) -> Result<(DaemonLifecycleManager, Arc<NativeServiceInstaller>), ServiceCommandError> {
    if !daemon_binary.is_file() {
        return Err(ServiceCommandError::DaemonBinaryMissing(daemon_binary));
    }
    let descriptor = ServiceDescriptor {
        label: constants::service::DAEMON_LABEL.to_string(),
        executable_path: daemon_binary,
        arguments: vec![],
        environment: vec![],
        stdout_path: None,
        stderr_path: None,
    };
    let installer = Arc::new(NativeServiceInstaller::for_current_user(descriptor)?);
    let settings: Arc<dyn AutoLaunchSettingStore> =
        Arc::new(JsonFileAutoLaunchSettingStore::new(config_path));
    let manager = DaemonLifecycleManager::new(installer.clone(), settings);
    Ok((manager, installer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vapor_lifecycle::InMemoryAutoLaunchSettingStore;
    use vapor_platform::{InMemoryServiceInstaller, ServiceDescriptor};

    fn fake_installer() -> Arc<InMemoryServiceInstaller> {
        Arc::new(InMemoryServiceInstaller::new(ServiceDescriptor {
            label: "sh.arn.vapor.test".to_string(),
            executable_path: PathBuf::from("/usr/bin/false"),
            arguments: vec![],
            environment: vec![],
            stdout_path: None,
            stderr_path: None,
        }))
    }

    #[test]
    fn install_sets_auto_launch_and_records_install_then_start() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::new());
        let manager = DaemonLifecycleManager::new(installer.clone(), settings.clone());

        dispatch(
            ServiceCommand::Install,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("install");

        assert_eq!(installer.operations(), vec!["install", "start"]);
        assert_eq!(settings.read().expect("read"), Some(true));
    }

    #[test]
    fn uninstall_disables_auto_launch_and_stops_daemon() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings.clone());

        dispatch(
            ServiceCommand::Uninstall,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("uninstall");

        assert_eq!(installer.operations(), vec!["uninstall", "stop"]);
        assert_eq!(settings.read().expect("read"), Some(false));
    }

    #[test]
    fn start_invokes_installer_start_when_no_crash_loop_pending() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);

        dispatch(
            ServiceCommand::Start,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("start");
        assert_eq!(installer.operations(), vec!["start"]);
    }

    #[test]
    fn stop_invokes_installer_stop() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);

        dispatch(
            ServiceCommand::Stop,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("stop");
        assert_eq!(installer.operations(), vec!["stop"]);
    }

    #[test]
    fn restart_stops_then_starts() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);

        dispatch(
            ServiceCommand::Restart,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("restart");
        assert_eq!(installer.operations(), vec!["stop", "start"]);
    }

    #[test]
    fn status_reports_current_installer_state() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);
        installer.set_status_for_testing(ServiceStatus::Running);

        let report = dispatch(
            ServiceCommand::Status,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("status")
        .expect("status report");
        assert_eq!(report.status, ServiceStatus::Running);
    }

    #[test]
    fn status_report_carries_the_service_label_not_the_log_file_name() {
        // Regression: status used to emit DAEMON_LOG_FILE_NAME
        // ("vapord.logs") in the label field, which made
        // `vapor service status` say `(label: vapord.logs)` — wrong.
        // The label is supposed to be the reverse-DNS service identifier.
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);

        let report = dispatch(
            ServiceCommand::Status,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("status")
        .expect("status report");

        assert_eq!(report.label, constants::service::DAEMON_LABEL);
        assert_ne!(report.label, constants::runtime::DAEMON_LOG_FILE_NAME);
    }
}
