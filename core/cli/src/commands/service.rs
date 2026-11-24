//! `vapor service bootstrap|install|uninstall|start|stop|restart|status|check|acknowledge`.
//!
//! `check --loop` is the supervisor: the one process that restarts a
//! daemon that exited on its own, within the crash-loop limits. Every
//! install registers it with the service manager as a second,
//! kept-alive job (`sh.arn.vapor.supervisor`), so a crashed daemon
//! comes back with or without an app. The surfaces only read what it
//! did (`status`).
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
//! tolerate the daemon already being in the target state. With no
//! service definition registered, `start` and `restart` report
//! `not-installed` instead of asking the service manager to kick a job
//! it does not have; auto-launch (`vapor service install`) is what
//! registers one.

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
        /// exits regardless: launchd tears the job down when its
        /// service definition is booted out, so this only controls
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
    /// What a supervision tick would find, read with nothing changed.
    pub health: DaemonHealthCheckOutcome,
    /// Whether the supervisor job is registered.
    pub supervisor_installed: bool,
    /// Whether the daemon answers on its socket; `None` while its job
    /// does not run. A job that runs with a daemon that gives no
    /// answer syncs nothing.
    pub answers: Option<bool>,
}

/// What a service subcommand produced. Rendered by [`render_text`] /
/// [`render_json`]; the JSON form is the contract the macOS app shim
/// parses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceCommandOutcome {
    /// `bootstrap` / `install` / `uninstall` / `start` / `stop` /
    /// `restart`: a lifecycle action result.
    Action(DaemonLifecycleActionResult),
    /// `check`: a supervision-tick outcome.
    Health(DaemonHealthCheckOutcome),
    /// `status`: the full report.
    Status(ServiceStatusReport),
    /// `acknowledge`: pause cleared.
    Acknowledged,
}

/// How long `start` and `restart` wait for the daemon to answer on its
/// socket: a daemon that was just started needs a moment to open it.
pub const START_ANSWER_WAIT: Duration = Duration::from_secs(10);

/// Ends a `start` or a `restart` whose daemon gives no answer with an
/// error. A start is done when a daemon answers, not when the service
/// manager took the request: a daemon that hangs keeps its job, and a
/// start against it changes nothing. `answers_within` waits up to the
/// given time for an answer on the socket.
pub fn confirm_started(
    command: ServiceCommand,
    result: &DaemonLifecycleActionResult,
    answers_within: &dyn Fn(Duration) -> bool,
) -> Result<(), String> {
    let started = matches!(command, ServiceCommand::Start | ServiceCommand::Restart)
        && matches!(
            result,
            DaemonLifecycleActionResult::Started | DaemonLifecycleActionResult::Unchanged
        );
    if !started || answers_within(START_ANSWER_WAIT) {
        return Ok(());
    }
    Err(
        "the daemon's job runs, but the daemon gives no answer on its socket, so \
         nothing syncs. `vapor service restart` ends it and starts a new one; \
         `vapor logs` shows what it did last"
            .to_string(),
    )
}

/// What a daemon of this build says about itself in `vapor status`.
pub fn this_build() -> String {
    format!("vapord/{}", vapor_daemon::build_info::VERSION)
}

/// After an install or a bootstrap: stops a running daemon that is
/// another build than this binary and starts the installed one.
///
/// An update replaces the binaries and leaves the service definition
/// as it was, so the install changes nothing, and a start leaves a
/// running daemon alone. Without this step the old daemon would serve
/// the new CLI and app until the next login. `running_build` is what
/// the running daemon answered (`None` when it gave no answer, which a
/// daemon that was just started does).
pub fn replace_daemon_of_another_build(
    manager: &DaemonLifecycleManager,
    installer: &dyn ServiceInstaller,
    running_build: Option<&str>,
    now: Instant,
) -> Result<bool, ServiceCommandError> {
    let Some(running) = running_build else {
        return Ok(false);
    };
    if running == this_build() || installer.status()? != ServiceStatus::Running {
        return Ok(false);
    }
    manager.replace_daemon(now)?;
    Ok(true)
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
            if installer.status()? == ServiceStatus::NotInstalled {
                return Ok(ServiceCommandOutcome::Action(
                    DaemonLifecycleActionResult::NotInstalled,
                ));
            }
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
            // state, unless the daemon is observably running.
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
                health: manager.observe_daemon_health_for(probed, now)?,
                supervisor_installed: false,
                answers: None,
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
    use constants::service as names;
    match status {
        ServiceStatus::NotInstalled => names::STATUS_NOT_INSTALLED,
        ServiceStatus::Stopped => names::STATUS_STOPPED,
        ServiceStatus::Running => names::STATUS_RUNNING,
        ServiceStatus::CrashLoopPaused => names::STATUS_CRASH_LOOP_PAUSED,
    }
}

