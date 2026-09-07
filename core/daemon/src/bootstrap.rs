//! Full daemon-process composition, shared by the `vapord` binary and
//! `vapor run`.
//!
//! One place owns the startup order so the two entry points can never
//! drift: shutdown signal handlers → singleton lock → configuration →
//! durable state DB → runtime (with the platform metrics sampler) →
//! IPC control channel → tick loop. The returned error type is coarse
//! by design — callers log it and exit non-zero.

use std::error::Error;
use std::fmt::{self, Display};
use std::sync::Arc;

use vapor_ipc::Service;
use vapor_platform::{NativeProcessSupervisor, ProcessSupervisor};
use vapor_shared::config::{self, VaporConfigLoadResult};

use crate::clock::system_clock;
use crate::ipc_server;
use crate::ipc_service::{DaemonIpcService, DaemonStatusSnapshot};
use crate::logging;
use crate::metrics::NativePlatformMetricsSampler;
use crate::multi_runtime::MultiProfileRuntime;
use crate::path_filter::EventPathFilterOptions;
use crate::runtime::{self, DaemonRuntimeError};
use crate::runtime_control::RuntimeControl;
use crate::singleton::{SingletonLock, SingletonLockError};
use crate::state_db::StateDbError;

#[derive(Debug)]
pub enum BootstrapError {
    /// Another daemon already serves this `vapor_dir`.
    AlreadyRunning(SingletonLockError),
    Lock(SingletonLockError),
    StateDb(StateDbError),
    Runtime(DaemonRuntimeError),
}

impl Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning(error) => {
                write!(f, "daemon already running: {error}")
            }
            Self::Lock(error) => write!(f, "failed to acquire daemon lock: {error}"),
            Self::StateDb(error) => write!(f, "failed to open durable state DB: {error}"),
            Self::Runtime(error) => write!(f, "daemon runtime error: {error:?}"),
        }
    }
}

impl Error for BootstrapError {}

impl From<StateDbError> for BootstrapError {
    fn from(error: StateDbError) -> Self {
        Self::StateDb(error)
    }
}

impl From<DaemonRuntimeError> for BootstrapError {
    fn from(error: DaemonRuntimeError) -> Self {
        Self::Runtime(error)
    }
}

