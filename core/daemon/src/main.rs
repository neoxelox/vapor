use vapor_daemon::{
    bootstrap::{self, BootstrapError},
    build_info, logging,
};

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

    // The full composition (singleton lock → config → state DB → runtime
    // → IPC → tick loop) lives in `bootstrap::run_daemon` and is shared
    // with `vapor run`, so the two entry points cannot drift.
    match bootstrap::run_daemon() {
        Ok(()) => {}
        Err(error @ BootstrapError::AlreadyRunning(_)) => {
            logging::error(
                "Refusing to start: another daemon already serves this vapor directory",
                &[("error", error.to_string())],
            );
            eprintln!("vapord: {error}");
            std::process::exit(1);
        }
        Err(error) => {
            logging::error(
                "Daemon exited with an error",
                &[("error", error.to_string())],
            );
            eprintln!("vapord: {error}");
            std::process::exit(1);
        }
    }
}
