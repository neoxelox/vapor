// `PathBuf`, `Instant`, and the `service` command module are only referenced
// by the `#[cfg(target_os = "macos")]` service-install wiring below, so they
// are unused on the non-macOS build that CI compiles under `-D warnings`.
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::process::ExitCode;
#[cfg(target_os = "macos")]
use std::time::Instant;

use clap::{Parser, Subcommand};
#[cfg(target_os = "macos")]
use vapor_cli::commands::service as service_cmd;
use vapor_cli::{RunOptions, ServiceCommand};
use vapor_cli::{
    commands::{
        auth as auth_cmd, config as config_cmd, doctor as doctor_cmd, ipc as ipc_cmd,
        run as run_cmd,
    },
    resolve_configuration_path,
};

#[derive(Parser, Debug)]
#[command(
    name = "vapor",
    version = vapor_daemon::build_info::VERSION,
    about = "Vapor CLI — control plane for the portable Rust runtime",
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the daemon in-process (foreground).
    Run {
        #[arg(long)]
        foreground: bool,
    },
    /// Read or write a key in `vapor.json`.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Print version + git commit short.
    Version,
    /// Run platform-aware sanity checks.
    Doctor,
    /// Manage the platform-native service installation.
    Service {
        /// Install / drive the per-user service definition (default).
        /// `--system` is reserved for a future system-wide install
        /// flow; today it is rejected with an actionable error so
        /// scripts that want to opt in to the future surface fail
        /// loudly rather than silently treating the flag as unknown.
        #[arg(long, conflicts_with = "system")]
        user: bool,
        #[arg(long, conflicts_with = "user")]
        system: bool,
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Print live daemon status (queries the IPC server).
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Pause the running daemon — stops admitting new work.
    Pause,
    /// Resume a paused daemon.
    Resume,
    /// Hint the daemon to drain its pending queue as fast as the
    /// throttle allows.
    FlushNow,
    /// Request a fresh whole-scope reconcile.
    Reconcile,
    /// Print the diagnostics timeline (Wave 7 returns an empty list
    /// until the C8-30 in-memory buffer ships).
    Timeline {
        #[arg(long)]
        json: bool,
    },
    /// Tail the daemon log file at `<vapor_dir>/logs/vapord.logs`.
    Logs {
        #[arg(long)]
        tail: Option<usize>,
    },
    /// Manage stored authentication tokens.
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
}

#[derive(Subcommand, Debug)]
enum AuthAction {
    /// Store a token for `provider`. Pre-Wave-8 the token is supplied
    /// verbatim; the OAuth-PKCE flow lands later. Omit `--token` (or
    /// pass `--token -`) to read the token from stdin, which keeps the
    /// secret out of shell history and process listings.
    Login {
        provider: String,
        #[arg(long)]
        token: Option<String>,
    },
    /// Remove the stored token for `provider`.
    Logout { provider: String },
    /// List bound providers (never reveals the token value).
    Status,
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Print the current value (or empty when unset).
    Get { key: String },
    /// Update one key, preserving every other key in the file.
    Set { key: String, value: String },
}

#[derive(Subcommand, Debug)]
enum ServiceAction {
    /// App-startup path: install + start only when autolaunch is
    /// enabled (a disabled autolaunch is a silent no-op).
    Bootstrap {
        #[arg(long)]
        json: bool,
    },
    /// Enable autolaunch, install the service definition, and start
    /// the daemon.
    Install {
        #[arg(long)]
        json: bool,
    },
    /// Disable autolaunch and remove the service definition. Also
    /// sends the daemon an explicit stop unless `--keep-running` is
    /// passed.
    Uninstall {
        #[arg(long)]
        json: bool,
        /// Skip the explicit stop signal (the "disable autolaunch"
        /// toggle path). Note: on macOS, launchd tears the job down
        /// anyway when its service definition is booted out, so the
        /// daemon still exits; the flag matters on service managers
        /// that keep a disabled unit running (e.g. systemd).
        #[arg(long)]
        keep_running: bool,
    },
    Start {
        #[arg(long)]
        json: bool,
    },
    Stop {
        #[arg(long)]
        json: bool,
    },
    Restart {
        #[arg(long)]
        json: bool,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
    /// One supervision tick: detect an unexpected daemon exit, register
    /// it with the crash-loop guard, and restart when policy allows.
    Check {
        #[arg(long)]
        json: bool,
    },
    /// Clear a crash-loop pause so restarts may resume.
    Acknowledge {
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("vapor: {message}");
            ExitCode::from(1)
        }
    }
}

