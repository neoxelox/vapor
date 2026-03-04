use vapor_daemon::DaemonApp;

fn main() {
    let app = DaemonApp::default();
    println!(
        "vapor-daemon started (provider={}, state={:?}, throttle={:?})",
        app.provider_name(),
        app.snapshot().run_state,
        app.snapshot().throttle_state
    );
}
