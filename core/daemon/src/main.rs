use vapor_daemon::{DaemonApp, logging};

fn main() {
    let app = DaemonApp::default();
    logging::info(
        "vapord started",
        &[
            ("provider", app.provider_name().to_string()),
            ("run_state", format!("{:?}", app.snapshot().run_state)),
            (
                "throttle_state",
                format!("{:?}", app.snapshot().throttle_state),
            ),
        ],
    );
}
