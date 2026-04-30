use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, Subcommand};
use vapor_cli::{ConfigCommand, RunOptions, ServiceCommand};
use vapor_cli::{
    commands::{
        config as config_cmd, doctor as doctor_cmd, run as run_cmd, service as service_cmd,
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
        #[command(subcommand)]
        action: ServiceAction,
    },
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
                    let _ = ConfigCommand::Set {
                        key: key.clone(),
                        value: value.clone(),
                    };
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
        Command::Service { action } => dispatch_service(action),
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