/// Stable machine-readable rendering (`--json`). The Swift app shim
/// parses this; key names and value enums are a versioned contract:
/// change them only with a coordinated Swift-side update.
pub fn render_json(outcome: &ServiceCommandOutcome) -> serde_json::Value {
    use constants::service as names;
    match outcome {
        ServiceCommandOutcome::Action(action) => match action {
            DaemonLifecycleActionResult::Unchanged => {
                serde_json::json!({"result": names::RESULT_UNCHANGED})
            }
            DaemonLifecycleActionResult::Started => {
                serde_json::json!({"result": names::RESULT_STARTED})
            }
            DaemonLifecycleActionResult::Stopped => {
                serde_json::json!({"result": names::RESULT_STOPPED})
            }
            DaemonLifecycleActionResult::RelaunchDeferred(remaining) => {
                if *remaining == Duration::MAX {
                    serde_json::json!({"result": names::RESULT_CRASH_LOOP_PAUSED})
                } else {
                    serde_json::json!({
                        "result": names::RESULT_RELAUNCH_DEFERRED,
                        "remainingSeconds": seconds(*remaining),
                    })
                }
            }
            DaemonLifecycleActionResult::NotInstalled => {
                serde_json::json!({"result": names::RESULT_NOT_INSTALLED})
            }
        },
        ServiceCommandOutcome::Health(health) => health_json(health),
        ServiceCommandOutcome::Status(report) => {
            let mut status = serde_json::json!({
                "status": status_wire_name(report.status),
                "label": report.label,
                "autoLaunch": report.auto_launch,
                "crashLoop": {
                    "paused": report.crash_loop.paused,
                    "consecutiveCrashes": report.crash_loop.consecutive_crashes,
                    "lastCrashAtMs": report.crash_loop.last_crash_at_ms,
                    "awaitingRestart": report.crash_loop.awaiting_restart,
                },
                "supervisorInstalled": report.supervisor_installed,
                "answers": report.answers,
            });
            // `health` and, for a deferred restart, `remainingSeconds`:
            // the keys of a `check` answer, so a surface reads both the
            // same way.
            if let (Some(status), serde_json::Value::Object(health)) =
                (status.as_object_mut(), health_json(&report.health))
            {
                status.extend(health);
            }
            status
        }
        ServiceCommandOutcome::Acknowledged => {
            serde_json::json!({"result": names::RESULT_ACKNOWLEDGED})
        }
    }
}

