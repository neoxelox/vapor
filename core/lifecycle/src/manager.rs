//! `DaemonLifecycleManager` — orchestrates install / enable / start /
//! stop and crash-loop bookkeeping over the platform's
//! [`vapor_platform::ServiceInstaller`].
//!
//! Port of the Swift `DaemonLifecycleManager` that previously lived in
//! `apps/macos/Sources/VaporCore/DaemonLifecycle.swift`. Same public
//! surface (`bootstrap_if_needed`, `set_auto_launch_enabled`,
//! `register_unexpected_daemon_exit`, `start_daemon_if_allowed`,
//! `stop_daemon_for_termination`, `acknowledge_crash_loop_pause`), plus
//! the durable-state + health-check layer the Swift copy never had:
//!
//! - With a [`LifecycleStateStore`] attached, crash-loop bookkeeping is
//!   hydrated from and persisted to `<vapor_dir>/state/lifecycle.json`,
//!   so backoff and pause survive process restarts and are shared by
//!   every surface.
//! - [`check_daemon_health`](DaemonLifecycleManager::check_daemon_health)
//!   is the supervision tick behind `vapor service check`: it detects an
//!   unexpected daemon exit, routes it through the crash-loop guard, and
//!   restarts when policy allows.

use std::error::Error;
use std::fmt::{self, Display};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vapor_platform::{ServiceInstallError, ServiceInstaller, ServiceStatus};

use crate::auto_launch::{AutoLaunchSettingStore, JsonFileError};
use crate::crash_loop::{CrashLoopDecision, CrashLoopGuard, CrashLoopPolicy};
use crate::durable::{
    LIFECYCLE_STATE_SCHEMA_VERSION, LifecycleStateStore, PersistedLifecycleState, SystemWallClock,
    WallClock,
};

#[derive(Debug)]
pub enum DaemonLifecycleError {
    /// The platform service installer rejected an operation.
    ServiceInstall(ServiceInstallError),
    /// The auto-launch setting store rejected a read or write.
    Settings(JsonFileError),
    /// The durable lifecycle state store rejected a read or write.
    LifecycleState(JsonFileError),
}

impl Display for DaemonLifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServiceInstall(error) => write!(f, "service installer failed: {error}"),
            Self::Settings(error) => write!(f, "auto-launch settings failed: {error}"),
            Self::LifecycleState(error) => write!(f, "lifecycle state store failed: {error}"),
        }
    }
}

impl Error for DaemonLifecycleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ServiceInstall(error) => Some(error),
            Self::Settings(error) => Some(error),
            Self::LifecycleState(error) => Some(error),
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
    /// No service definition is registered with the OS, so there is
    /// nothing to start or restart. Auto-launch is what installs one;
    /// an explicit start against a bare host reports this instead of
    /// failing inside the service manager.
    NotInstalled,
}

/// Outcome of one supervision tick ([`DaemonLifecycleManager::check_daemon_health`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DaemonHealthCheckOutcome {
    /// Service reports running — nothing to do.
    Running,
    /// No service definition is installed; nothing to supervise.
    NotInstalled,
    /// Not running, and the last lifecycle action expected it stopped.
    StoppedExpected,
    /// An unexpected exit was detected (or a pending backoff elapsed)
    /// and the daemon was restarted.
    RestartedAfterCrash,
    /// An unexpected exit is registered; restart deferred by backoff.
    RestartDeferred(Duration),
    /// Crash-loop pause engaged; awaiting user acknowledgement.
    CrashLoopPaused,
}

/// Read-only view of the crash-loop bookkeeping for status surfaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrashLoopStateSnapshot {
    pub paused: bool,
    pub consecutive_crashes: u32,
    pub last_crash_at_ms: Option<u64>,
    pub awaiting_restart: bool,
}

/// Lifecycle orchestrator. Owns the service installer + autolaunch
/// store + crash-loop guard (+ optional durable state). Every surface
/// (the macOS Swift app via the `vapor` CLI subprocess; the CLI itself;
/// the Windows / Linux apps eventually) sees identical behavior.
pub struct DaemonLifecycleManager {
    installer: Arc<dyn ServiceInstaller>,
    settings: Arc<dyn AutoLaunchSettingStore>,
    state_store: Option<Arc<dyn LifecycleStateStore>>,
    wall_clock: Arc<dyn WallClock>,
    inner: Mutex<Inner>,
}

