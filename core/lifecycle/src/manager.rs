//! `DaemonLifecycleManager` — orchestrates install / enable / start /
//! stop and crash-loop bookkeeping over the platform's
//! [`vapor_platform::ServiceInstaller`].
//!
//! Verbatim port of the Swift `DaemonLifecycleManager` from
//! `apps/macos/Sources/VaporCore/DaemonLifecycle.swift`. Same public
//! surface (`bootstrap_if_needed`, `set_auto_launch_enabled`,
//! `register_unexpected_daemon_exit`, `start_daemon_if_allowed`,
//! `stop_daemon_for_termination`, `acknowledge_crash_loop_pause`).
//!
//! Closes `core.md` C4-3.

use std::error::Error;
use std::fmt::{self, Display};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vapor_platform::{ServiceInstallError, ServiceInstaller};

use crate::auto_launch::{AutoLaunchSettingStore, JsonFileError};
use crate::crash_loop::{CrashLoopDecision, CrashLoopGuard, CrashLoopPolicy};

#[derive(Debug)]
pub enum DaemonLifecycleError {
    /// The platform service installer rejected an operation.
    ServiceInstall(ServiceInstallError),
    /// The auto-launch setting store rejected a read or write.
    Settings(JsonFileError),
}

impl Display for DaemonLifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServiceInstall(error) => write!(f, "service installer failed: {error}"),
            Self::Settings(error) => write!(f, "auto-launch settings failed: {error}"),
        }
    }
}

impl Error for DaemonLifecycleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ServiceInstall(error) => Some(error),
            Self::Settings(error) => Some(error),
        }
    }
}

impl From<ServiceInstallError> for DaemonLifecycleError {
    fn from(error: ServiceInstallError) -> Self {
        Self::ServiceInstall(error)
    }
}