/// Runs a complete daemon process in the current thread until a shutdown
/// signal arrives (or the runtime fails terminally).
pub fn run_daemon() -> Result<(), BootstrapError> {
    install_shutdown_signal_handlers();

    // Exactly one daemon per vapor_dir: a second instance would race the
    // first on the durable queue and steal its IPC socket.
    let _singleton_lock = match SingletonLock::acquire_for_current_vapor_dir() {
        Ok(lock) => lock,
        Err(error @ SingletonLockError::AlreadyRunning(_)) => {
            return Err(BootstrapError::AlreadyRunning(error));
        }
        Err(error) => return Err(BootstrapError::Lock(error)),
    };

    // Bound the service-manager stdout/stderr redirect files across
    // restarts (the structured log rotates itself as it grows).
    let logs_directory = vapor_shared::runtime_paths::logs_directory();
    for redirect in [
        vapor_shared::constants::runtime::DAEMON_STDOUT_LOG_FILE_NAME,
        vapor_shared::constants::runtime::DAEMON_STDERR_LOG_FILE_NAME,
    ] {
        vapor_shared::logging::trim_redirect_log_if_oversized(&logs_directory.join(redirect));
    }

    // Persisted configuration reaches the runtime here; environment
    // variables stay as per-field overrides (docs: README Configuration).
    let VaporConfigLoadResult { config, load_issue } = config::load_default();
    if let Some(issue) = load_issue {
        logging::error(
            "Preserved unreadable vapor configuration; continuing with defaults",
            &[("issue", issue)],
        );
    }

    // Stable device identifier: persisted at first run, never
    // silently regenerated. A write failure degrades to an ephemeral id
    // for this run rather than blocking the daemon.
    let config_path = vapor_shared::runtime_paths::vapor_directory()
        .join(vapor_shared::constants::runtime::CONFIGURATION_FILE_NAME);
    let device_id = match vapor_shared::device_id::resolve_or_persist(&config_path) {
        Ok(device_id) => device_id,
        Err(error) => {
            logging::warning(
                "Could not persist device id; using an ephemeral one for this run",
                &[("error", error.to_string())],
            );
            vapor_shared::device_id::derive_device_id()
        }
    };

    // One runtime per enabled profile over a shared workgate and
    // deduplicated watchers. A configuration without a
    // `profiles` array runs the single implicit `default` profile on
    // the legacy state paths.
    let profiles = crate::profiles::resolve_profiles(&config);
    let filter_options = EventPathFilterOptions::from_environment_and_config(&config);
    let inputs_source = throttle_inputs_source();
    let static_inputs = matches!(inputs_source, ThrottleInputsSource::Static);
    let metrics_sampler: Arc<dyn crate::metrics::MetricsSampler> = if static_inputs {
        logging::info(
            "Throttle inputs are static by request: neutral defaults and zero idle time",
            &[(
                "env",
                vapor_shared::constants::env::VAPOR_THROTTLE_INPUTS.to_string(),
            )],
        );
        Arc::new(crate::metrics::StaticMetricsSampler::default())
    } else if let ThrottleInputsSource::File(path) = &inputs_source {
        logging::info(
            "Throttle inputs are read from a file on every sample",
            &[("path", path.display().to_string())],
        );
        Arc::new(crate::metrics::FileMetricsSampler::new(path.clone()))
    } else if NativePlatformMetricsSampler::has_native_sampling() {
        logging::info(
            "Throttle inputs come from the host",
            &[(
                "inputs",
                NativePlatformMetricsSampler::input_sources().to_string(),
            )],
        );
        Arc::new(NativePlatformMetricsSampler::for_current_host())
    } else {
        logging::warning(
            "Throttle inputs are static placeholders on this OS: battery, thermal and CPU \
             pressure will not throttle the daemon, and idle boost stays off",
            &[],
        );
        Arc::new(NativePlatformMetricsSampler::for_current_host())
    };

    let budget_config = crate::resource_budget::EffectiveBudgetConfig::resolve(&config);
    let mass_delete_settings = crate::safeguards::MassDeleteGuardSettings::resolve(&config);
    let mut runtime = MultiProfileRuntime::start(
        profiles,
        filter_options,
        metrics_sampler,
        system_clock(),
        &device_id,
        true,
        budget_config,
        mass_delete_settings,
    )?;
    // Production daemons run provider I/O on worker threads so network
    // RTT never stalls the tick loop; tests keep the inline mode for
    // deterministic single-threaded ticks.
    runtime.enable_transfer_workers();
    runtime.watch_config(&config_path, config.clone());
    match &inputs_source {
        ThrottleInputsSource::Static => {
            runtime.set_idle_notifier(Arc::new(vapor_platform::ManualIdleNotifier::new(
                std::time::Duration::ZERO,
            )));
        }
        ThrottleInputsSource::File(path) => {
            runtime.set_idle_notifier(Arc::new(crate::metrics::FileIdleNotifier::new(
                path.clone(),
            )));
        }
        ThrottleInputsSource::Host => {}
    }

    log_started(&runtime);

    // Spawn the IPC server so other Vapor surfaces (`vapor status`, the
    // macOS app) can query and control the running daemon. The handle is
    // kept alive for the duration of the runtime loop; its Drop removes
    // the socket file.
    let initial_snapshot = DaemonStatusSnapshot {
        run_state: "Starting".to_string(),
        throttle_state: vapor_shared::ThrottleState::Light,
        provider_name: config.provider.clone(),
        throttle_reason: "starting up".to_string(),
        ..DaemonStatusSnapshot::default()
    };
    let runtime_control = Arc::new(RuntimeControl::new());
    runtime.attach_control(runtime_control.clone());
    runtime.set_timeline_limit(config.timeline_limit);
    runtime.configure_trash(crate::trash::TrashSettings::resolve(&config));
    let ipc_service = Arc::new(DaemonIpcService::with_timeline(
        initial_snapshot,
        runtime_control,
        runtime.timeline(),
    ));
    runtime.attach_status_publisher(ipc_service.clone());
    let ipc_handle = match ipc_server::spawn(ipc_service as Arc<dyn Service>) {
        Ok(handle) => Some(handle),
        Err(error) => {
            logging::warning(
                "Failed to start IPC server; daemon will run without status endpoint",
                &[("error", error.to_string())],
            );
            None
        }
    };
    if let Some(handle) = &ipc_handle {
        logging::info(
            "Started IPC server",
            &[("socket_path", handle.socket_path().display().to_string())],
        );
    }

    let result = runtime.run_forever();

    // Drop the IPC handle explicitly so the socket file is removed on
    // clean shutdown even if drop order is later modified.
    drop(ipc_handle);

    result.map_err(BootstrapError::from)
}

