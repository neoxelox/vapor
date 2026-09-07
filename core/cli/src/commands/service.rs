//! `vapor service bootstrap|install|uninstall|start|stop|restart|status|check|acknowledge`.
//!
//! `check --loop` is the headless supervisor: what the macOS app's
//! health tick does every 30 seconds, as a process, for a CLI-only
//! install. `install --supervise` registers it with the service
//! manager as a second, kept-alive job (`sh.arn.vapor.supervisor`).
//!
//! Drives `core/lifecycle::DaemonLifecycleManager` over the platform's
//! native `ServiceInstaller`. macOS today; Windows / Linux land with
//! their platform support. Together with the durable lifecycle state
//! in `core/lifecycle`, this provides the stable subprocess surface
//! the macOS Swift app consumes.
//!
//! Every subcommand renders both a human line and, with `--json`, a
//! stable machine shape (documented per-variant on
//! [`ServiceCommandOutcome`]). The Swift shim parses only the JSON
//! form; treat key names and value enums as a versioned contract.
//!
//! The `restart` command is a stop-then-start sequence; both halves
//! tolerate the daemon already being in the target state.

use std::error::Error;
use std::fmt::{self, Display};
use std::path::PathBuf;
// `Arc` is only referenced by the `build_native` constructor (macOS and
// Linux) and by the tests (which build `Arc<InMemory*>` fakes); it is
// unused on the Windows lib build, which `-D warnings` treats as an error.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
use std::sync::Arc;
use std::time::{Duration, Instant};

use vapor_lifecycle::{
    CrashLoopStateSnapshot, DaemonHealthCheckOutcome, DaemonLifecycleActionResult,
    DaemonLifecycleError, DaemonLifecycleManager,
};
// The `AutoLaunchSettingStore` trait is needed by `build_native` and by the
// tests (its `read` method is called on the in-memory store); the concrete
// JSON-file stores only exist where a native service manager does.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
use vapor_lifecycle::AutoLaunchSettingStore;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use vapor_lifecycle::{
    CrashLoopPolicy, JsonFileAutoLaunchSettingStore, JsonFileLifecycleStateStore, SystemWallClock,
};
use vapor_platform::{ServiceInstallError, ServiceInstaller, ServiceStatus};
// The native installer type + descriptor are only used by `build_native`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
use vapor_platform::{NativeServiceInstaller, ServiceDescriptor};
use vapor_shared::constants;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceCommand {
    /// App-startup path: install + start only when autolaunch is
    /// enabled; a disabled autolaunch is a silent no-op.
    Bootstrap,
    Install,
    Uninstall {
        /// Skip the explicit stop signal (the "disable autolaunch"
        /// toggle path); default is to send one. On macOS the daemon
        /// exits regardless — launchd tears the job down when its
        /// service definition is booted out — so this only controls
        /// whether *we* signal it; it matters on service managers that
        /// keep a disabled unit running (e.g. systemd).
        keep_running: bool,
    },
    Start,
    Stop,
    Restart,
    Status,
    /// One supervision tick: detect an unexpected daemon exit, register
    /// it with the crash-loop guard, restart when policy allows.
    Check,
    /// Clear a crash-loop pause so restarts may resume.
    Acknowledge,
}

#[derive(Debug)]
pub enum ServiceCommandError {
    /// The platform installer rejected the request (e.g. `launchctl`
    /// returned a non-zero exit code or the daemon binary was missing).
    Install(ServiceInstallError),
    /// Some other lifecycle layer (autolaunch settings or durable
    /// lifecycle state persistence) failed.
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
    pub auto_launch: bool,
    pub crash_loop: CrashLoopStateSnapshot,
    /// Whether the headless supervisor job is registered.
    pub supervisor_installed: bool,
}

