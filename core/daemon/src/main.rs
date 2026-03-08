use vapor_daemon::{DaemonApp, logging, sync_directories};

fn main() {
    let app = DaemonApp::default();
    let sync_directories = sync_directories::resolve_from_process_environment();

    logging::info(
        "vapord started",
        &[
            ("provider", app.provider_name().to_string()),
            ("run_state", format!("{:?}", app.snapshot().run_state)),
            (
                "throttle_state",
                format!("{:?}", app.snapshot().throttle_state),
            ),
            ("sync_directory_count", sync_directories.len().to_string()),
        ],
    );
}
