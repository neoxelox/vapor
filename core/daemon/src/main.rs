use vapor_daemon::{DaemonApp, build_info, logging, sync_directories};

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
            ("version", build_info::VERSION.to_string()),
            ("git_commit", build_info::GIT_COMMIT_SHORT.to_string()),
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
