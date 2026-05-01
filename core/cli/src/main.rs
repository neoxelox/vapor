use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, Subcommand};
use vapor_cli::{RunOptions, ServiceCommand};
use vapor_cli::{
    commands::{
        auth as auth_cmd, config as config_cmd, doctor as doctor_cmd, ipc as ipc_cmd,
        run as run_cmd, service as service_cmd,
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
    /// verbatim via `--token`; the OAuth-PKCE flow lands later.
    Login {
        provider: String,
        #[arg(long)]
        token: String,
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
    Install,
    Uninstall,
    Start,
    Stop,
    Restart,
    Status,
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
            auth_cmd::login_into(store.as_ref(), &provider, &token).map_err(|e| e.to_string())?;
            if persistent {
                println!("auth login: stored token for {provider}");
            } else {
                eprintln!(
                    "vapor: warning: native secret store is not yet wired in on this OS; \
                     the token was kept in process memory only and will not survive restart \
                     (see core/tasks/core.md C4-5 / Waves 12 / 13)."
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

fn dispatch_service(action: ServiceAction) -> Result<ExitCode, String> {
    let command = match action {
        ServiceAction::Install => ServiceCommand::Install,
        ServiceAction::Uninstall => ServiceCommand::Uninstall,
        ServiceAction::Start => ServiceCommand::Start,
        ServiceAction::Stop => ServiceCommand::Stop,
        ServiceAction::Restart => ServiceCommand::Restart,
        ServiceAction::Status => ServiceCommand::Status,
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
        let report = service_cmd::dispatch(command, &manager, installer.as_ref(), Instant::now())
            .map_err(|e| e.to_string())?;
        if let Some(report) = report {
            println!(
                "service status: {:?} (label: {})",
                report.status, report.label
            );
        }
        Ok(ExitCode::SUCCESS)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = command;
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
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("vapord");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
