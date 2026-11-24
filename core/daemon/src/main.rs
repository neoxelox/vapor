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

    // Another Vapor process asks this binary to write or remove one
    // secret, so that the daemon is the one that created the item it
    // reads. Nothing of a daemon starts for that.
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some(vapor_platform::secrets::HELPER_COMMAND) {
        match vapor_platform::secrets::run_helper(&arguments[1..], &mut std::io::stdin()) {
            Ok(()) => return,
            Err(error) => {
                eprintln!("vapord: {error}");
                std::process::exit(1);
            }
        }
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
