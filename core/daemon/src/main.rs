use vapor_daemon::{
    build_info, logging, runtime, runtime::DaemonRuntime, state_db::DurableStateDb,
    sync_directories,
};
use vapor_providers::default_provider;

extern "C" fn handle_shutdown_signal(_signal: libc::c_int) {
    runtime::request_shutdown();
}

fn install_shutdown_signal_handlers() {
    let handler: libc::sighandler_t = handle_shutdown_signal as *const () as libc::sighandler_t;
    // SAFETY: signal(3) is async-signal-safe; handle_shutdown_signal only performs
    // an atomic store. Installed exactly once at process start.
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
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
    if let Err(error) = runtime.run_forever() {
        logging::error(
            "Daemon runtime loop exited unexpectedly",
            &[("error", format!("{:?}", error))],
        );
        std::process::exit(1);
    }
}
