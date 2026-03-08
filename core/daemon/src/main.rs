use vapor_daemon::{DaemonApp, logging, sync_directories};

fn main() {
    let app = DaemonApp::default();
    let sync_scope = sync_directories::resolve_from_process_environment();
    app.ensure_cloud_sync_directory(sync_scope.cloud_sync_directory.as_str());
    let local_sync_directory = sync_scope
        .local_sync_directory
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "none".to_string());

    logging::info(
        "vapord started",
        &[
            ("provider", app.provider_name().to_string()),
            ("run_state", format!("{:?}", app.snapshot().run_state)),
            (
                "throttle_state",
                format!("{:?}", app.snapshot().throttle_state),
            ),
            ("local_sync_directory", local_sync_directory),
            ("cloud_sync_directory", sync_scope.cloud_sync_directory),
        ],
    );
}