/// What a service subcommand produced. Rendered by [`render_text`] /
/// [`render_json`]; the JSON form is the contract the macOS app shim
/// parses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceCommandOutcome {
    /// `bootstrap` / `install` / `uninstall` / `start` / `stop` /
    /// `restart` — a lifecycle action result.
    Action(DaemonLifecycleActionResult),
    /// `check` — a supervision-tick outcome.
    Health(DaemonHealthCheckOutcome),
    /// `status` — the full report.
    Status(ServiceStatusReport),
    /// `acknowledge` — pause cleared.
    Acknowledged,
}

/// Entry point for the binary's `clap` dispatch. The `manager`,
/// `installer`, and current-time hooks are split out so unit tests can
/// substitute fakes without touching real `launchctl`.
pub fn dispatch(
    command: ServiceCommand,
    manager: &DaemonLifecycleManager,
    installer: &dyn ServiceInstaller,
    now: Instant,
) -> Result<ServiceCommandOutcome, ServiceCommandError> {
    match command {
        ServiceCommand::Bootstrap => Ok(ServiceCommandOutcome::Action(
            manager.bootstrap_if_needed(now)?,
        )),
        ServiceCommand::Install => Ok(ServiceCommandOutcome::Action(
            manager.set_auto_launch_enabled(true, false, now)?,
        )),
        ServiceCommand::Uninstall { keep_running } => Ok(ServiceCommandOutcome::Action(
            manager.set_auto_launch_enabled(false, !keep_running, now)?,
        )),
        ServiceCommand::Start => Ok(ServiceCommandOutcome::Action(
            manager.start_daemon_if_allowed(now)?,
        )),
        ServiceCommand::Stop => {
            // Probe before the (best-effort, always-attempted) stop so we
            // report `Stopped` only when something was actually running;
            // `vapor service stop` on a machine that never installed the
            // service must not claim it stopped one.
            let was_running = installer.status()? == ServiceStatus::Running;
            manager.stop_daemon_for_termination(now)?;
            let result = if was_running {
                DaemonLifecycleActionResult::Stopped
            } else {
                DaemonLifecycleActionResult::Unchanged
            };
            Ok(ServiceCommandOutcome::Action(result))
        }
        ServiceCommand::Restart => {
            manager.stop_daemon_for_termination(now)?;
            Ok(ServiceCommandOutcome::Action(
                manager.start_daemon_if_allowed(now)?,
            ))
        }
        ServiceCommand::Status => {
            let probed = installer.status()?;
            // The installer can only see the OS service manager; the
            // crash-loop pause lives in the durable lifecycle state.
            // Overlay it so `CrashLoopPaused` is a real, reportable
            // state — unless the daemon is observably running.
            let status = if manager.is_in_crash_loop_pause() && probed != ServiceStatus::Running {
                ServiceStatus::CrashLoopPaused
            } else {
                probed
            };
            Ok(ServiceCommandOutcome::Status(ServiceStatusReport {
                status,
                label: constants::service::DAEMON_LABEL.to_string(),
                auto_launch: manager.auto_launch_enabled()?,
                crash_loop: manager.crash_loop_state(now),
                supervisor_installed: false,
            }))
        }
        ServiceCommand::Check => Ok(ServiceCommandOutcome::Health(
            manager.check_daemon_health(now)?,
        )),
        ServiceCommand::Acknowledge => {
            manager.acknowledge_crash_loop_pause(now)?;
            Ok(ServiceCommandOutcome::Acknowledged)
        }
    }
}

fn seconds(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 10.0).round() / 10.0
}

fn status_wire_name(status: ServiceStatus) -> &'static str {
    match status {
        ServiceStatus::NotInstalled => "not_installed",
        ServiceStatus::Stopped => "stopped",
        ServiceStatus::Running => "running",
        ServiceStatus::CrashLoopPaused => "crash_loop_paused",
    }
}