fn health_json(health: &DaemonHealthCheckOutcome) -> serde_json::Value {
    use constants::service as names;
    match health {
        DaemonHealthCheckOutcome::Running => {
            serde_json::json!({"health": names::HEALTH_RUNNING})
        }
        DaemonHealthCheckOutcome::NotInstalled => {
            serde_json::json!({"health": names::HEALTH_NOT_INSTALLED})
        }
        DaemonHealthCheckOutcome::StoppedExpected => {
            serde_json::json!({"health": names::HEALTH_STOPPED_EXPECTED})
        }
        DaemonHealthCheckOutcome::RestartedAfterCrash => {
            serde_json::json!({"health": names::HEALTH_RESTARTED_AFTER_CRASH})
        }
        DaemonHealthCheckOutcome::RestartDeferred(remaining) => serde_json::json!({
            "health": names::HEALTH_RESTART_DEFERRED,
            "remainingSeconds": seconds(*remaining),
        }),
        DaemonHealthCheckOutcome::CrashLoopPaused => {
            serde_json::json!({"health": names::HEALTH_CRASH_LOOP_PAUSED})
        }
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
                    "service: crash-loop pause active; run `vapor service acknowledge` \
                     to allow restarts"
                        .to_string()
                } else {
                    format!(
                        "service: start deferred {:.1}s by crash-loop backoff",
                        remaining.as_secs_f64()
                    )
                }
            }
            DaemonLifecycleActionResult::NotInstalled => {
                "service: not installed; nothing to start; `vapor service install` \
                 (or Start Vapor at login in the app) registers the background service"
                    .to_string()
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
                "service check: unexpected exit detected; daemon restarted".to_string()
            }
            DaemonHealthCheckOutcome::RestartDeferred(remaining) => format!(
                "service check: unexpected exit detected; restart deferred {:.1}s",
                remaining.as_secs_f64()
            ),
            DaemonHealthCheckOutcome::CrashLoopPaused => {
                "service check: crash-loop pause active; run `vapor service acknowledge`"
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
                    "\ncrash-loop: paused; run `vapor service acknowledge` to allow restarts",
                );
            } else if report.crash_loop.consecutive_crashes > 0 {
                line.push_str(&format!(
                    "\ncrash-loop: {} recent crash(es) in window",
                    report.crash_loop.consecutive_crashes
                ));
            }
            if report.answers == Some(false) {
                line.push_str(
                    "\ndaemon: its job runs, but it gives no answer on its socket, so nothing syncs; \
                     `vapor service restart` ends it and starts a new one",
                );
            }
            line.push_str(if report.supervisor_installed {
                "\nsupervisor: installed (it restarts a daemon that exited on its own)"
            } else {
                "\nsupervisor: not installed (`vapor service install` registers it)"
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
/// only `VAPOR_DIR` (plus `VAPOR_ENV` and `VAPOR_THROTTLE_INPUTS` when
/// set in the invoking environment); every other setting reaches the daemon through
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
    let environment = service_environment(&vapor_directory);

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

/// The environment a service definition carries: the runtime directory
/// always, and `VAPOR_ENV` and `VAPOR_THROTTLE_INPUTS` when the
/// invoking environment sets them. Everything else the daemon reads
/// from `vapor.json`. The throttle variable is what a test harness
/// pins to keep a daemon on neutral inputs; a service installed from
/// an ordinary shell never has it, so a real install samples the host.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn service_environment(vapor_directory: &std::path::Path) -> Vec<(String, String)> {
    let mut environment = vec![(
        vapor_shared::constants::env::VAPOR_DIR.to_string(),
        vapor_directory.display().to_string(),
    )];
    for key in [
        vapor_shared::constants::env::VAPOR_ENV,
        vapor_shared::constants::env::VAPOR_THROTTLE_INPUTS,
    ] {
        if let Ok(value) = std::env::var(key)
            && !value.trim().is_empty()
        {
            environment.push((key.to_string(), value));
        }
    }
    environment
}

/// Registers the supervisor job next to a daemon job that is
/// registered, and reports whether it did. With no daemon job there is
/// nothing to supervise, and a supervisor would only start and exit.
pub fn ensure_supervisor(
    daemon: &dyn ServiceInstaller,
    supervisor: &dyn ServiceInstaller,
) -> Result<bool, ServiceCommandError> {
    if daemon.status()? == ServiceStatus::NotInstalled {
        return Ok(false);
    }
    supervisor.install_and_enable()?;
    Ok(true)
}

/// `bootstrap` or `install`, whole: the daemon job, a daemon of this
/// build, and the supervisor job. Every surface that puts Vapor in the
/// background goes through here, so no install is left without the one
/// process that restarts a crashed daemon.
pub fn install_with_supervisor(
    command: ServiceCommand,
    manager: &DaemonLifecycleManager,
    installer: &dyn ServiceInstaller,
    supervisor: &dyn ServiceInstaller,
    running_build: Option<&str>,
    now: Instant,
) -> Result<ServiceCommandOutcome, ServiceCommandError> {
    let outcome = dispatch(command, manager, installer, now)?;
    // The supervisor first: if the replacement below reports an error
    // (a daemon slow to exit), the supervisor is there to finish it.
    ensure_supervisor(installer, supervisor)?;
    replace_daemon_of_another_build(manager, installer, running_build, now)?;
    Ok(outcome)
}

/// Whether an install turns lingering on. Left to itself Vapor does it
/// where no desktop session keeps the user's services up, so the
/// daemon would stop at the last logout; `choice` is the user's own
/// answer. A display that an SSH session forwards (`ssh -X`) is no
/// desktop session: it ends with the connection.
#[cfg(any(target_os = "linux", test))]
pub fn linger_wanted(choice: Option<bool>, display: bool, over_ssh: bool) -> bool {
    choice.unwrap_or(!display || over_ssh)
}

/// What became of lingering during an install.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LingerOutcome {
    On,
    /// Not wanted here; the setting stays as it was.
    Off,
    /// The system refused. Holds what it said.
    Refused(String),
}

/// Turns lingering on when [`linger_wanted`] says so. A refusal is an
/// outcome, not an error: the service is installed and runs, and the
/// caller tells the user what is left to do.
#[cfg(target_os = "linux")]
pub fn apply_linger(choice: Option<bool>) -> LingerOutcome {
    let display = vapor_platform::NativeIdleNotifier::for_current_host().has_gui_session();
    let over_ssh = std::env::var_os("SSH_CONNECTION").is_some();
    if !linger_wanted(choice, display, over_ssh) {
        return LingerOutcome::Off;
    }
    if vapor_platform::linger_enabled() == Some(true) {
        return LingerOutcome::On;
    }
    match vapor_platform::enable_linger() {
        Ok(()) => LingerOutcome::On,
        Err(error) => LingerOutcome::Refused(error.to_string()),
    }
}

