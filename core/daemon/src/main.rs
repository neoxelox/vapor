use std::time::SystemTime;

use vapor_daemon::{DaemonApp, build_info, logging, state_db::DurableStateDb, sync_directories};

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

    let mut app = DaemonApp::default();
    let mut state_db = match DurableStateDb::open_default() {
        Ok(state_db) => state_db,
        Err(error) => {
            logging::error(
                "Failed to initialize durable queue/state DB",
                &[("error", error.to_string())],
            );
            std::process::exit(1);
        }
    };
    let recovered_count = match state_db.recover_leased(SystemTime::now()) {
        Ok(recovered_count) => recovered_count,
        Err(error) => {
            logging::error(
                "Failed to recover leased durable intents",
                &[
                    ("database_path", state_db.path().display().to_string()),
                    ("error", error.to_string()),
                ],
            );
            std::process::exit(1);
        }
    };
    logging::info(
        "Durable queue/state DB is ready",
        &[
            ("database_path", state_db.path().display().to_string()),
            ("recovered_leased_intents", recovered_count.to_string()),
        ],
    );
    match app.restore_retry_slowdown(&mut state_db, SystemTime::now()) {
        Ok(Some(slowdown_until)) => logging::warning(
            "Restored retry slowdown window from durable state",
            &[("retry_slowdown_until", format!("{:?}", slowdown_until))],
        ),
        Ok(None) => {}
        Err(error) => {
            logging::error(
                "Failed to restore retry slowdown state",
                &[
                    ("database_path", state_db.path().display().to_string()),
                    ("error", error.to_string()),
                ],
            );
            std::process::exit(1);
        }
    }
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
