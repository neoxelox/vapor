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
use vapor_providers::default_provider;
use vapor_shared::config::{self, VaporConfigLoadResult};

use crate::clock::system_clock;
use crate::ipc_server;
use crate::ipc_service::{DaemonIpcService, DaemonStatusSnapshot};
use crate::logging;
use crate::metrics::NativePlatformMetricsSampler;
use crate::path_filter::EventPathFilterOptions;
use crate::runtime::{self, DaemonRuntime, DaemonRuntimeError};
use crate::runtime_control::RuntimeControl;
use crate::singleton::{SingletonLock, SingletonLockError};
use crate::state_db::{DurableStateDb, StateDbError};
use crate::sync_directories;

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

    // Persisted configuration reaches the runtime here; environment
    // variables stay as per-field overrides (docs: README Configuration).
    let VaporConfigLoadResult { config, load_issue } = config::load_default();
    if let Some(issue) = load_issue {
        logging::error(
            "Preserved unreadable vapor configuration; continuing with defaults",
            &[("issue", issue)],
        );
    }

    let state_db = DurableStateDb::open_default()?;
    let sync_scope = sync_directories::resolve_with_config(&config);
    let filter_options = EventPathFilterOptions::from_environment_and_config(&config);
    // The native platform sampler (per-OS FFI lands incrementally; it
    // currently reports static idle inputs) — wired here so the seam is
    // exercised in production, not just in tests.
    let metrics_sampler = Arc::new(NativePlatformMetricsSampler::for_current_host());

    let mut runtime = DaemonRuntime::start_configured(
        sync_scope,
        filter_options,
        state_db,
        default_provider(),
        metrics_sampler,
        system_clock(),
    )?;

    log_started(&runtime);

    // Spawn the IPC server so other Vapor surfaces (`vapor status`, the
    // macOS app) can query and control the running daemon. The handle is
    // kept alive for the duration of the runtime loop; its Drop removes
    // the socket file.
    let initial_snapshot = DaemonStatusSnapshot::from_app(runtime.app());
    let runtime_control = Arc::new(RuntimeControl::new());
    runtime.attach_control(runtime_control.clone());
    let ipc_service = Arc::new(DaemonIpcService::new(initial_snapshot, runtime_control));
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
    // (Wave 12 / C6-7) returns `Unsupported` until its bridge lands.
    let supervisor = NativeProcessSupervisor::new();
    if let Err(error) = supervisor.register_shutdown_handler(runtime::request_shutdown) {
        logging::warning(
            "Failed to register process supervisor shutdown handler",
            &[("error", error.to_string())],
        );
    }
}

fn log_started(runtime: &DaemonRuntime) {
    let local_sync_directory = runtime
        .sync_scope()
        .local_sync_directory
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "none".to_string());
    logging::info(
        "vapord started",
        &[
            ("version", crate::build_info::VERSION.to_string()),
            (
                "git_commit",
                crate::build_info::GIT_COMMIT_SHORT.to_string(),
            ),
            ("provider", runtime.app().provider_name().to_string()),
            (
                "run_state",
                format!("{:?}", runtime.app().snapshot().run_state),
            ),
            (
                "throttle_state",
                format!("{:?}", runtime.app().snapshot().throttle_state),
            ),
            ("local_sync_directory", local_sync_directory),
            (
                "cloud_sync_directory",
                runtime.sync_scope().cloud_sync_directory.clone(),
            ),
            ("watcher_active", runtime.has_live_watcher().to_string()),
        ],
    );
}