/// The supervisor's service definition: this very `vapor` binary
/// running `service check --loop`, kept alive by the service manager,
/// with the same runtime directory as the daemon.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn supervisor_installer() -> Result<NativeServiceInstaller, ServiceCommandError> {
    let executable = super::daemon_binary::cli_path().ok_or_else(|| {
        ServiceCommandError::Install(ServiceInstallError::Backend(Box::new(
            std::io::Error::other("cannot tell where the running vapor binary is"),
        )))
    })?;
    let vapor_directory = vapor_shared::runtime_paths::vapor_directory();
    let logs_directory = vapor_shared::runtime_paths::logs_directory();
    let environment = service_environment(&vapor_directory);
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

/// What tells one build of a file from the next: an update replaces
/// the binary, and its size or its modification time changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BinaryStamp(Option<(u64, std::time::SystemTime)>);

impl BinaryStamp {
    pub fn of(path: &std::path::Path) -> Self {
        Self(
            std::fs::metadata(path)
                .ok()
                .and_then(|meta| Some((meta.len(), meta.modified().ok()?))),
        )
    }

    /// Whether the file is now another one. A file that cannot be read
    /// at either moment says nothing, and counts as the same.
    pub fn replaced_by(&self, now: &Self) -> bool {
        matches!((&self.0, &now.0), (Some(before), Some(after)) if before != after)
    }
}

/// The clock of the supervisor loop.
#[derive(Clone, Copy, Debug)]
pub struct SupervisorWatch {
    /// The time between two supervision ticks.
    pub interval: Duration,
    /// How often the loop asks, between two ticks, whether the daemon
    /// still answers.
    pub look: Duration,
    /// How long a daemon that answered before may give no answer
    /// before it is ended: it hangs.
    pub silence_limit: Duration,
    /// How long a daemon that has not answered yet may take. A start
    /// can be slow (a large state database to check, a volume that
    /// wakes up), and a slow start is not a hang.
    pub start_limit: Duration,
}

impl SupervisorWatch {
    /// The clock of an install, with `interval` between two ticks.
    pub fn every(interval: Duration) -> Self {
        Self {
            interval,
            look: Duration::from_millis(constants::service::SUPERVISOR_WATCH_MILLIS),
            silence_limit: Duration::from_secs(constants::service::UNRESPONSIVE_KILL_SECONDS),
            start_limit: Duration::from_secs(constants::service::UNRESPONSIVE_START_SECONDS),
        }
    }
}