/// Stable machine-readable rendering (`--json`). The Swift app shim
/// parses this; key names and value enums are a versioned contract —
/// change them only with a coordinated Swift-side update.
pub fn render_json(outcome: &ServiceCommandOutcome) -> serde_json::Value {
    match outcome {
        ServiceCommandOutcome::Action(action) => match action {
            DaemonLifecycleActionResult::Unchanged => serde_json::json!({"result": "unchanged"}),
            DaemonLifecycleActionResult::Started => serde_json::json!({"result": "started"}),
            DaemonLifecycleActionResult::Stopped => serde_json::json!({"result": "stopped"}),
            DaemonLifecycleActionResult::RelaunchDeferred(remaining) => {
                if *remaining == Duration::MAX {
                    serde_json::json!({"result": "crash_loop_paused"})
                } else {
                    serde_json::json!({
                        "result": "relaunch_deferred",
                        "remaining_seconds": seconds(*remaining),
                    })
                }
            }
        },
        ServiceCommandOutcome::Health(health) => match health {
            DaemonHealthCheckOutcome::Running => serde_json::json!({"health": "running"}),
            DaemonHealthCheckOutcome::NotInstalled => {
                serde_json::json!({"health": "not_installed"})
            }
            DaemonHealthCheckOutcome::StoppedExpected => {
                serde_json::json!({"health": "stopped_expected"})
            }
            DaemonHealthCheckOutcome::RestartedAfterCrash => {
                serde_json::json!({"health": "restarted_after_crash"})
            }
            DaemonHealthCheckOutcome::RestartDeferred(remaining) => serde_json::json!({
                "health": "restart_deferred",
                "remaining_seconds": seconds(*remaining),
            }),
            DaemonHealthCheckOutcome::CrashLoopPaused => {
                serde_json::json!({"health": "crash_loop_paused"})
            }
        },
        ServiceCommandOutcome::Status(report) => serde_json::json!({
            "status": status_wire_name(report.status),
            "label": report.label,
            "auto_launch": report.auto_launch,
            "crash_loop": {
                "paused": report.crash_loop.paused,
                "consecutive_crashes": report.crash_loop.consecutive_crashes,
                "last_crash_at_ms": report.crash_loop.last_crash_at_ms,
                "awaiting_restart": report.crash_loop.awaiting_restart,
            },
            "supervisor_installed": report.supervisor_installed,
        }),
        ServiceCommandOutcome::Acknowledged => serde_json::json!({"result": "acknowledged"}),
    }
}

/// Human rendering (default, no `--json`).
pub fn render_text(outcome: &ServiceCommandOutcome) -> String {
    match outcome {
        ServiceCommandOutcome::Action(action) => match action {
            DaemonLifecycleActionResult::Unchanged => "service: no change".to_string(),
            DaemonLifecycleActionResult::Started => "service: daemon started".to_string(),
            DaemonLifecycleActionResult::Stopped => "service: daemon stopped".to_string(),
            DaemonLifecycleActionResult::RelaunchDeferred(remaining) => {
                if *remaining == Duration::MAX {
                    "service: crash-loop pause active — run `vapor service acknowledge` \
                     to allow restarts"
                        .to_string()
                } else {
                    format!(
                        "service: start deferred {:.1}s by crash-loop backoff",
                        remaining.as_secs_f64()
                    )
                }
            }
        },
        ServiceCommandOutcome::Health(health) => match health {
            DaemonHealthCheckOutcome::Running => "service check: daemon running".to_string(),
            DaemonHealthCheckOutcome::NotInstalled => {
                "service check: service not installed".to_string()
            }
            DaemonHealthCheckOutcome::StoppedExpected => {
                "service check: daemon stopped (expected)".to_string()
            }
            DaemonHealthCheckOutcome::RestartedAfterCrash => {
                "service check: unexpected exit detected — daemon restarted".to_string()
            }
            DaemonHealthCheckOutcome::RestartDeferred(remaining) => format!(
                "service check: unexpected exit detected — restart deferred {:.1}s",
                remaining.as_secs_f64()
            ),
            DaemonHealthCheckOutcome::CrashLoopPaused => {
                "service check: crash-loop pause active — run `vapor service acknowledge`"
                    .to_string()
            }
        },
        ServiceCommandOutcome::Status(report) => {
            let mut line = format!(
                "service status: {:?} (label: {}, autolaunch: {})",
                report.status,
                report.label,
                if report.auto_launch { "on" } else { "off" }
            );
            if report.crash_loop.paused {
                line.push_str(
                    "\ncrash-loop: paused — run `vapor service acknowledge` to allow restarts",
                );
            } else if report.crash_loop.consecutive_crashes > 0 {
                line.push_str(&format!(
                    "\ncrash-loop: {} recent crash(es) in window",
                    report.crash_loop.consecutive_crashes
                ));
            }
            line.push_str(if report.supervisor_installed {
                "\nsupervisor: installed (vapor service check --loop runs under the service manager)"
            } else {
                "\nsupervisor: not installed (the app supervises; headless installs use `vapor service install --supervise`)"
            });
            line
        }
        ServiceCommandOutcome::Acknowledged => "service: crash-loop pause acknowledged".to_string(),
    }
}