impl From<JsonFileError> for DaemonLifecycleError {
    fn from(error: JsonFileError) -> Self {
        Self::Settings(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DaemonLifecycleActionResult {
    /// The action was a no-op given current state (e.g. autolaunch
    /// already in the requested state).
    Unchanged,
    /// The daemon transitioned from stopped to running.
    Started,
    /// The daemon transitioned from running to stopped.
    Stopped,
    /// Crash-loop guard wants the next start delayed by this much
    /// (`Duration::MAX` means paused indefinitely awaiting user
    /// acknowledgement).
    RelaunchDeferred(Duration),
}

/// Lifecycle orchestrator. Owns the service installer + autolaunch
/// store + crash-loop guard. Mirrors the Swift class one-to-one;
/// callers (the macOS Swift app via the `vapor` CLI subprocess; the
/// CLI itself; the Windows / Linux apps eventually) see identical
/// behavior.
pub struct DaemonLifecycleManager {
    installer: Arc<dyn ServiceInstaller>,
    settings: Arc<dyn AutoLaunchSettingStore>,
    inner: Mutex<Inner>,
}

struct Inner {
    crash_loop_guard: CrashLoopGuard,
}

impl DaemonLifecycleManager {
    pub fn new(
        installer: Arc<dyn ServiceInstaller>,
        settings: Arc<dyn AutoLaunchSettingStore>,
    ) -> Self {
        Self::with_crash_loop_policy(installer, settings, CrashLoopPolicy::default())
    }

    pub fn with_crash_loop_policy(
        installer: Arc<dyn ServiceInstaller>,
        settings: Arc<dyn AutoLaunchSettingStore>,
        crash_loop_policy: CrashLoopPolicy,
    ) -> Self {
        Self {
            installer,
            settings,
            inner: Mutex::new(Inner {
                crash_loop_guard: CrashLoopGuard::new(crash_loop_policy),
            }),
        }
    }

    /// Reads the persisted autolaunch preference, defaulting to `true`
    /// the first time it is observed (matching Swift's behavior of
    /// writing the default back to disk on first observation).
    pub fn auto_launch_enabled(&self) -> Result<bool, DaemonLifecycleError> {
        if let Some(persisted) = self.settings.read()? {
            return Ok(persisted);
        }
        // Persist the default so subsequent reads are deterministic.
        self.settings.write(true)?;
        Ok(true)
    }

    /// Mirrors `bootstrapIfNeeded(now:)`.
    pub fn bootstrap_if_needed(
        &self,
        now: Instant,
    ) -> Result<DaemonLifecycleActionResult, DaemonLifecycleError> {
        if !self.auto_launch_enabled()? {
            return Ok(DaemonLifecycleActionResult::Unchanged);
        }
        self.installer.install_and_enable()?;
        self.start_daemon_if_allowed_inner(now)
    }

    /// Mirrors `setAutoLaunchEnabled(_:stopDaemonNow:now:)`.
    pub fn set_auto_launch_enabled(
        &self,
        enabled: bool,
        stop_daemon_now: bool,
        now: Instant,
    ) -> Result<DaemonLifecycleActionResult, DaemonLifecycleError> {
        self.settings.write(enabled)?;
        if enabled {
            self.installer.install_and_enable()?;
            return self.start_daemon_if_allowed_inner(now);
        }

        self.installer.disable_and_uninstall()?;
        self.with_inner(|inner| inner.crash_loop_guard.reset());
        if stop_daemon_now {
            self.installer.stop_daemon()?;
            return Ok(DaemonLifecycleActionResult::Stopped);
        }
        Ok(DaemonLifecycleActionResult::Unchanged)
    }

    /// Mirrors `registerUnexpectedDaemonExit(now:)`.
    pub fn register_unexpected_daemon_exit(&self, now: Instant) -> CrashLoopDecision {
        self.with_inner(|inner| inner.crash_loop_guard.register_crash(now))
    }

    pub fn is_in_crash_loop_pause(&self) -> bool {
        self.with_inner(|inner| inner.crash_loop_guard.is_paused_indefinitely())
    }

    /// Mirrors `acknowledgeCrashLoopPause()`.
    pub fn acknowledge_crash_loop_pause(&self) {
        self.with_inner(|inner| inner.crash_loop_guard.acknowledge_and_resume());
    }

    /// Mirrors `startDaemonIfAllowed(now:)`.
    pub fn start_daemon_if_allowed(
        &self,
        now: Instant,
    ) -> Result<DaemonLifecycleActionResult, DaemonLifecycleError> {
        self.start_daemon_if_allowed_inner(now)
    }

    /// Mirrors `stopDaemonForTermination()`.
    pub fn stop_daemon_for_termination(&self) -> Result<(), DaemonLifecycleError> {
        self.installer.stop_daemon()?;
        Ok(())
    }

    fn start_daemon_if_allowed_inner(
        &self,
        now: Instant,
    ) -> Result<DaemonLifecycleActionResult, DaemonLifecycleError> {
        let remaining = self.with_inner(|inner| {
            if inner.crash_loop_guard.is_paused_indefinitely() {
                Duration::MAX
            } else {
                inner.crash_loop_guard.remaining_delay(now)
            }
        });

        if remaining > Duration::ZERO {
            return Ok(DaemonLifecycleActionResult::RelaunchDeferred(remaining));
        }

        self.installer.start_daemon()?;
        Ok(DaemonLifecycleActionResult::Started)
    }

    fn with_inner<R>(&self, body: impl FnOnce(&mut Inner) -> R) -> R {
        let mut guard = self
            .inner
            .lock()
            .expect("DaemonLifecycleManager mutex poisoned");
        body(&mut guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_launch::InMemoryAutoLaunchSettingStore;
    use std::path::PathBuf;
    use vapor_platform::{InMemoryServiceInstaller, ServiceDescriptor, ServiceStatus};

    fn descriptor() -> ServiceDescriptor {
        ServiceDescriptor {
            label: "sh.arn.vapor.test".to_string(),
            executable_path: PathBuf::from("/usr/bin/false"),
            arguments: vec![],
            environment: vec![],
            stdout_path: None,
            stderr_path: None,
        }
    }

    fn fixed_policy() -> CrashLoopPolicy {
        // Mirrors the policy used in the Swift parity tests.
        CrashLoopPolicy::new(
            Duration::from_secs(60),
            Duration::from_secs(2),
            Duration::from_secs(32),
            2,
            10,
        )
    }

    fn manager_with(
        installer: Arc<InMemoryServiceInstaller>,
        settings: Arc<InMemoryAutoLaunchSettingStore>,
    ) -> DaemonLifecycleManager {
        DaemonLifecycleManager::with_crash_loop_policy(installer, settings, fixed_policy())
    }

    #[test]
    fn bootstrap_defaults_to_auto_launch_enabled_and_starts_daemon() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::new());
        let manager = manager_with(installer.clone(), settings.clone());

        assert!(manager.auto_launch_enabled().expect("auto launch read"));

        let result = manager
            .bootstrap_if_needed(Instant::now())
            .expect("bootstrap");
        assert_eq!(result, DaemonLifecycleActionResult::Started);
        assert_eq!(settings.read().expect("read"), Some(true));
        assert_eq!(installer.operations(), vec!["install", "start"]);
    }

    #[test]
    fn disabling_auto_launch_without_stop_keeps_daemon_running() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = manager_with(installer.clone(), settings.clone());

        let result = manager
            .set_auto_launch_enabled(false, false, Instant::now())
            .expect("set");

        assert_eq!(result, DaemonLifecycleActionResult::Unchanged);
        assert_eq!(installer.operations(), vec!["uninstall"]);
        assert_eq!(settings.read().expect("read"), Some(false));
    }

    #[test]
    fn disabling_auto_launch_with_stop_also_stops_daemon() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = manager_with(installer.clone(), settings);

        let result = manager
            .set_auto_launch_enabled(false, true, Instant::now())
            .expect("set");

        assert_eq!(result, DaemonLifecycleActionResult::Stopped);
        assert_eq!(installer.operations(), vec!["uninstall", "stop"]);
    }

    #[test]
    fn crash_loop_defers_relaunch_with_exponential_backoff() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = manager_with(installer.clone(), settings);

        let t0 = Instant::now();
        // First two crashes are within `delay_starts_after_failures = 2`
        // so they are NoDelay; the third crash triggers backoff.
        assert_eq!(
            manager.register_unexpected_daemon_exit(t0),
            CrashLoopDecision::NoDelay
        );
        assert_eq!(
            manager.register_unexpected_daemon_exit(t0 + Duration::from_secs(1)),
            CrashLoopDecision::NoDelay
        );
        assert_eq!(
            manager.register_unexpected_daemon_exit(t0 + Duration::from_secs(2)),
            CrashLoopDecision::Backoff(Duration::from_secs(2))
        );

        // Try to start while still inside the backoff window — must
        // defer.
        let deferred = manager
            .start_daemon_if_allowed(t0 + Duration::from_secs(3))
            .expect("start");
        assert!(matches!(
            deferred,
            DaemonLifecycleActionResult::RelaunchDeferred(remaining)
                if remaining <= Duration::from_secs(2)
        ));
        assert!(installer.operations().is_empty());

        // After the window elapses, the start succeeds.
        let started = manager
            .start_daemon_if_allowed(t0 + Duration::from_secs(10))
            .expect("start");
        assert_eq!(started, DaemonLifecycleActionResult::Started);
        assert_eq!(installer.operations(), vec!["start"]);
    }

