use std::sync::Arc;

use vapor_daemon::{
    build_info, ipc_server,
    ipc_service::{DaemonIpcService, DaemonStatusSnapshot},
    logging, runtime,
    runtime::DaemonRuntime,
    runtime_control::RuntimeControl,
    state_db::DurableStateDb,
    sync_directories,
};
use vapor_ipc::Service;
use vapor_platform::{NativeProcessSupervisor, ProcessSupervisor};
use vapor_providers::default_provider;

fn install_shutdown_signal_handlers() {
    // Wave 4 routes shutdown signals through `core/platform`'s
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

fn main() {
    if let Some(flag) = std::env::args().nth(1)
        && (flag == "--version" || flag == "-V")
    {
        println!(
            "vapord {} ({})",
            build_info::VERSION,
            build_info::GIT_COMMIT_SHORT
        );
        return;
    }

    install_shutdown_signal_handlers();

    let state_db = match DurableStateDb::open_default() {
        Ok(state_db) => state_db,
        Err(error) => {
            logging::error(
                "Failed to initialize durable queue/state DB",
                &[("error", error.to_string())],
            );
            std::process::exit(1);
        }
    };
    let sync_scope = sync_directories::resolve_from_process_environment();
    let mut runtime = match DaemonRuntime::start(sync_scope, state_db, default_provider()) {
        Ok(runtime) => runtime,
        Err(error) => {
            logging::error(
                "Failed to compose daemon runtime",
                &[("error", format!("{:?}", error))],
            );
            std::process::exit(1);
        }
    };
    let local_sync_directory = runtime
        .sync_scope()
        .local_sync_directory
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "none".to_string());
    let cloud_sync_directory = runtime.sync_scope().cloud_sync_directory.clone();
    let provider_name = runtime.app().provider_name().to_string();
    let run_state = format!("{:?}", runtime.app().snapshot().run_state);
    let throttle_state = format!("{:?}", runtime.app().snapshot().throttle_state);
    let watcher_active = runtime.has_live_watcher().to_string();

    logging::info(
        "vapord started",
        &[
            ("version", build_info::VERSION.to_string()),
            ("git_commit", build_info::GIT_COMMIT_SHORT.to_string()),
            ("provider", provider_name),
            ("run_state", run_state),
            ("throttle_state", throttle_state),
            ("local_sync_directory", local_sync_directory),
            ("cloud_sync_directory", cloud_sync_directory),
            ("watcher_active", watcher_active),
        ],
    );

    // Wave 6 phase 2: spawn the IPC server so other Vapor surfaces
    // (`vapor status`, future macOS app diagnostics) can query the
    // running daemon. Wave 7 wires the IPC `pause` / `resume` /
    // `flush-now` / `reconcile` requests through `RuntimeControl`,
    // which the runtime tick observes between iterations. The handle
    // is kept alive for the duration of the runtime loop; its Drop
    // removes the socket file.
    let initial_snapshot = DaemonStatusSnapshot::from_app(runtime.app());
    let runtime_control = Arc::new(RuntimeControl::new());
    runtime.attach_control(runtime_control.clone());
    let ipc_service = Arc::new(DaemonIpcService::new(
        initial_snapshot,
        runtime_control.clone(),
    ));
    let ipc_handle = match ipc_server::spawn(ipc_service.clone() as Arc<dyn Service>) {
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

    if let Err(error) = runtime.run_forever() {
        logging::error(
            "Daemon runtime loop exited unexpectedly",
            &[("error", format!("{:?}", error))],
        );
        std::process::exit(1);
    }

    // Drop the IPC handle explicitly so the socket file is removed on
    // clean shutdown even if drop order is later modified.
    drop(ipc_handle);
}