/// Production-mode constructor for the native service surface: a
/// LaunchAgent on macOS, a systemd user unit on Linux. Looks for the
/// bundled daemon binary at `<cli-binary-parent>/vapord`, falling back
/// to `PATH` lookup. Returns the manager + installer pair so the binary
/// can call `dispatch` against them.
///
/// The service descriptor follows
/// `docs/operations/macos/launchagent-policy.md`: stdout/stderr are
/// redirected under `<vapor_dir>/logs/`, and the environment carries
/// only `VAPOR_DIR` (plus `VAPOR_ENV` when set in the invoking
/// environment); every other setting reaches the daemon through
/// `vapor.json`. This keeps the definition identical no matter which
/// surface (CLI or app shim) drives the install.
///
/// The manager is built over the durable lifecycle state at
/// `<vapor_dir>/state/lifecycle.json`, so crash-loop backoff and pause
/// survive across invocations and surfaces.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn build_native(
    config_path: PathBuf,
    daemon_binary: PathBuf,
) -> Result<(DaemonLifecycleManager, Arc<NativeServiceInstaller>), ServiceCommandError> {
    if !daemon_binary.is_file() {
        return Err(ServiceCommandError::DaemonBinaryMissing(daemon_binary));
    }

    let vapor_directory = vapor_shared::runtime_paths::vapor_directory();
    let logs_directory = vapor_shared::runtime_paths::logs_directory();
    let mut environment = vec![(
        vapor_shared::constants::env::VAPOR_DIR.to_string(),
        vapor_directory.display().to_string(),
    )];
    if let Ok(vapor_env) = std::env::var(vapor_shared::constants::env::VAPOR_ENV)
        && !vapor_env.trim().is_empty()
    {
        environment.push((
            vapor_shared::constants::env::VAPOR_ENV.to_string(),
            vapor_env,
        ));
    }

    let descriptor = ServiceDescriptor {
        label: constants::service::DAEMON_LABEL.to_string(),
        executable_path: daemon_binary,
        arguments: vec![],
        environment,
        stdout_path: Some(logs_directory.join(constants::runtime::DAEMON_STDOUT_LOG_FILE_NAME)),
        stderr_path: Some(logs_directory.join(constants::runtime::DAEMON_STDERR_LOG_FILE_NAME)),
        keep_alive: false,
    };
    let installer = Arc::new(NativeServiceInstaller::for_current_user(descriptor)?);
    let settings: Arc<dyn AutoLaunchSettingStore> =
        Arc::new(JsonFileAutoLaunchSettingStore::new(config_path));
    let state_store = Arc::new(JsonFileLifecycleStateStore::new(
        vapor_shared::runtime_paths::lifecycle_state_path(),
    ));
    let manager = DaemonLifecycleManager::with_durable_state(
        installer.clone(),
        settings,
        CrashLoopPolicy::default(),
        state_store,
        Arc::new(SystemWallClock),
    )?;
    Ok((manager, installer))
}