    #[test]
    fn crash_loop_pauses_after_max_consecutive_failures_and_refuses_auto_restart() {
        let policy = CrashLoopPolicy::new(
            Duration::from_secs(600),
            Duration::from_secs(2),
            Duration::from_secs(120),
            1,
            3,
        );
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager =
            DaemonLifecycleManager::with_crash_loop_policy(installer.clone(), settings, policy);

        let t0 = Instant::now();
        let _ = manager.register_unexpected_daemon_exit(t0);
        let _ = manager.register_unexpected_daemon_exit(t0 + Duration::from_secs(1));
        let decision = manager.register_unexpected_daemon_exit(t0 + Duration::from_secs(2));
        assert_eq!(decision, CrashLoopDecision::Paused);
        assert!(manager.is_in_crash_loop_pause());

        let result = manager
            .start_daemon_if_allowed(t0 + Duration::from_secs(3_600))
            .expect("start");
        assert!(matches!(
            result,
            DaemonLifecycleActionResult::RelaunchDeferred(remaining)
                if remaining == Duration::MAX
        ));
        assert!(installer.operations().is_empty());

        manager.acknowledge_crash_loop_pause();
        assert!(!manager.is_in_crash_loop_pause());

        let resumed = manager
            .start_daemon_if_allowed(t0 + Duration::from_secs(3_700))
            .expect("start");
        assert_eq!(resumed, DaemonLifecycleActionResult::Started);
        assert_eq!(installer.operations(), vec!["start"]);
    }

    #[test]
    fn stop_daemon_for_termination_calls_installer_stop() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = manager_with(installer.clone(), settings);

        manager.stop_daemon_for_termination().expect("stop");
        assert_eq!(installer.operations(), vec!["stop"]);
    }

    #[test]
    fn auto_launch_enabled_persists_default_true_on_first_read() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::new());
        let manager = manager_with(installer, settings.clone());

        assert!(manager.auto_launch_enabled().expect("read"));
        assert_eq!(settings.read().expect("read"), Some(true));
    }

    #[test]
    fn enabling_auto_launch_after_disable_re_installs_and_starts() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(false)));
        let manager = manager_with(installer.clone(), settings);

        let result = manager
            .set_auto_launch_enabled(true, false, Instant::now())
            .expect("set");
        assert_eq!(result, DaemonLifecycleActionResult::Started);
        assert_eq!(installer.operations(), vec!["install", "start"]);
        assert_eq!(installer.status().expect("status"), ServiceStatus::Running);
    }
}