/// Runs supervision ticks every `interval` until a shutdown signal
/// arrives, printing an outcome only when it differs from the previous
/// tick so the log stays quiet while the daemon is healthy. Returns
/// the last outcome.
///
/// `updated` says whether an update replaced this binary. The loop
/// then ends: the service manager keeps the job alive, so it starts
/// the new binary, and that one replaces a daemon of the old build.
///
/// `answers` says whether the daemon answers on its socket. Between
/// two ticks the loop asks it twice a second, and an answer that stops
/// runs the next tick at once: the socket closes the moment the daemon
/// exits, so a crash is seen within a second and not at the next tick.
/// A daemon whose job runs and which gives no answer for
/// `UNRESPONSIVE_KILL_SECONDS` is ended; the tick after that counts
/// the exit and starts a new one under the crash-loop guard.
pub fn check_loop(
    manager: &DaemonLifecycleManager,
    installer: &dyn ServiceInstaller,
    watch: SupervisorWatch,
    json: bool,
    stop: &std::sync::atomic::AtomicBool,
    updated: &dyn Fn() -> bool,
    answers: &dyn Fn() -> bool,
) -> Result<Option<DaemonHealthCheckOutcome>, ServiceCommandError> {
    use std::sync::atomic::Ordering;
    let SupervisorWatch {
        interval,
        look: watch,
        silence_limit,
        start_limit,
    } = watch;
    let mut last: Option<DaemonHealthCheckOutcome> = None;
    let mut silent_since: Option<Instant> = None;
    // Whether the daemon that runs now has answered, and how many
    // daemons in a row were ended before they ever did.
    let mut answered = false;
    let mut ended_before_an_answer = 0u32;
    while !stop.load(Ordering::SeqCst) {
        if updated() {
            break;
        }
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
        let answering = outcome == DaemonHealthCheckOutcome::Running && answers();
        if outcome != DaemonHealthCheckOutcome::Running {
            silent_since = None;
            answered = false;
        } else if answering {
            silent_since = None;
            answered = true;
            ended_before_an_answer = 0;
        } else {
            // The job runs and nobody answers. A daemon that answered
            // before hangs, or it stops. One that has not answered yet
            // may still start, so it gets the longer limit; and when
            // the daemon started in its place does not answer either,
            // it is left alone: a daemon whose socket cannot open still
            // syncs, and ending it again and again would stop that too.
            let since = *silent_since.get_or_insert_with(Instant::now);
            let limit = if answered { silence_limit } else { start_limit };
            if since.elapsed() >= limit && (answered || ended_before_an_answer == 0) {
                eprintln!(
                    "vapor: the daemon gave no answer for {} s; ending its process",
                    limit.as_secs()
                );
                if let Err(error) = installer.kill_daemon() {
                    eprintln!("vapor: warning: could not end the daemon: {error}");
                }
                if !answered {
                    ended_before_an_answer += 1;
                }
                silent_since = None;
                answered = false;
                last = None;
                continue;
            }
        }
        // The next tick: at the interval, at the end of a restart
        // backoff when that is sooner, and soon while a daemon that
        // should answer does not.
        let wait = match &outcome {
            DaemonHealthCheckOutcome::RestartDeferred(remaining) => {
                interval.min(*remaining + watch)
            }
            _ if silent_since.is_some() => interval.min(watch * 4),
            _ => interval,
        };
        let deadline = Instant::now() + wait;
        let mut next_look = Instant::now() + watch;
        while Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(
                Duration::from_millis(250)
                    .min(watch)
                    .min(deadline.saturating_duration_since(Instant::now())),
            );
            if answering && Instant::now() >= next_look {
                next_look = Instant::now() + watch;
                if !answers() {
                    // The daemon went away: look now.
                    break;
                }
            }
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
    fn a_running_daemon_of_another_build_is_replaced_and_the_same_build_is_left_alone() {
        let installer = fake_installer();
        installer.set_status_for_testing(ServiceStatus::Running);
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);
        let now = Instant::now();

        // The same build, and a daemon that gave no answer: nothing.
        assert!(
            !replace_daemon_of_another_build(
                &manager,
                installer.as_ref(),
                Some(&this_build()),
                now
            )
            .expect("same build")
        );
        assert!(
            !replace_daemon_of_another_build(&manager, installer.as_ref(), None, now)
                .expect("no answer")
        );
        assert!(installer.operations().is_empty());

        // The build of before an update: stop, then start.
        assert!(
            replace_daemon_of_another_build(
                &manager,
                installer.as_ref(),
                Some("vapord/0.0.1-older"),
                now
            )
            .expect("another build")
        );
        assert_eq!(installer.operations(), vec!["stop", "start"]);
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
        let watched = installer.clone();
        // Stops the loop when it has restarted the daemon. The deadline
        // only ends a run in which the restart never comes.
        let stopper = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(10);
            while !watched.operations().contains(&"start") && Instant::now() < until {
                std::thread::yield_now();
            }
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let last = check_loop(
            &manager,
            installer.as_ref(),
            SupervisorWatch::every(Duration::from_millis(20)),
            false,
            &stop,
            &|| false,
            &|| true,
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

    /// Runs the loop on its own thread until `done` says so (or ten
    /// seconds pass, which only ends a run that never gets there).
    fn run_loop_until(
        installer: &Arc<InMemoryServiceInstaller>,
        watch: SupervisorWatch,
        answers: impl Fn() -> bool + Sync,
        done: impl Fn(&InMemoryServiceInstaller) -> bool + Send + 'static,
    ) {
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);
        manager
            .start_daemon_if_allowed(Instant::now())
            .expect("the daemon is meant to run");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (flag, watched) = (stop.clone(), installer.clone());
        let stopper = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(10);
            while !done(&watched) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(2));
            }
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        check_loop(
            &manager,
            installer.as_ref(),
            watch,
            false,
            &stop,
            &|| false,
            &answers,
        )
        .expect("loop");
        stopper.join().expect("stopper");
    }

    #[test]
    fn the_check_loop_sees_an_exit_between_two_ticks_and_starts_the_daemon_again() {
        let installer = fake_installer();
        installer.install_and_enable().expect("install");
        // A minute between two ticks: only the watch of the socket can
        // see the exit in the time this test runs.
        let watch = SupervisorWatch {
            interval: Duration::from_secs(60),
            look: Duration::from_millis(5),
            silence_limit: Duration::from_secs(60),
            start_limit: Duration::from_secs(60),
        };
        let crashed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (crash, daemon) = (crashed.clone(), installer.clone());
        let crasher = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            daemon.set_status_for_testing(ServiceStatus::Stopped);
            crash.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let started = |installer: &InMemoryServiceInstaller| {
            installer
                .operations()
                .iter()
                .filter(|op| **op == "start")
                .count()
                >= 2
        };
        run_loop_until(
            &installer,
            watch,
            || !crashed.load(std::sync::atomic::Ordering::SeqCst),
            started,
        );
        crasher.join().expect("crasher");
        assert!(
            started(&installer),
            "the daemon is started again within the tick it crashed in: {:?}",
            installer.operations()
        );
    }

    #[test]
    fn the_check_loop_ends_a_daemon_that_answered_and_then_stops_answering() {
        let installer = fake_installer();
        installer.install_and_enable().expect("install");
        // A long limit for a start: only the limit for a daemon that
        // answered before can end this one in the time of the test.
        let watch = SupervisorWatch {
            interval: Duration::from_millis(20),
            look: Duration::from_millis(5),
            silence_limit: Duration::from_millis(60),
            start_limit: Duration::from_secs(600),
        };
        let replaced = |installer: &InMemoryServiceInstaller| {
            let operations = installer.operations();
            let killed = operations.iter().position(|op| *op == "kill");
            killed.is_some_and(|at| operations[at..].contains(&"start"))
        };
        let looks = std::sync::atomic::AtomicUsize::new(0);
        let answers_at_first = move || looks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 3;
        run_loop_until(&installer, watch, answers_at_first, replaced);
        assert!(
            replaced(&installer),
            "a daemon that hangs is ended and a new one started: {:?}",
            installer.operations()
        );
    }

    #[test]
    fn a_start_whose_daemon_gives_no_answer_ends_with_an_error() {
        use DaemonLifecycleActionResult::{Started, Stopped, Unchanged};
        let silent = |wait: Duration| {
            assert_eq!(wait, START_ANSWER_WAIT);
            false
        };
        for command in [ServiceCommand::Start, ServiceCommand::Restart] {
            // `Unchanged`: the job ran already, and a daemon that hangs
            // keeps it.
            for result in [Started, Unchanged] {
                let error = confirm_started(command, &result, &silent)
                    .expect_err("a daemon that gives no answer did not start");
                assert!(error.contains("gives no answer"), "{error}");
                assert!(error.contains("vapor service restart"), "{error}");
                assert_eq!(confirm_started(command, &result, &|_| true), Ok(()));
            }
        }
        // Nothing was started: nobody is asked, and nothing is wrong.
        let never_asked = |_: Duration| -> bool { panic!("no daemon to wait for") };
        assert_eq!(
            confirm_started(ServiceCommand::Stop, &Stopped, &never_asked),
            Ok(())
        );
        assert_eq!(
            confirm_started(
                ServiceCommand::Start,
                &DaemonLifecycleActionResult::RelaunchDeferred(Duration::from_secs(2)),
                &never_asked
            ),
            Ok(())
        );
    }

    #[test]
    fn a_daemon_that_never_answers_is_ended_one_time_and_then_left_alone() {
        let installer = fake_installer();
        installer.install_and_enable().expect("install");
        let watch = SupervisorWatch {
            interval: Duration::from_millis(10),
            look: Duration::from_millis(2),
            silence_limit: Duration::from_millis(20),
            start_limit: Duration::from_millis(40),
        };
        // The loop runs for many start limits after the first kill:
        // long enough to end the second daemon too, if it did that.
        let looks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = looks.clone();
        let never = move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            false
        };
        let (seen, at_the_kill) = (
            looks.clone(),
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        let long_after_the_kill = move |installer: &InMemoryServiceInstaller| {
            if !installer.operations().contains(&"kill") {
                return false;
            }
            let now = seen.load(std::sync::atomic::Ordering::SeqCst);
            let _ = at_the_kill.compare_exchange(
                0,
                now.max(1),
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            );
            now >= at_the_kill.load(std::sync::atomic::Ordering::SeqCst) + 60
        };
        run_loop_until(&installer, watch, never, long_after_the_kill);
        let kills = installer
            .operations()
            .iter()
            .filter(|operation| **operation == "kill")
            .count();
        assert_eq!(
            kills,
            1,
            "a slow start gets one new try, not a kill at every limit: {:?}",
            installer.operations()
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
            SupervisorWatch::every(Duration::from_secs(60)),
            false,
            &stop,
            &|| false,
            &|| true,
        )
        .expect("loop");
        assert_eq!(last, Some(DaemonHealthCheckOutcome::NotInstalled));
    }

    #[test]
    fn the_check_loop_ends_when_an_update_replaced_the_binary() {
        let installer = fake_installer();
        installer.set_status_for_testing(ServiceStatus::Running);
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);
        let stop = std::sync::atomic::AtomicBool::new(false);
        // The first tick runs, the second look finds another binary.
        let looks = std::sync::atomic::AtomicUsize::new(0);
        let last = check_loop(
            &manager,
            installer.as_ref(),
            SupervisorWatch::every(Duration::from_millis(1)),
            false,
            &stop,
            &|| looks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 1,
            &|| true,
        )
        .expect("loop");
        assert_eq!(last, Some(DaemonHealthCheckOutcome::Running));
        assert_eq!(looks.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn a_binary_stamp_changes_when_the_file_is_replaced_and_not_when_it_is_gone() {
        let temp = tempfile::TempDir::new().expect("temp");
        let binary = temp.path().join("vapor");
        std::fs::write(&binary, b"one").expect("write");
        let before = BinaryStamp::of(&binary);
        assert!(!before.replaced_by(&BinaryStamp::of(&binary)));

        std::fs::write(&binary, b"another build").expect("write");
        assert!(before.replaced_by(&BinaryStamp::of(&binary)));

        std::fs::remove_file(&binary).expect("remove");
        assert!(
            !before.replaced_by(&BinaryStamp::of(&binary)),
            "a file that is gone is not a new build to hand over to"
        );
    }

    #[test]
    fn lingering_is_the_default_only_where_no_desktop_keeps_a_session() {
        assert!(linger_wanted(None, false, true), "a server: on by default");
        assert!(
            linger_wanted(None, false, false),
            "a console with no display"
        );
        assert!(!linger_wanted(None, true, false), "a desktop: not needed");
        assert!(
            linger_wanted(None, true, true),
            "a display forwarded over SSH ends with the connection"
        );
        assert!(
            !linger_wanted(Some(false), false, true),
            "the user said off"
        );
        assert!(linger_wanted(Some(true), true, false), "the user said on");
    }

    #[test]
    fn an_install_registers_the_supervisor_and_replaces_a_daemon_of_another_build() {
        let installer = fake_installer();
        let supervisor = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(None));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);
        install_with_supervisor(
            ServiceCommand::Install,
            &manager,
            installer.as_ref(),
            supervisor.as_ref(),
            Some("vapord/0.0.0-older"),
            Instant::now(),
        )
        .expect("install");
        assert_eq!(
            installer.operations(),
            vec!["install", "start", "stop", "start"]
        );
        assert_eq!(supervisor.operations(), vec!["install"]);

        // Auto-launch off: a bootstrap installs nothing, the supervisor
        // included.
        let installer = fake_installer();
        let supervisor = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(false)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);
        install_with_supervisor(
            ServiceCommand::Bootstrap,
            &manager,
            installer.as_ref(),
            supervisor.as_ref(),
            None,
            Instant::now(),
        )
        .expect("bootstrap");
        assert!(installer.operations().is_empty());
        assert!(supervisor.operations().is_empty());
    }

    #[test]
    fn the_supervisor_is_registered_only_next_to_a_registered_daemon() {
        let daemon = fake_installer();
        let supervisor = fake_installer();
        assert!(!ensure_supervisor(daemon.as_ref(), supervisor.as_ref()).expect("ensure"));
        assert!(supervisor.operations().is_empty());

        daemon.install_and_enable().expect("install");
        assert!(ensure_supervisor(daemon.as_ref(), supervisor.as_ref()).expect("ensure"));
        assert_eq!(supervisor.operations(), vec!["install"]);
    }

    #[test]
    fn status_carries_the_health_a_check_would_find_and_changes_nothing() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);
        dispatch(
            ServiceCommand::Install,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("install");
        // The daemon exits on its own. A status read reports the exit
        // and leaves the restart to the supervisor.
        installer.set_status_for_testing(ServiceStatus::Stopped);
        let before = installer.operations();
        let outcome = dispatch(
            ServiceCommand::Status,
            &manager,
            installer.as_ref(),
            Instant::now(),
        )
        .expect("status");
        let ServiceCommandOutcome::Status(report) = &outcome else {
            panic!("status yields a report: {outcome:?}");
        };
        assert_eq!(
            report.health,
            DaemonHealthCheckOutcome::RestartDeferred(Duration::ZERO)
        );
        assert_eq!(report.crash_loop.consecutive_crashes, 0);
        assert_eq!(installer.operations(), before);
        let json = render_json(&outcome);
        assert_eq!(json["health"], "restart-deferred");
        assert_eq!(json["remainingSeconds"], 0.0);
    }

    #[test]
    fn start_invokes_installer_start_when_no_crash_loop_pending() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(true)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);
        installer.set_status_for_testing(ServiceStatus::Stopped);

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
    fn start_and_restart_report_not_installed_on_a_bare_host() {
        let installer = fake_installer();
        let settings = Arc::new(InMemoryAutoLaunchSettingStore::seeded(Some(false)));
        let manager = DaemonLifecycleManager::new(installer.clone(), settings);

        for command in [ServiceCommand::Start, ServiceCommand::Restart] {
            let outcome =
                dispatch(command, &manager, installer.as_ref(), Instant::now()).expect("dispatch");
            assert_eq!(
                outcome,
                ServiceCommandOutcome::Action(DaemonLifecycleActionResult::NotInstalled)
            );
        }
        // Neither half of a restart may touch a service manager that
        // has no job to act on.
        assert!(installer.operations().is_empty());
        assert_eq!(
            render_json(&ServiceCommandOutcome::Action(
                DaemonLifecycleActionResult::NotInstalled
            ))
            .to_string(),
            r#"{"result":"not-installed"}"#
        );
        assert!(
            render_text(&ServiceCommandOutcome::Action(
                DaemonLifecycleActionResult::NotInstalled
            ))
            .contains("vapor service install")
        );
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
        installer.set_status_for_testing(ServiceStatus::Running);

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
        // The label field holds the reverse-DNS service identifier,
        // never the log file name that sits next to it in the constants.
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
        installer.set_status_for_testing(ServiceStatus::Stopped);
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

    // --- JSON contract (the macOS app shim parses these shapes) ---

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
            r#"{"remainingSeconds":1.5,"result":"relaunch-deferred"}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Action(
                DaemonLifecycleActionResult::RelaunchDeferred(Duration::MAX)
            )),
            r#"{"result":"crash-loop-paused"}"#
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
            r#"{"health":"not-installed"}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Health(
                DaemonHealthCheckOutcome::StoppedExpected
            )),
            r#"{"health":"stopped-expected"}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Health(
                DaemonHealthCheckOutcome::RestartedAfterCrash
            )),
            r#"{"health":"restarted-after-crash"}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Health(
                DaemonHealthCheckOutcome::RestartDeferred(Duration::from_secs(2))
            )),
            r#"{"health":"restart-deferred","remainingSeconds":2.0}"#
        );
        assert_eq!(
            json_string(&ServiceCommandOutcome::Health(
                DaemonHealthCheckOutcome::CrashLoopPaused
            )),
            r#"{"health":"crash-loop-paused"}"#
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
            health: DaemonHealthCheckOutcome::CrashLoopPaused,
            supervisor_installed: true,
            // The job does not run: there is no daemon to ask.
            answers: None,
        };
        assert_eq!(
            json_string(&ServiceCommandOutcome::Status(report)),
            concat!(
                r#"{"answers":null,"autoLaunch":true,"crashLoop":{"awaitingRestart":true,"#,
                r#""consecutiveCrashes":5,"lastCrashAtMs":1750000000000,"paused":true},"#,
                r#""health":"crash-loop-paused","label":"sh.arn.vapor.daemon","#,
                r#""status":"crash-loop-paused","supervisorInstalled":true}"#
            )
        );
    }

    #[test]
    fn status_says_when_the_job_runs_and_the_daemon_gives_no_answer() {
        let report = ServiceStatusReport {
            status: ServiceStatus::Running,
            label: constants::service::DAEMON_LABEL.to_string(),
            auto_launch: true,
            crash_loop: CrashLoopStateSnapshot {
                paused: false,
                consecutive_crashes: 0,
                last_crash_at_ms: None,
                awaiting_restart: false,
            },
            health: DaemonHealthCheckOutcome::Running,
            supervisor_installed: true,
            answers: Some(false),
        };
        let outcome = ServiceCommandOutcome::Status(report);
        let json: serde_json::Value = serde_json::from_str(&json_string(&outcome)).expect("json");
        assert_eq!(json["answers"], serde_json::json!(false));
        assert_eq!(json["status"], "running");
        let text = render_text(&outcome);
        assert!(
            text.contains("gives no answer on its socket")
                && text.contains("vapor service restart"),
            "{text}"
        );
    }
}