/// The headless supervisor's service definition: this very `vapor`
/// binary running `service check --loop`, kept alive by the service
/// manager, with the same runtime directory as the daemon.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn supervisor_installer() -> Result<NativeServiceInstaller, ServiceCommandError> {
    let executable = std::env::current_exe().map_err(|error| {
        ServiceCommandError::Install(ServiceInstallError::Backend(Box::new(error)))
    })?;
    let vapor_directory = vapor_shared::runtime_paths::vapor_directory();
    let logs_directory = vapor_shared::runtime_paths::logs_directory();
    let mut environment = vec![(
        vapor_shared::constants::env::VAPOR_DIR.to_string(),
        vapor_directory.display().to_string(),
    )];
    if let Ok(vapor_env) = std::env::var(vapor_shared::constants::env::VAPOR_ENV)
        && !vapor_env.trim().is_empty()
    {
        environment.push((
            vapor_shared::constants::env::VAPOR_ENV.to_string(),
            vapor_env,
        ));
    }
    let descriptor = ServiceDescriptor {
        label: constants::service::SUPERVISOR_LABEL.to_string(),
        executable_path: executable,
        arguments: vec![
            "service".to_string(),
            "check".to_string(),
            "--loop".to_string(),
        ],
        environment,
        stdout_path: Some(logs_directory.join(constants::runtime::SUPERVISOR_LOG_FILE_NAME)),
        stderr_path: Some(logs_directory.join(constants::runtime::SUPERVISOR_LOG_FILE_NAME)),
        keep_alive: true,
    };
    Ok(NativeServiceInstaller::for_current_user(descriptor)?)
}