struct Inner {
    crash_loop_guard: CrashLoopGuard,
    /// Wall-clock mirror of the guard's most recent crash, kept for
    /// persistence (the guard itself is `Instant`-based).
    last_crash_at_ms: Option<u64>,
    /// True after any successful start; false after an expected stop /
    /// uninstall. `check_daemon_health` only treats an absent daemon as
    /// a crash while this is set.
    daemon_should_be_running: bool,
    /// A registered unexpected exit whose restart hasn't happened yet.
    awaiting_restart: bool,
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
            state_store: None,
            wall_clock: Arc::new(SystemWallClock),
            inner: Mutex::new(Inner {
                crash_loop_guard: CrashLoopGuard::new(crash_loop_policy),
                last_crash_at_ms: None,
                daemon_should_be_running: false,
                awaiting_restart: false,
            }),
        }
    }

    /// Production constructor: hydrates crash-loop bookkeeping from the
    /// durable store and persists every subsequent transition back to
    /// it, so lifecycle state survives process restarts and is shared
    /// across surfaces.
    pub fn with_durable_state(
        installer: Arc<dyn ServiceInstaller>,
        settings: Arc<dyn AutoLaunchSettingStore>,
        crash_loop_policy: CrashLoopPolicy,
        state_store: Arc<dyn LifecycleStateStore>,
        wall_clock: Arc<dyn WallClock>,
    ) -> Result<Self, DaemonLifecycleError> {
        let persisted = state_store
            .load()
            .map_err(DaemonLifecycleError::LifecycleState)?
            .unwrap_or_default();

        let now = Instant::now();
        let last_crash_elapsed = persisted
            .last_crash_at_ms
            .map(|at_ms| Duration::from_millis(wall_clock.now_ms().saturating_sub(at_ms)))
            .unwrap_or(Duration::ZERO);
        let crash_loop_guard = CrashLoopGuard::restore(
            crash_loop_policy,
            persisted.consecutive_crashes,
            last_crash_elapsed,
            persisted.paused_indefinitely,
            now,
        );

        Ok(Self {
            installer,
            settings,
            state_store: Some(state_store),
            wall_clock,
            inner: Mutex::new(Inner {
                crash_loop_guard,
                last_crash_at_ms: persisted.last_crash_at_ms,
                daemon_should_be_running: persisted.daemon_should_be_running,
                awaiting_restart: persisted.awaiting_restart,
            }),
        })
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
        // Consult the crash-loop guard BEFORE install_and_enable: on macOS
        // install writes a RunAtLoad=true LaunchAgent and launchd starts
        // the daemon immediately, which would bypass a durable crash-loop
        // pause/backoff on every app launch (the UI shows "paused" while
        // the daemon is actually relaunched).
        let relaunch_delay = self.current_relaunch_delay(now);
        if relaunch_delay > Duration::ZERO {
            return Ok(DaemonLifecycleActionResult::RelaunchDeferred(
                relaunch_delay,
            ));
        }
        self.installer.install_and_enable()?;
        self.start_daemon_if_allowed_inner(now)
    }

    /// The relaunch delay the crash-loop guard currently imposes
    /// (`Duration::MAX` when paused indefinitely, zero when a relaunch is
    /// allowed). Consulted before any install path that would auto-start.
    fn current_relaunch_delay(&self, now: Instant) -> Duration {
        self.with_inner(|inner| {
            if inner.crash_loop_guard.is_paused_indefinitely() {
                Duration::MAX
            } else {
                inner.crash_loop_guard.remaining_delay(now)
            }
        })
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
            // Same guard-before-install ordering as bootstrap_if_needed:
            // enabling autolaunch must not relaunch a crash-loop-paused
            // daemon via RunAtLoad.
            let relaunch_delay = self.current_relaunch_delay(now);
            if relaunch_delay > Duration::ZERO {
                return Ok(DaemonLifecycleActionResult::RelaunchDeferred(
                    relaunch_delay,
                ));
            }
            self.installer.install_and_enable()?;
            return self.start_daemon_if_allowed_inner(now);
        }

        self.installer.disable_and_uninstall()?;
        self.with_inner(|inner| {
            inner.crash_loop_guard.reset();
            inner.last_crash_at_ms = None;
            // The service definition is gone; nothing supervises an
            // unregistered daemon, whether or not it keeps running.
            inner.daemon_should_be_running = false;
            inner.awaiting_restart = false;
            self.persist_locked(inner, now)
        })?;
        if stop_daemon_now {
            self.installer.stop_daemon()?;
            return Ok(DaemonLifecycleActionResult::Stopped);
        }
        Ok(DaemonLifecycleActionResult::Unchanged)
    }

    /// Mirrors `registerUnexpectedDaemonExit(now:)`. Records the crash
    /// durably (when a state store is attached) so backoff survives
    /// restarts.
    pub fn register_unexpected_daemon_exit(
        &self,
        now: Instant,
    ) -> Result<CrashLoopDecision, DaemonLifecycleError> {
        self.with_inner(|inner| {
            let decision = inner.crash_loop_guard.register_crash(now);
            inner.last_crash_at_ms = Some(self.wall_clock.now_ms());
            self.persist_locked(inner, now)?;
            Ok(decision)
        })
    }

    pub fn is_in_crash_loop_pause(&self) -> bool {
        self.with_inner(|inner| inner.crash_loop_guard.is_paused_indefinitely())
    }

    /// Read-only crash-loop bookkeeping for `vapor service status`.
    pub fn crash_loop_state(&self, now: Instant) -> CrashLoopStateSnapshot {
        self.with_inner(|inner| CrashLoopStateSnapshot {
            paused: inner.crash_loop_guard.is_paused_indefinitely(),
            consecutive_crashes: inner.crash_loop_guard.consecutive_crashes(now),
            last_crash_at_ms: inner.last_crash_at_ms,
            awaiting_restart: inner.awaiting_restart,
        })
    }

    /// Mirrors `acknowledgeCrashLoopPause()`. Clears the pause and the
    /// crash history; the next `check` / `start` may launch the daemon
    /// again.
    pub fn acknowledge_crash_loop_pause(&self, now: Instant) -> Result<(), DaemonLifecycleError> {
        self.with_inner(|inner| {
            inner.crash_loop_guard.acknowledge_and_resume();
            inner.last_crash_at_ms = None;
            self.persist_locked(inner, now)
        })
    }

    /// Mirrors `startDaemonIfAllowed(now:)`. An explicit start needs a
    /// registered service definition; without one the answer is
    /// [`DaemonLifecycleActionResult::NotInstalled`], and the guard's
    /// bookkeeping stays untouched.
    pub fn start_daemon_if_allowed(
        &self,
        now: Instant,
    ) -> Result<DaemonLifecycleActionResult, DaemonLifecycleError> {
        if self.installer.status()? == ServiceStatus::NotInstalled {
            return Ok(DaemonLifecycleActionResult::NotInstalled);
        }
        self.start_daemon_if_allowed_inner(now)
    }

    /// Mirrors `stopDaemonForTermination()`.
    pub fn stop_daemon_for_termination(&self, now: Instant) -> Result<(), DaemonLifecycleError> {
        self.installer.stop_daemon()?;
        self.with_inner(|inner| {
            inner.daemon_should_be_running = false;
            inner.awaiting_restart = false;
            self.persist_locked(inner, now)
        })
    }

    /// One supervision tick (the engine behind `vapor service check`).
    ///
    /// Probes the platform service manager and reconciles observed
    /// reality with expectations:
    ///
    /// - running → healthy (clears any pending-restart bookkeeping);
    /// - not installed → nothing to supervise;
    /// - stopped while expected stopped → fine;
    /// - stopped while expected running → an unexpected exit. The first
    ///   observation registers the crash with the guard; subsequent
    ///   observations of the same exit re-check the backoff instead of
    ///   double-counting. The daemon restarts as soon as policy allows;
    ///   `CrashLoopPaused` restarts only after user acknowledgement.
    pub fn check_daemon_health(
        &self,
        now: Instant,
    ) -> Result<DaemonHealthCheckOutcome, DaemonLifecycleError> {
        let status = self.installer.status()?;
        self.with_inner(|inner| match status {
            ServiceStatus::Running => {
                if inner.awaiting_restart || !inner.daemon_should_be_running {
                    // Observed reality wins: someone (launchd RunAtLoad,
                    // a manual `vapor service start`) brought it up.
                    inner.awaiting_restart = false;
                    inner.daemon_should_be_running = true;
                    self.persist_locked(inner, now)?;
                }
                Ok(DaemonHealthCheckOutcome::Running)
            }
            ServiceStatus::NotInstalled => Ok(DaemonHealthCheckOutcome::NotInstalled),
            ServiceStatus::Stopped | ServiceStatus::CrashLoopPaused => {
                if !inner.daemon_should_be_running {
                    return Ok(DaemonHealthCheckOutcome::StoppedExpected);
                }

                if !inner.awaiting_restart {
                    // First observation of this exit — count it.
                    let decision = inner.crash_loop_guard.register_crash(now);
                    inner.last_crash_at_ms = Some(self.wall_clock.now_ms());
                    inner.awaiting_restart = true;
                    return match decision {
                        CrashLoopDecision::NoDelay => {
                            self.installer.start_daemon()?;
                            inner.awaiting_restart = false;
                            self.persist_locked(inner, now)?;
                            Ok(DaemonHealthCheckOutcome::RestartedAfterCrash)
                        }
                        CrashLoopDecision::Backoff(delay) => {
                            self.persist_locked(inner, now)?;
                            Ok(DaemonHealthCheckOutcome::RestartDeferred(delay))
                        }
                        CrashLoopDecision::Paused => {
                            self.persist_locked(inner, now)?;
                            Ok(DaemonHealthCheckOutcome::CrashLoopPaused)
                        }
                    };
                }

                // Same exit as a prior tick — restart once the backoff
                // has elapsed.
                if inner.crash_loop_guard.is_paused_indefinitely() {
                    return Ok(DaemonHealthCheckOutcome::CrashLoopPaused);
                }
                let remaining = inner.crash_loop_guard.remaining_delay(now);
                if remaining > Duration::ZERO {
                    return Ok(DaemonHealthCheckOutcome::RestartDeferred(remaining));
                }
                self.installer.start_daemon()?;
                inner.awaiting_restart = false;
                self.persist_locked(inner, now)?;
                Ok(DaemonHealthCheckOutcome::RestartedAfterCrash)
            }
        })
    }

    fn start_daemon_if_allowed_inner(
        &self,
        now: Instant,
    ) -> Result<DaemonLifecycleActionResult, DaemonLifecycleError> {
        self.with_inner(|inner| {
            let remaining = if inner.crash_loop_guard.is_paused_indefinitely() {
                Duration::MAX
            } else {
                inner.crash_loop_guard.remaining_delay(now)
            };

            if remaining > Duration::ZERO {
                return Ok(DaemonLifecycleActionResult::RelaunchDeferred(remaining));
            }

            self.installer.start_daemon()?;
            inner.daemon_should_be_running = true;
            inner.awaiting_restart = false;
            self.persist_locked(inner, now)?;
            Ok(DaemonLifecycleActionResult::Started)
        })
    }

    /// Writes the current bookkeeping to the durable store, when one is
    /// attached. Called with the `inner` lock held.
    fn persist_locked(&self, inner: &mut Inner, now: Instant) -> Result<(), DaemonLifecycleError> {
        let Some(store) = &self.state_store else {
            return Ok(());
        };
        let consecutive_crashes = inner.crash_loop_guard.consecutive_crashes(now);
        store
            .save(&PersistedLifecycleState {
                schema_version: LIFECYCLE_STATE_SCHEMA_VERSION,
                daemon_should_be_running: inner.daemon_should_be_running,
                consecutive_crashes,
                last_crash_at_ms: if consecutive_crashes == 0 {
                    None
                } else {
                    inner.last_crash_at_ms
                },
                paused_indefinitely: inner.crash_loop_guard.is_paused_indefinitely(),
                awaiting_restart: inner.awaiting_restart,
            })
            .map_err(DaemonLifecycleError::LifecycleState)
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
    use crate::durable::{FixedWallClock, InMemoryLifecycleStateStore};
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
            keep_alive: false,
        }
    }

    /// A fake whose service definition is already registered, for the
    /// tests that model a supervised daemon rather than a bare host.
    fn installed_installer() -> Arc<InMemoryServiceInstaller> {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        installer.set_status_for_testing(ServiceStatus::Stopped);
        installer
    }

    fn fixed_policy() -> CrashLoopPolicy {
        // Two free crashes, then 2s-base backoff capped at 32s.
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

    fn durable_manager_with(
        installer: Arc<InMemoryServiceInstaller>,
        store: Arc<InMemoryLifecycleStateStore>,
        clock: Arc<FixedWallClock>,
        policy: CrashLoopPolicy,
    ) -> DaemonLifecycleManager {
        DaemonLifecycleManager::with_durable_state(
            installer,
            Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true))),
            policy,
            store,
            clock,
        )
        .expect("durable manager")
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
    fn bootstrap_defers_without_installing_while_a_crash_loop_backoff_is_pending() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = manager_with(installer.clone(), settings);
        let t0 = Instant::now();

        // Two free crashes, then one more → a backoff is pending.
        manager.register_unexpected_daemon_exit(t0).expect("c1");
        manager.register_unexpected_daemon_exit(t0).expect("c2");
        manager.register_unexpected_daemon_exit(t0).expect("c3");

        let result = manager.bootstrap_if_needed(t0).expect("bootstrap");
        assert!(matches!(
            result,
            DaemonLifecycleActionResult::RelaunchDeferred(_)
        ));
        // Crucially, no install/start ran: a RunAtLoad install would have
        // relaunched the crash-looping daemon behind the deferral.
        assert!(
            installer.operations().is_empty(),
            "ops should be empty, got {:?}",
            installer.operations()
        );
    }

    #[test]
    fn disabling_auto_launch_without_stop_skips_the_explicit_stop_signal() {
        // "Without stop" means no explicit stop is sent through the
        // installer. Whether the daemon survives is up to the platform
        // service manager — launchd tears the job down on bootout.
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
        let installer = installed_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = manager_with(installer.clone(), settings);

        let t0 = Instant::now();
        // First two crashes are within `delay_starts_after_failures = 2`
        // so they are NoDelay; the third crash triggers backoff.
        assert_eq!(
            manager
                .register_unexpected_daemon_exit(t0)
                .expect("register"),
            CrashLoopDecision::NoDelay
        );
        assert_eq!(
            manager
                .register_unexpected_daemon_exit(t0 + Duration::from_secs(1))
                .expect("register"),
            CrashLoopDecision::NoDelay
        );
        assert_eq!(
            manager
                .register_unexpected_daemon_exit(t0 + Duration::from_secs(2))
                .expect("register"),
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
        let installer = installed_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager =
            DaemonLifecycleManager::with_crash_loop_policy(installer.clone(), settings, policy);

        let t0 = Instant::now();
        let _ = manager
            .register_unexpected_daemon_exit(t0)
            .expect("register");
        let _ = manager
            .register_unexpected_daemon_exit(t0 + Duration::from_secs(1))
            .expect("register");
        let decision = manager
            .register_unexpected_daemon_exit(t0 + Duration::from_secs(2))
            .expect("register");
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

        manager
            .acknowledge_crash_loop_pause(t0 + Duration::from_secs(3_650))
            .expect("acknowledge");
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

        manager
            .stop_daemon_for_termination(Instant::now())
            .expect("stop");
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

    // --- durable state ---

    #[test]
    fn successful_start_persists_supervision_expectation() {
        let installer = installed_installer();
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let clock = Arc::new(FixedWallClock::at(1_000_000));
        let manager = durable_manager_with(installer, store.clone(), clock, fixed_policy());

        manager
            .start_daemon_if_allowed(Instant::now())
            .expect("start");

        let state = store.current().expect("persisted state");
        assert!(state.daemon_should_be_running);
        assert!(!state.awaiting_restart);
        assert_eq!(state.consecutive_crashes, 0);
    }

    #[test]
    fn registered_crash_is_persisted_with_wall_clock_timestamp() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let clock = Arc::new(FixedWallClock::at(1_750_000_000_000));
        let manager = durable_manager_with(installer, store.clone(), clock, fixed_policy());

        manager
            .register_unexpected_daemon_exit(Instant::now())
            .expect("register");

        let state = store.current().expect("persisted state");
        assert_eq!(state.consecutive_crashes, 1);
        assert_eq!(state.last_crash_at_ms, Some(1_750_000_000_000));
        assert!(!state.paused_indefinitely);
    }

    #[test]
    fn crash_loop_pause_survives_a_process_restart() {
        // Headline scenario: a manager pauses, the process
        // dies, and a brand-new manager over the same store still
        // refuses to start the daemon.
        let policy = CrashLoopPolicy::new(
            Duration::from_secs(600),
            Duration::from_secs(2),
            Duration::from_secs(120),
            1,
            3,
        );
        let installer = installed_installer();
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let clock = Arc::new(FixedWallClock::at(1_000_000));

        let first = durable_manager_with(installer.clone(), store.clone(), clock.clone(), policy);
        let t0 = Instant::now();
        let _ = first.register_unexpected_daemon_exit(t0).expect("register");
        let _ = first
            .register_unexpected_daemon_exit(t0 + Duration::from_millis(10))
            .expect("register");
        let decision = first
            .register_unexpected_daemon_exit(t0 + Duration::from_millis(20))
            .expect("register");
        assert_eq!(decision, CrashLoopDecision::Paused);
        drop(first);

        let second = durable_manager_with(installer.clone(), store.clone(), clock, policy);
        assert!(second.is_in_crash_loop_pause());
        let result = second
            .start_daemon_if_allowed(Instant::now())
            .expect("start");
        assert!(matches!(
            result,
            DaemonLifecycleActionResult::RelaunchDeferred(remaining)
                if remaining == Duration::MAX
        ));
        assert!(installer.operations().is_empty());

        // Acknowledgement clears the pause durably too.
        second
            .acknowledge_crash_loop_pause(Instant::now())
            .expect("acknowledge");
        let state = store.current().expect("persisted state");
        assert!(!state.paused_indefinitely);
        assert_eq!(state.consecutive_crashes, 0);

        let third = durable_manager_with(
            installer,
            store,
            Arc::new(FixedWallClock::at(1_000_000)),
            policy,
        );
        assert!(!third.is_in_crash_loop_pause());
    }

    #[test]
    fn backoff_window_survives_a_process_restart() {
        let installer = installed_installer();
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let clock = Arc::new(FixedWallClock::at(1_000_000));

        let first = durable_manager_with(
            installer.clone(),
            store.clone(),
            clock.clone(),
            fixed_policy(),
        );
        let t0 = Instant::now();
        let _ = first.register_unexpected_daemon_exit(t0).expect("register");
        let _ = first
            .register_unexpected_daemon_exit(t0 + Duration::from_millis(10))
            .expect("register");
        // Third crash → Backoff(2 s) with the fixed policy.
        let decision = first
            .register_unexpected_daemon_exit(t0 + Duration::from_millis(20))
            .expect("register");
        assert_eq!(decision, CrashLoopDecision::Backoff(Duration::from_secs(2)));
        drop(first);

        // "Restart" 500 ms later (wall clock advanced): ~1.5 s of the
        // backoff window must still be in force.
        clock.set(1_000_500);
        let second = durable_manager_with(installer.clone(), store, clock, fixed_policy());
        let result = second
            .start_daemon_if_allowed(Instant::now())
            .expect("start");
        match result {
            DaemonLifecycleActionResult::RelaunchDeferred(remaining) => {
                assert!(
                    remaining > Duration::from_millis(1_300)
                        && remaining <= Duration::from_millis(1_500),
                    "expected ~1.5 s remaining, got {remaining:?}"
                );
            }
            other => panic!("expected RelaunchDeferred, got {other:?}"),
        }
        assert!(installer.operations().is_empty());
    }

    // --- check_daemon_health ---

    fn check_policy() -> CrashLoopPolicy {
        CrashLoopPolicy::new(
            Duration::from_secs(600),
            Duration::from_secs(2),
            Duration::from_secs(120),
            1,
            3,
        )
    }

    #[test]
    fn check_reports_running_daemon_as_healthy() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let clock = Arc::new(FixedWallClock::at(0));
        let manager = durable_manager_with(installer.clone(), store, clock, check_policy());

        installer.set_status_for_testing(ServiceStatus::Running);
        let outcome = manager.check_daemon_health(Instant::now()).expect("check");
        assert_eq!(outcome, DaemonHealthCheckOutcome::Running);
    }

    #[test]
    fn check_reports_not_installed_service() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let clock = Arc::new(FixedWallClock::at(0));
        let manager = durable_manager_with(installer.clone(), store, clock, check_policy());

        installer.set_status_for_testing(ServiceStatus::NotInstalled);
        let outcome = manager.check_daemon_health(Instant::now()).expect("check");
        assert_eq!(outcome, DaemonHealthCheckOutcome::NotInstalled);
    }

    #[test]
    fn check_does_not_count_an_expected_stop_as_a_crash() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let clock = Arc::new(FixedWallClock::at(0));
        let manager = durable_manager_with(installer.clone(), store.clone(), clock, check_policy());

        let t0 = Instant::now();
        manager.start_daemon_if_allowed(t0).expect("start");
        manager
            .stop_daemon_for_termination(t0 + Duration::from_secs(1))
            .expect("stop");
        installer.set_status_for_testing(ServiceStatus::Stopped);

        let outcome = manager
            .check_daemon_health(t0 + Duration::from_secs(2))
            .expect("check");
        assert_eq!(outcome, DaemonHealthCheckOutcome::StoppedExpected);
        assert_eq!(store.current().expect("state").consecutive_crashes, 0);
    }

    #[test]
    fn check_restarts_immediately_on_first_unexpected_exit() {
        let installer = installed_installer();
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let clock = Arc::new(FixedWallClock::at(10_000));
        let manager = durable_manager_with(installer.clone(), store.clone(), clock, check_policy());

        let t0 = Instant::now();
        manager.start_daemon_if_allowed(t0).expect("start");
        // Simulate the daemon dying.
        installer.set_status_for_testing(ServiceStatus::Stopped);

        let outcome = manager
            .check_daemon_health(t0 + Duration::from_secs(5))
            .expect("check");
        assert_eq!(outcome, DaemonHealthCheckOutcome::RestartedAfterCrash);
        assert_eq!(installer.operations(), vec!["start", "start"]);

        let state = store.current().expect("state");
        assert_eq!(state.consecutive_crashes, 1);
        assert!(!state.awaiting_restart);
        assert!(state.daemon_should_be_running);
    }

    #[test]
    fn check_defers_restart_during_backoff_and_restarts_after_it_elapses() {
        let installer = installed_installer();
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let clock = Arc::new(FixedWallClock::at(10_000));
        let manager = durable_manager_with(installer.clone(), store.clone(), clock, check_policy());

        let t0 = Instant::now();
        manager.start_daemon_if_allowed(t0).expect("start");

        // Crash 1 → immediate restart.
        installer.set_status_for_testing(ServiceStatus::Stopped);
        assert_eq!(
            manager
                .check_daemon_health(t0 + Duration::from_secs(1))
                .expect("check"),
            DaemonHealthCheckOutcome::RestartedAfterCrash
        );

        // Crash 2 → Backoff(2 s): deferred, and NOT double-counted on a
        // second observation of the same exit.
        installer.set_status_for_testing(ServiceStatus::Stopped);
        let outcome = manager
            .check_daemon_health(t0 + Duration::from_secs(2))
            .expect("check");
        assert_eq!(
            outcome,
            DaemonHealthCheckOutcome::RestartDeferred(Duration::from_secs(2))
        );
        let observed_again = manager
            .check_daemon_health(t0 + Duration::from_secs(3))
            .expect("check");
        assert!(matches!(
            observed_again,
            DaemonHealthCheckOutcome::RestartDeferred(remaining)
                if remaining <= Duration::from_secs(1)
        ));
        assert_eq!(store.current().expect("state").consecutive_crashes, 2);

        // Backoff elapsed → the pending restart happens.
        let restarted = manager
            .check_daemon_health(t0 + Duration::from_secs(10))
            .expect("check");
        assert_eq!(restarted, DaemonHealthCheckOutcome::RestartedAfterCrash);
        assert_eq!(
            installer.operations(),
            vec!["start", "start", "start"],
            "initial start + crash-1 restart + post-backoff restart"
        );
    }

    #[test]
    fn check_pauses_after_repeated_crashes_and_restarts_only_after_acknowledge() {
        let installer = installed_installer();
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let clock = Arc::new(FixedWallClock::at(10_000));
        let manager = durable_manager_with(installer.clone(), store.clone(), clock, check_policy());

        let t0 = Instant::now();
        manager.start_daemon_if_allowed(t0).expect("start");

        // Crash 1: restart. Crash 2: backoff, then restart. Crash 3:
        // paused (policy pauses on the 3rd crash in the window).
        installer.set_status_for_testing(ServiceStatus::Stopped);
        let _ = manager
            .check_daemon_health(t0 + Duration::from_secs(1))
            .expect("check");
        installer.set_status_for_testing(ServiceStatus::Stopped);
        let _ = manager
            .check_daemon_health(t0 + Duration::from_secs(2))
            .expect("check");
        let _ = manager
            .check_daemon_health(t0 + Duration::from_secs(10))
            .expect("check");
        installer.set_status_for_testing(ServiceStatus::Stopped);
        let outcome = manager
            .check_daemon_health(t0 + Duration::from_secs(11))
            .expect("check");
        assert_eq!(outcome, DaemonHealthCheckOutcome::CrashLoopPaused);
        assert!(manager.is_in_crash_loop_pause());

        // Still paused on later ticks; no restart attempts.
        let still_paused = manager
            .check_daemon_health(t0 + Duration::from_secs(500))
            .expect("check");
        assert_eq!(still_paused, DaemonHealthCheckOutcome::CrashLoopPaused);

        // Acknowledge → the next tick restarts.
        manager
            .acknowledge_crash_loop_pause(t0 + Duration::from_secs(600))
            .expect("acknowledge");
        let restarted = manager
            .check_daemon_health(t0 + Duration::from_secs(601))
            .expect("check");
        assert_eq!(restarted, DaemonHealthCheckOutcome::RestartedAfterCrash);
    }

    #[test]
    fn check_clears_pending_restart_bookkeeping_when_daemon_reappears() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let clock = Arc::new(FixedWallClock::at(10_000));
        let manager = durable_manager_with(installer.clone(), store.clone(), clock, check_policy());

        let t0 = Instant::now();
        manager.start_daemon_if_allowed(t0).expect("start");
        installer.set_status_for_testing(ServiceStatus::Stopped);
        let _ = manager
            .check_daemon_health(t0 + Duration::from_secs(1))
            .expect("check"); // crash 1 → restarted
        installer.set_status_for_testing(ServiceStatus::Stopped);
        let _ = manager
            .check_daemon_health(t0 + Duration::from_secs(2))
            .expect("check"); // crash 2 → deferred

        // Someone starts it manually while the backoff is pending.
        installer.set_status_for_testing(ServiceStatus::Running);
        let outcome = manager
            .check_daemon_health(t0 + Duration::from_secs(3))
            .expect("check");
        assert_eq!(outcome, DaemonHealthCheckOutcome::Running);
        let state = store.current().expect("state");
        assert!(!state.awaiting_restart);
        assert!(state.daemon_should_be_running);
    }

    #[test]
    fn crash_loop_state_snapshot_reflects_bookkeeping() {
        let installer = Arc::new(InMemoryServiceInstaller::new(descriptor()));
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let clock = Arc::new(FixedWallClock::at(42_000));
        let manager = durable_manager_with(installer, store, clock, fixed_policy());

        let t0 = Instant::now();
        let snapshot = manager.crash_loop_state(t0);
        assert_eq!(snapshot.consecutive_crashes, 0);
        assert_eq!(snapshot.last_crash_at_ms, None);
        assert!(!snapshot.paused);

        manager
            .register_unexpected_daemon_exit(t0)
            .expect("register");
        let snapshot = manager.crash_loop_state(t0 + Duration::from_secs(1));
        assert_eq!(snapshot.consecutive_crashes, 1);
        assert_eq!(snapshot.last_crash_at_ms, Some(42_000));
    }
}