fn install_shutdown_signal_handlers() {
    // Shutdown signals route through `core/platform`'s
    // `ProcessSupervisor`. The macOS / Linux native impl uses
    // `signal-hook` to translate `SIGTERM` / `SIGINT` into a flag-flip
    // on `runtime::SHUTDOWN_REQUESTED`; the Windows native impl
    // returns `Unsupported` until its bridge lands.
    let supervisor = NativeProcessSupervisor::new();
    if let Err(error) = supervisor.register_shutdown_handler(runtime::request_shutdown) {
        logging::warning(
            "Failed to register process supervisor shutdown handler",
            &[("error", error.to_string())],
        );
    }
}

fn log_started(runtime: &MultiProfileRuntime) {
    let profile_summary = runtime
        .profile_summaries()
        .iter()
        .map(|profile| {
            format!(
                "{}({}, {})",
                profile.id,
                profile.provider_kind,
                profile.sync_mode.as_config_value()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    logging::info(
        "vapord started",
        &[
            ("version", crate::build_info::VERSION.to_string()),
            (
                "git_commit",
                crate::build_info::GIT_COMMIT_SHORT.to_string(),
            ),
            ("profile_count", runtime.profile_count().to_string()),
            ("profiles", profile_summary),
        ],
    );
}

enum ThrottleInputsSource {
    Host,
    Static,
    File(std::path::PathBuf),
}

/// `VAPOR_THROTTLE_INPUTS=static` pins the throttle to neutral inputs;
/// `file:<path>` re-reads a JSON document every sample. Any other value
/// (or none) reads the host. An unknown value is logged and treated as
/// `host` so a typo never silently disables sampling.
fn throttle_inputs_source() -> ThrottleInputsSource {
    use vapor_shared::constants::{engine, env};
    let Ok(value) = std::env::var(env::VAPOR_THROTTLE_INPUTS) else {
        return ThrottleInputsSource::Host;
    };
    let trimmed = value.trim();
    if trimmed == engine::THROTTLE_INPUTS_STATIC {
        return ThrottleInputsSource::Static;
    }
    if trimmed.is_empty() || trimmed == engine::THROTTLE_INPUTS_HOST {
        return ThrottleInputsSource::Host;
    }
    if let Some(path) = trimmed.strip_prefix(engine::THROTTLE_INPUTS_FILE_PREFIX)
        && !path.is_empty()
    {
        return ThrottleInputsSource::File(std::path::PathBuf::from(path));
    }
    match Ok::<String, ()>(value) {
        Ok(value) => {
            logging::warning(
                "Unknown throttle-input source; reading the host instead",
                &[
                    ("env", env::VAPOR_THROTTLE_INPUTS.to_string()),
                    ("value", value),
                ],
            );
            ThrottleInputsSource::Host
        }
        Err(()) => ThrottleInputsSource::Host,
    }
}