/// Runs supervision ticks every `interval` until a shutdown signal
/// arrives, printing an outcome only when it differs from the previous
/// tick so the log stays quiet while the daemon is healthy. Returns
/// the last outcome.
pub fn check_loop(
    manager: &DaemonLifecycleManager,
    installer: &dyn ServiceInstaller,
    interval: Duration,
    json: bool,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Option<DaemonHealthCheckOutcome>, ServiceCommandError> {
    use std::sync::atomic::Ordering;
    let mut last: Option<DaemonHealthCheckOutcome> = None;
    while !stop.load(Ordering::SeqCst) {
        let outcome = match dispatch(ServiceCommand::Check, manager, installer, Instant::now())? {
            ServiceCommandOutcome::Health(outcome) => outcome,
            _ => unreachable!("check yields a health outcome"),
        };
        if last.as_ref() != Some(&outcome) {
            let rendered = ServiceCommandOutcome::Health(outcome.clone());
            if json {
                println!("{}", render_json(&rendered));
            } else {
                println!("{}", render_text(&rendered));
            }
            last = Some(outcome.clone());
        }
        if outcome == DaemonHealthCheckOutcome::NotInstalled {
            // Nothing to supervise; looping would only log the same
            // line forever.
            break;
        }
        let deadline = Instant::now() + interval;
        while Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(250).min(deadline - Instant::now()));
        }
    }
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vapor_lifecycle::{
        CrashLoopPolicy, FixedWallClock, InMemoryAutoLaunchSettingStore,
        InMemoryLifecycleStateStore,
    };
    use vapor_platform::{InMemoryServiceInstaller, ServiceDescriptor};

    fn fake_installer() -> Arc<InMemoryServiceInstaller> {
        Arc::new(InMemoryServiceInstaller::new(ServiceDescriptor {
            label: "sh.arn.vapor.test".to_string(),
            executable_path: PathBuf::from("/usr/bin/false"),
            arguments: vec![],
            environment: vec![],
            stdout_path: None,
            stderr_path: None,
            keep_alive: false,
        }))
    }

    fn durable_manager(
        installer: Arc<InMemoryServiceInstaller>,
        store: Arc<InMemoryLifecycleStateStore>,
    ) -> DaemonLifecycleManager {
        DaemonLifecycleManager::with_durable_state(
            installer,
            Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true))),
            CrashLoopPolicy::new(
                Duration::from_secs(600),
                Duration::from_secs(2),
                Duration::from_secs(120),
                1,
                3,
            ),
            store,
            Arc::new(FixedWallClock::at(1_000)),
        )
        .expect("durable manager")
    }

    #[test]
    fn install_sets_auto_launch_and_records_install_then_start() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::new());
        let manager = DaemonLifecycleManager::new(installer.clone(), settings.clone());

        let outcome = dispatch(
            ServiceCommand::Install,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("install");

        assert_eq!(
            outcome,
            ServiceCommandOutcome::Action(DaemonLifecycleActionResult::Started)
        );
        assert_eq!(installer.operations(), vec!["install", "start"]);
        assert_eq!(settings.read().expect("read"), Some(true));
    }

    #[test]
    fn uninstall_disables_auto_launch_and_stops_daemon() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings.clone());

        let outcome = dispatch(
            ServiceCommand::Uninstall {
                keep_running: false,
            },
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("uninstall");

        assert_eq!(
            outcome,
            ServiceCommandOutcome::Action(DaemonLifecycleActionResult::Stopped)
        );
        assert_eq!(installer.operations(), vec!["uninstall", "stop"]);
        assert_eq!(settings.read().expect("read"), Some(false));
    }

    #[test]
    fn uninstall_keep_running_skips_the_explicit_stop_signal() {
        // `--keep-running` suppresses the explicit stop; on macOS the
        // real installer's bootout still terminates the job (launchd
        // semantics), so the name promises signal behavior, not
        // process survival.
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings.clone());

        let outcome = dispatch(
            ServiceCommand::Uninstall { keep_running: true },
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("uninstall");

        assert_eq!(
            outcome,
            ServiceCommandOutcome::Action(DaemonLifecycleActionResult::Unchanged)
        );
        assert_eq!(installer.operations(), vec!["uninstall"]);
        assert_eq!(settings.read().expect("read"), Some(false));
    }

    #[test]
    fn bootstrap_respects_disabled_auto_launch() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(false)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);

        let outcome = dispatch(
            ServiceCommand::Bootstrap,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("bootstrap");

        assert_eq!(
            outcome,
            ServiceCommandOutcome::Action(DaemonLifecycleActionResult::Unchanged)
        );
        assert!(installer.operations().is_empty());
    }

    #[test]
    fn bootstrap_installs_and_starts_when_auto_launch_enabled() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);

        let outcome = dispatch(
            ServiceCommand::Bootstrap,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("bootstrap");

        assert_eq!(
            outcome,
            ServiceCommandOutcome::Action(DaemonLifecycleActionResult::Started)
        );
        assert_eq!(installer.operations(), vec!["install", "start"]);
    }

    #[test]
    fn the_check_loop_restarts_a_crashed_daemon_and_stops_on_signal() {
        let installer = fake_installer();
        installer.set_status_for_testing(ServiceStatus::Running);
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);
        // One healthy tick establishes "should be running"; then the
        // daemon dies between ticks and the loop brings it back.
        dispatch(
            ServiceCommand::Check,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("first check");
        installer.set_status_for_testing(ServiceStatus::Stopped);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let last = check_loop(
            &manager,
            installer.as_ref(),
            Duration::from_millis(20),
            false,
            &stop,
        )
        .expect("loop");
        stopper.join().expect("stopper");
        assert!(
            installer.operations().contains(&"start"),
            "the loop must restart the crashed daemon: {:?}",
            installer.operations()
        );
        assert!(
            matches!(
                last,
                Some(DaemonHealthCheckOutcome::RestartedAfterCrash)
                    | Some(DaemonHealthCheckOutcome::Running)
            ),
            "{last:?}"
        );
    }

    #[test]
    fn the_check_loop_ends_when_nothing_is_installed() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);
        let stop = std::sync::atomic::AtomicBool::new(false);
        let last = check_loop(
            &manager,
            installer.as_ref(),
            Duration::from_secs(60),
            false,
            &stop,
        )
        .expect("loop");
        assert_eq!(last, Some(DaemonHealthCheckOutcome::NotInstalled));
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
    fn stop_reports_stopped_only_when_the_service_was_running() {
        let installer = fake_installer();
        installer.set_status_for_testing(ServiceStatus::Running);
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);

        let outcome = dispatch(
            ServiceCommand::Stop,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("stop");
        assert_eq!(
            outcome,
            ServiceCommandOutcome::Action(DaemonLifecycleActionResult::Stopped)
        );
        assert_eq!(installer.operations(), vec!["stop"]);
    }

    #[test]
    fn stop_reports_unchanged_when_nothing_was_installed_or_running() {
        // Default fake status is NotInstalled: `vapor service stop` must not
        // claim it stopped a daemon that was never there.
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);

        let outcome = dispatch(
            ServiceCommand::Stop,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("stop");
        assert_eq!(
            outcome,
            ServiceCommandOutcome::Action(DaemonLifecycleActionResult::Unchanged)
        );
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

        let outcome = dispatch(
            ServiceCommand::Status,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("status");
        let ServiceCommandOutcome::Status(report) = outcome else {
            panic!("expected status outcome");
        };
        assert_eq!(report.status, ServiceStatus::Running);
        assert!(report.auto_launch);
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

        let outcome = dispatch(
            ServiceCommand::Status,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("status");
        let ServiceCommandOutcome::Status(report) = outcome else {
            panic!("expected status outcome");
        };

        assert_eq!(report.label, constants::service::DAEMON_LABEL);
        assert_ne!(report.label, constants::runtime::DAEMON_LOG_FILE_NAME);
    }

    #[test]
    fn status_overlays_crash_loop_pause_over_a_stopped_service() {
        let installer = fake_installer();
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let manager = durable_manager(installer.clone(), store);

        let t0 = Instant::now();
        // Three crashes with the test policy → paused.
        let _ = manager.register_unexpected_daemon_exit(t0).expect("crash");
        let _ = manager
            .register_unexpected_daemon_exit(t0 + Duration::from_millis(10))
            .expect("crash");
        let _ = manager
            .register_unexpected_daemon_exit(t0 + Duration::from_millis(20))
            .expect("crash");
        installer.set_status_for_testing(ServiceStatus::Stopped);

        let outcome = dispatch(
            ServiceCommand::Status,
            &manager,
            installer.as_ref(),
            t0 + Duration::from_secs(1),
        )
        .expect("status");
        let ServiceCommandOutcome::Status(report) = outcome else {
            panic!("expected status outcome");
        };
        assert_eq!(report.status, ServiceStatus::CrashLoopPaused);
        assert!(report.crash_loop.paused);
        assert_eq!(report.crash_loop.consecutive_crashes, 3);
    }

    #[test]
    fn check_and_acknowledge_round_trip_through_dispatch() {
        let installer = fake_installer();
        let store = Arc::new(InMemoryLifecycleStateStore::new());
        let manager = durable_manager(installer.clone(), store);

        let t0 = Instant::now();
        dispatch(ServiceCommand::Start, &manager, installer.as_ref(), t0).expect("start");

        // Healthy tick.
        assert_eq!(
            dispatch(
                ServiceCommand::Check,
                &manager,
                installer.as_ref(),
                t0 + Duration::from_secs(1)
            )
            .expect("check"),
            ServiceCommandOutcome::Health(DaemonHealthCheckOutcome::Running)
        );

        // Daemon dies → first tick restarts it immediately.
        installer.set_status_for_testing(ServiceStatus::Stopped);
        assert_eq!(
            dispatch(
                ServiceCommand::Check,
                &manager,
                installer.as_ref(),
                t0 + Duration::from_secs(2)
            )
            .expect("check"),
            ServiceCommandOutcome::Health(DaemonHealthCheckOutcome::RestartedAfterCrash)
        );

        // Acknowledge clears any pause state through dispatch too.
        assert_eq!(
            dispatch(
                ServiceCommand::Acknowledge,
                &manager,
                installer.as_ref(),
                t0 + Duration::from_secs(3)
            )
            .expect("acknowledge"),
            ServiceCommandOutcome::Acknowledged
        );
    }

    // --- JSON contract (consumed by the macOS app shim; LT-1 style) ---

    fn json_string(outcome: &ServiceCommandOutcome) -> String {
        serde_json::to_string(&render_json(outcome)).expect("serialize")
    }

    #[test]
    fn json_contract_for_action_results_is_stable() {
        assert_eq!(
            json_string(&ServiceCommandOutcome::Action(
                DaemonLifecycleActionResult::Unchanged
            )),
            r#"{"result":"unchanged"}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Action(
                DaemonLifecycleActionResult::Started
            )),
            r#"{"result":"started"}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Action(
                DaemonLifecycleActionResult::Stopped
            )),
            r#"{"result":"stopped"}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Action(
                DaemonLifecycleActionResult::RelaunchDeferred(Duration::from_millis(1_500))
            )),
            r#"{"remaining_seconds":1.5,"result":"relaunch_deferred"}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Action(
                DaemonLifecycleActionResult::RelaunchDeferred(Duration::MAX)
            )),
            r#"{"result":"crash_loop_paused"}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Acknowledged),
            r#"{"result":"acknowledged"}"#
        );
    }

    #[test]
    fn json_contract_for_health_outcomes_is_stable() {
        assert_eq!(
            json_string(&ServiceCommandOutcome::Health(
                DaemonHealthCheckOutcome::Running
            )),
            r#"{"health":"running"}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Health(
                DaemonHealthCheckOutcome::NotInstalled
            )),
            r#"{"health":"not_installed"}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Health(
                DaemonHealthCheckOutcome::StoppedExpected
            )),
            r#"{"health":"stopped_expected"}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Health(
                DaemonHealthCheckOutcome::RestartedAfterCrash
            )),
            r#"{"health":"restarted_after_crash"}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Health(
                DaemonHealthCheckOutcome::RestartDeferred(Duration::from_secs(2))
            )),
            r#"{"health":"restart_deferred","remaining_seconds":2.0}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Health(
                DaemonHealthCheckOutcome::CrashLoopPaused
            )),
            r#"{"health":"crash_loop_paused"}"#
        );
    }

    #[test]
    fn json_contract_for_status_report_is_stable() {
        let report = ServiceStatusReport {
            status: ServiceStatus::CrashLoopPaused,
            label: constants::service::DAEMON_LABEL.to_string(),
            auto_launch: true,
            crash_loop: CrashLoopStateSnapshot {
                paused: true,
                consecutive_crashes: 5,
                last_crash_at_ms: Some(1_750_000_000_000),
                awaiting_restart: true,
            },
            supervisor_installed: true,
        };
        assert_eq!(
            json_string(&ServiceCommandOutcome::Status(report)),
            concat!(
                r#"{"auto_launch":true,"crash_loop":{"awaiting_restart":true,"#,
                r#""consecutive_crashes":5,"last_crash_at_ms":1750000000000,"paused":true},"#,
                r#""label":"sh.arn.vapor.daemon","status":"crash_loop_paused","supervisor_installed":true}"#
            )
        );
    }
}