fn dispatch(cli: Cli) -> Result<ExitCode, String> {
    match cli.command {
        Command::Version => {
            println!("{}", vapor_cli::version_string());
            Ok(ExitCode::SUCCESS)
        }
        Command::Run { foreground } => {
            run_cmd::run(RunOptions { foreground }).map_err(|e| e.to_string())?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Config { action } => {
            let path = resolve_configuration_path();
            match action {
                ConfigAction::Get { key } => {
                    let value = config_cmd::get(&path, &key).map_err(|e| e.to_string())?;
                    match value {
                        Some(v) => println!("{v}"),
                        None => println!(),
                    }
                    Ok(ExitCode::SUCCESS)
                }
                ConfigAction::Set { key, value } => {
                    config_cmd::set(&path, &key, &value).map_err(|e| e.to_string())?;
                    Ok(ExitCode::SUCCESS)
                }
            }
        }
        Command::Doctor => {
            let report = doctor_cmd::run();
            for check in &report.checks {
                let badge = match check.status {
                    doctor_cmd::DoctorCheckStatus::Ok => "OK",
                    doctor_cmd::DoctorCheckStatus::Warning => "WARN",
                    doctor_cmd::DoctorCheckStatus::Failure => "FAIL",
                };
                println!("[{badge}] {} — {}", check.name, check.detail);
            }
            match report.worst_status() {
                doctor_cmd::DoctorCheckStatus::Failure => Ok(ExitCode::from(1)),
                _ => Ok(ExitCode::SUCCESS),
            }
        }
        Command::Service {
            user: _,
            system,
            action,
        } => {
            if system {
                return Err(
                    "--system service install is not implemented yet (Wave 12 / 13 work); \
                     pass --user (default) for the per-user LaunchAgent / systemd / Task \
                     Scheduler entry"
                        .to_string(),
                );
            }
            dispatch_service(action)
        }
        Command::Status { json } => dispatch_status(json),
        Command::Pause => dispatch_ack("pause", ipc_cmd::pause()),
        Command::Resume => dispatch_ack("resume", ipc_cmd::resume()),
        Command::FlushNow => dispatch_ack("flush-now", ipc_cmd::flush_now()),
        Command::Reconcile => dispatch_ack("reconcile", ipc_cmd::reconcile()),
        Command::Timeline { json } => dispatch_timeline(json),
        Command::Logs { tail } => dispatch_logs(tail),
        Command::Auth { action } => dispatch_auth(action),
    }
}

fn dispatch_status(json: bool) -> Result<ExitCode, String> {
    let status = ipc_cmd::status().map_err(|e| e.to_string())?;
    if json {
        let serialized = serde_json::to_string_pretty(&status).map_err(|e| e.to_string())?;
        println!("{serialized}");
    } else {
        println!("{}", ipc_cmd::render_status(&status));
    }
    Ok(ExitCode::SUCCESS)
}

fn dispatch_ack(
    command: &str,
    result: Result<vapor_ipc::AckResponse, ipc_cmd::IpcCliError>,
) -> Result<ExitCode, String> {
    let ack = result.map_err(|e| e.to_string())?;
    if ack.accepted {
        println!("{command}: ok ({})", ack.note);
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("{command}: not accepted — {}", ack.note);
        Ok(ExitCode::from(1))
    }
}

fn dispatch_timeline(json: bool) -> Result<ExitCode, String> {
    let timeline = ipc_cmd::timeline().map_err(|e| e.to_string())?;
    if json {
        let serialized = serde_json::to_string_pretty(&timeline).map_err(|e| e.to_string())?;
        println!("{serialized}");
    } else if timeline.entries.is_empty() {
        println!(
            "(timeline is empty — Wave 7 ships the IPC seam; in-memory buffer lands with C8-30)"
        );
    } else {
        for entry in &timeline.entries {
            println!(
                "[{}] {} — {}",
                entry.timestamp_ms, entry.kind, entry.message
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn dispatch_logs(tail: Option<usize>) -> Result<ExitCode, String> {
    let contents = ipc_cmd::tail_logs(tail).map_err(|e| e.to_string())?;
    if contents.is_empty() {
        println!("(no log lines yet)");
    } else {
        println!("{contents}");
    }
    Ok(ExitCode::SUCCESS)
}

fn dispatch_auth(action: AuthAction) -> Result<ExitCode, String> {
    let store = auth_cmd::build_native_store();
    let persistent = store.is_persistent();
    match action {
        AuthAction::Login { provider, token } => {
            let token = resolve_auth_token(token)?;
            auth_cmd::login_into(store.as_ref(), &provider, &token).map_err(|e| e.to_string())?;
            if persistent {
                println!("auth login: stored token for {provider}");
            } else {
                eprintln!(
                    "vapor: warning: native secret store is not yet wired in on this OS; \
                     the token was kept in process memory only and will not survive restart \
                     (see docs/tasks/core.md C4-5 / Waves 12 / 13)."
                );
                println!("auth login: stored token for {provider} (process-local only)");
            }
            Ok(ExitCode::SUCCESS)
        }
        AuthAction::Logout { provider } => {
            auth_cmd::logout_from(store.as_ref(), &provider).map_err(|e| e.to_string())?;
            println!("auth logout: removed token for {provider}");
            Ok(ExitCode::SUCCESS)
        }
        AuthAction::Status => {
            let entries = auth_cmd::status_from(store.as_ref()).map_err(|e| e.to_string())?;
            if !persistent {
                eprintln!(
                    "vapor: note: native secret store is not yet wired in on this OS; \
                     `bound` states below reflect process-local memory only."
                );
            }
            for entry in entries {
                let state = if entry.bound { "bound" } else { "not bound" };
                println!("{}: {}", entry.provider, state);
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Resolves the login token: an explicit `--token VALUE` is used
/// verbatim (with a hygiene warning, since argv leaks into shell history
/// and `ps` output); `--token -` or no flag reads one line from stdin.
fn resolve_auth_token(token: Option<String>) -> Result<String, String> {
    match token.as_deref() {
        Some("-") | None => {
            use std::io::BufRead;
            if token.is_none() {
                eprintln!("vapor: reading token from stdin (end with newline / EOF)");
            }
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map_err(|error| format!("failed to read token from stdin: {error}"))?;
            let token = line.trim();
            if token.is_empty() {
                return Err("no token provided on stdin".to_string());
            }
            Ok(token.to_string())
        }
        Some(value) => {
            eprintln!(
                "vapor: warning: passing --token on the command line exposes the secret to \
                 shell history and process listings; prefer piping it via stdin (`--token -`)."
            );
            Ok(value.to_string())
        }
    }
}

fn dispatch_service(action: ServiceAction) -> Result<ExitCode, String> {
    let (command, json) = match action {
        ServiceAction::Bootstrap { json } => (ServiceCommand::Bootstrap, json),
        ServiceAction::Install { json } => (ServiceCommand::Install, json),
        ServiceAction::Uninstall { json, keep_running } => {
            (ServiceCommand::Uninstall { keep_running }, json)
        }
        ServiceAction::Start { json } => (ServiceCommand::Start, json),
        ServiceAction::Stop { json } => (ServiceCommand::Stop, json),
        ServiceAction::Restart { json } => (ServiceCommand::Restart, json),
        ServiceAction::Status { json } => (ServiceCommand::Status, json),
        ServiceAction::Check { json } => (ServiceCommand::Check, json),
        ServiceAction::Acknowledge { json } => (ServiceCommand::Acknowledge, json),
    };

    #[cfg(target_os = "macos")]
    {
        let config_path = resolve_configuration_path();
        let daemon_binary = locate_daemon_binary().ok_or_else(|| {
            "vapord binary not found near the running CLI; install via the macOS app or set PATH"
                .to_string()
        })?;
        let (manager, installer) = service_cmd::build_native_macos(config_path, daemon_binary)
            .map_err(|e| e.to_string())?;
        let outcome = service_cmd::dispatch(command, &manager, installer.as_ref(), Instant::now())
            .map_err(|e| e.to_string())?;
        if json {
            let serialized = serde_json::to_string_pretty(&service_cmd::render_json(&outcome))
                .map_err(|e| e.to_string())?;
            println!("{serialized}");
        } else {
            println!("{}", service_cmd::render_text(&outcome));
        }
        Ok(ExitCode::SUCCESS)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (command, json);
        Err("`vapor service` currently supports macOS only (Wave 6); Linux / Windows land in Waves 12 / 13".to_string())
    }
}

#[cfg(target_os = "macos")]
fn locate_daemon_binary() -> Option<PathBuf> {
    if let Ok(current_exe) = std::env::current_exe()
        && let Some(parent) = current_exe.parent()
    {
        let sibling = parent.join("vapord");
        if sibling.is_file() {
            return Some(sibling);
        }
        // Bundled layout: the CLI ships at `Vapor.app/Contents/Helpers/vapor`
        // (it cannot sit next to the `Vapor` app binary — the default macOS
        // filesystem is case-insensitive), while `vapord` lives at
        // `Contents/MacOS/vapord`.
        if let Some(contents) = parent.parent() {
            let bundled = contents.join("MacOS").join("vapord");
            if bundled.is_file() {
                return Some(bundled);
            }
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("vapord");
            if candidate.is_file() {
                eprintln!(
                    "vapor: warning: using vapord from PATH ({}) instead of a sibling of this \
                     binary; the service definition will pin this path.",
                    candidate.display()
                );
                return Some(candidate);
            }
        }
    }
    None
}
