// `PathBuf`, `Instant`, and the `service` command module are only referenced
// by the service-install wiring below, which exists where a native service
// manager does (launchd, systemd); they are unused on the Windows build that
// CI compiles under `-D warnings`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::time::Instant;

use clap::{Parser, Subcommand};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use vapor_cli::commands::service as service_cmd;
use vapor_cli::{RunOptions, ServiceCommand};
use vapor_cli::{
    commands::{
        auth as auth_cmd, config as config_cmd, decisions as decisions_cmd, doctor as doctor_cmd,
        ipc as ipc_cmd, run as run_cmd, trash as trash_cmd,
    },
    resolve_configuration_path,
};

#[derive(Parser, Debug)]
#[command(
    name = "vapor",
    // Include the git commit so `vapor --version` matches `vapor version`
    // and `vapord --version` (clap prepends the binary name).
    version = vapor_daemon::build_info::VERSION_WITH_COMMIT,
    about = "Keep a folder on this device in sync with a folder in the cloud.",
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Show what the daemon is doing and why.
    Status {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Pause syncing. Changes are still recorded and sync on resume.
    Pause {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Resume syncing.
    Resume {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Compare both folders now and sync the differences, without
    /// waiting for the device to go idle.
    Reconcile {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// List and answer the questions Vapor asks: conflicts (a file
    /// changed in more than one place) and irreversible actions it will
    /// not take on its own. Works with or without a running daemon.
    Decisions {
        #[command(subcommand)]
        action: DecisionsAction,
    },
    /// The files Vapor removed from this device, kept for a while so
    /// they can be restored. Works with or without a running daemon.
    Trash {
        #[command(subcommand)]
        action: TrashAction,
    },
    /// Read or change a setting in vapor.json.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Sign a profile in to its cloud provider, or out.
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Install, start, stop, and supervise the background daemon.
    Service {
        /// Use the per-user service (the default).
        #[arg(long, conflicts_with = "system")]
        user: bool,
        /// Use a system-wide service (not available yet).
        #[arg(long, conflicts_with = "user")]
        system: bool,
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Show recent daemon activity.
    Timeline {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// List queued and in-flight work, with the reason each item waits.
    Diagnostics {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the daemon log.
    Logs {
        /// Print only the last N lines.
        #[arg(long, value_name = "N")]
        tail: Option<usize>,
    },
    /// Check that this device can run Vapor, and say what to fix.
    Doctor {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Write a support report (settings, logs, and the daemon's live
    /// state) to share when asking for help.
    Report {
        /// Directory to write the report under (default
        /// `<vapor_dir>/reports`).
        #[arg(long, value_name = "DIR")]
        output: Option<std::path::PathBuf>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run the daemon in this terminal, for a server or for debugging.
    /// The service starts it on its own otherwise.
    Run {
        // Always foreground; hidden so it does not suggest a background
        // mode. Still parses (the e2e harness passes it).
        #[arg(long, hide = true)]
        foreground: bool,
    },
    /// Print the version and the git commit.
    Version,
}

#[derive(Subcommand, Debug)]
enum DecisionsAction {
    /// List the open decisions of every profile.
    List {
        /// Include answered decisions.
        #[arg(long)]
        all: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show one decision with its question, options, and evidence.
    Show {
        id: i64,
        /// The profile the id belongs to; needed only when several
        /// profiles have a decision with this id.
        #[arg(long, value_name = "ID")]
        profile: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Answer a decision. A conflict is resolved at once; anything else
    /// is applied by the daemon on its next tick, or at its next start.
    Resolve {
        id: i64,
        /// One of the decision's option keys (`show` lists them). For a
        /// conflict, the file name of the version to keep.
        #[arg(long, value_name = "OPTION")]
        choose: String,
        /// The profile the id belongs to; needed only when several
        /// profiles have a decision with this id.
        #[arg(long, value_name = "ID")]
        profile: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum TrashAction {
    /// List what is in the trash, newest first.
    List {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Put an item back where it was, or next to it when that path is
    /// taken again.
    Restore {
        id: String,
        /// The profile the id belongs to; needed only when several
        /// profiles have an item with this id.
        #[arg(long, value_name = "ID")]
        profile: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Delete everything in the trash, for good.
    Empty {
        /// Only this profile's trash (default: every profile).
        #[arg(long, value_name = "ID")]
        profile: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Print a setting's value, or its default when unset.
    Get { key: String },
    /// Change one setting and keep the rest of the file as it is.
    Set { key: String, value: String },
}

#[derive(Subcommand, Debug)]
enum AuthAction {
    /// Sign a profile in to its provider. Google Drive opens the
    /// browser; on a host without one, pipe a token on stdin.
    Login {
        /// The profile to sign in (default: the only profile).
        #[arg(long, value_name = "ID")]
        profile: Option<String>,
        /// Use this token instead of signing in. `-` reads it from
        /// stdin, which keeps it out of shell history.
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
        /// Open the browser even when stdin is not a terminal. The app's
        /// Sign in button uses it; without it a login with no terminal
        /// reads the token from stdin.
        #[arg(long, conflicts_with = "token")]
        browser: bool,
    },
    /// Remove a profile's stored credentials.
    Logout {
        /// The profile to sign out (default: the only profile).
        #[arg(long, value_name = "ID")]
        profile: Option<String>,
    },
    /// Show which profiles are signed in. Never prints a token.
    Status {
        /// Only this profile (default: every profile).
        #[arg(long, value_name = "ID")]
        profile: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum ServiceAction {
    /// Install and start the service when auto-launch is on; do
    /// nothing when it is off. What the app runs at startup.
    Bootstrap {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Turn auto-launch on, install the service, and start the daemon.
    Install {
        /// Also install a supervisor that restarts a crashed daemon, for
        /// a device without the app.
        #[arg(long)]
        supervise: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Turn auto-launch off, remove the service, and stop the daemon.
    Uninstall {
        /// Leave the daemon running. On macOS launchd stops it anyway
        /// when the service is removed.
        #[arg(long)]
        keep_running: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Start the installed daemon.
    Start {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop the daemon. The service stays installed.
    Stop {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Restart the daemon, which is how a restart-required setting
    /// takes effect.
    Restart {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Say whether the service is installed and running, or paused
    /// after repeated crashes.
    Status {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Restart the daemon if it exited unexpectedly, within the
    /// crash-loop limits.
    Check {
        /// Keep checking until stopped, printing only changes.
        #[arg(long = "loop")]
        keep_looping: bool,
        /// Seconds between checks with `--loop`.
        #[arg(long, value_name = "SECONDS")]
        interval: Option<u64>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Clear a crash-loop pause so the daemon may be restarted again.
    Acknowledge {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    // Rust's runtime sets SIGPIPE to ignore, which turns writes to a
    // closed pipe (`vapor logs | head -1`) into stdout panics. Restore
    // the default die-on-SIGPIPE so the CLI behaves like standard Unix
    // tools in pipelines.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
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
                    println!("{}", config_cmd::apply_hint(&key));
                    Ok(ExitCode::SUCCESS)
                }
            }
        }
        Command::Doctor { json } => {
            let report = doctor_cmd::run();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report.render_json())
                        .map_err(|e| e.to_string())?
                );
            } else {
                print!("{}", report.render_text());
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
                return Err("--system service install is not implemented yet; \
                     pass --user (default) for the per-user LaunchAgent / systemd / Task \
                     Scheduler entry"
                    .to_string());
            }
            dispatch_service(action)
        }
        Command::Status { json } => dispatch_status(json),
        Command::Pause { json } => dispatch_ack_json("pause", ipc_cmd::pause(), json),
        Command::Resume { json } => dispatch_ack_json("resume", ipc_cmd::resume(), json),
        Command::Reconcile { json } => dispatch_ack_json("reconcile", ipc_cmd::reconcile(), json),
        Command::Timeline { json } => dispatch_timeline(json),
        Command::Diagnostics { json } => dispatch_diagnostics(json),
        Command::Logs { tail } => dispatch_logs(tail),
        Command::Auth { action } => dispatch_auth(action),
        Command::Report { output, json } => dispatch_report(output, json),
        Command::Decisions { action } => dispatch_decisions(action),
        Command::Trash { action } => dispatch_trash(action),
    }
}

fn dispatch_decisions(action: DecisionsAction) -> Result<ExitCode, String> {
    let loaded = vapor_shared::config::load_from(&resolve_configuration_path());
    if let Some(issue) = loaded.load_issue {
        eprintln!("vapor: warning: {issue}");
    }
    match action {
        DecisionsAction::List { json, all } => {
            let report = decisions_cmd::list_decisions(&loaded.config, all);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
            } else {
                println!("{}", decisions_cmd::render_list(&report));
            }
            Ok(ExitCode::SUCCESS)
        }
        DecisionsAction::Show { id, profile, json } => {
            let decision = decisions_cmd::show_decision(&loaded.config, id, profile.as_deref())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&decision).map_err(|e| e.to_string())?
                );
            } else {
                println!("{}", decisions_cmd::render_show(&decision));
            }
            Ok(ExitCode::SUCCESS)
        }
        DecisionsAction::Resolve {
            id,
            choose,
            profile,
            json,
        } => {
            let decision =
                decisions_cmd::resolve_decision(&loaded.config, id, &choose, profile.as_deref())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&decision).map_err(|e| e.to_string())?
                );
            } else if decision.applied_at_ms.is_some() {
                println!("Applied {choose} for decision #{id}.");
            } else {
                println!(
                    "Recorded {choose} for decision #{id}; the daemon applies it on its next tick (or at its next start)."
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn dispatch_trash(action: TrashAction) -> Result<ExitCode, String> {
    let loaded = vapor_shared::config::load_from(&resolve_configuration_path());
    if let Some(issue) = loaded.load_issue {
        eprintln!("vapor: warning: {issue}");
    }
    match action {
        TrashAction::List { json } => {
            let report = trash_cmd::list_trash(&loaded.config);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
            } else {
                println!("{}", trash_cmd::render_list(&report));
            }
            Ok(ExitCode::SUCCESS)
        }
        TrashAction::Restore { id, profile, json } => {
            let report = trash_cmd::restore(&loaded.config, &id, profile.as_deref())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
            } else {
                println!("Restored {} to {}", report.id, report.restored_to.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        TrashAction::Empty { profile, json } => {
            let report = trash_cmd::empty(&loaded.config, profile.as_deref());
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
            } else {
                println!("Removed {} item(s) from the trash.", report.removed);
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn dispatch_report(output: Option<std::path::PathBuf>, json: bool) -> Result<ExitCode, String> {
    use vapor_cli::commands::report as support;

    // Live captures are best-effort and independent: a daemon that
    // answers one endpoint but fails another still contributes what it
    // could, and the failures are recorded rather than discarding the
    // successful captures.
    let mut capture_errors = Vec::new();
    let capture =
        |result: Result<String, String>, endpoint: &str, errors: &mut Vec<String>| match result {
            Ok(value) => Some(value),
            Err(error) => {
                errors.push(format!("{endpoint}: {error}"));
                None
            }
        };
    let status_json = capture(
        ipc_cmd::status()
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::to_string_pretty(&s).map_err(|e| e.to_string())),
        "status",
        &mut capture_errors,
    );
    let diagnostics_json = capture(
        ipc_cmd::diagnostics()
            .map_err(|e| e.to_string())
            .and_then(|d| serde_json::to_string_pretty(&d).map_err(|e| e.to_string())),
        "diagnostics",
        &mut capture_errors,
    );
    let timeline_json = capture(
        ipc_cmd::timeline()
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::to_string_pretty(&t).map_err(|e| e.to_string())),
        "timeline",
        &mut capture_errors,
    );
    let live = Some(support::LiveCaptures {
        status_json,
        diagnostics_json,
        timeline_json,
        capture_errors,
    });

    let vapor_dir = vapor_shared::runtime_paths::vapor_directory();
    let output_root = output.unwrap_or_else(|| vapor_dir.join("reports"));
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .map_err(|_| "system clock is before the Unix epoch".to_string())?;
    let report = support::write_support_report(&vapor_dir, &output_root, live, timestamp_ms)
        .map_err(|e| format!("cannot write the support report: {e}"))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", support::render_report(&report));
    }
    Ok(ExitCode::SUCCESS)
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

/// `dispatch_ack` with a `--json` form for the app shim: the ack as the
/// daemon sent it, exit 0 when accepted and 1 otherwise, like the text
/// form.
fn dispatch_ack_json(
    command: &str,
    result: Result<vapor_ipc::AckResponse, ipc_cmd::IpcCliError>,
    json: bool,
) -> Result<ExitCode, String> {
    if !json {
        return dispatch_ack(command, result);
    }
    let ack = result.map_err(|e| e.to_string())?;
    let serialized = serde_json::to_string_pretty(&ack).map_err(|e| e.to_string())?;
    println!("{serialized}");
    Ok(if ack.accepted {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
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
        println!("(no timeline events recorded yet)");
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

fn dispatch_diagnostics(json: bool) -> Result<ExitCode, String> {
    let diagnostics = ipc_cmd::diagnostics().map_err(|e| e.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&diagnostics).map_err(|e| e.to_string())?
        );
    } else {
        println!("{}", ipc_cmd::render_diagnostics(&diagnostics));
    }
    Ok(ExitCode::SUCCESS)
}

fn dispatch_logs(tail: Option<usize>) -> Result<ExitCode, String> {
    match tail {
        // Bounded: the backward chunk-scan returns at most `n` lines.
        Some(line_count) => {
            let contents = ipc_cmd::tail_logs(Some(line_count)).map_err(|e| e.to_string())?;
            if contents.is_empty() {
                println!("(no log lines yet)");
            } else {
                println!("{contents}");
            }
        }
        // Stream the whole file so `vapor logs` on a large log does not
        // spike CLI memory by the full file size.
        None => {
            use std::io::Write;
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let wrote = ipc_cmd::stream_full_log(&mut lock).map_err(|e| e.to_string())?;
            if wrote {
                let _ = lock.flush();
            } else {
                let _ = writeln!(lock, "(no log lines yet)");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn dispatch_auth(action: AuthAction) -> Result<ExitCode, String> {
    let loaded = vapor_shared::config::load_from(&resolve_configuration_path());
    if let Some(issue) = loaded.load_issue {
        eprintln!("vapor: warning: {issue}");
    }
    let config = loaded.config;
    let (store, fallback_reason) = auth_cmd::build_native_store();
    let persistent = store.is_persistent();
    let fallback_reason = fallback_reason.unwrap_or_default();
    match action {
        AuthAction::Login {
            profile,
            token,
            browser,
        } => {
            let profile =
                auth_cmd::select_profile(&config, profile.as_deref()).map_err(|e| e.to_string())?;
            auth_cmd::require_sign_in(&profile).map_err(|e| e.to_string())?;
            // Google Drive without an explicit --token runs the full
            // OAuth-PKCE browser flow — but only interactively. When stdin
            // is not a TTY (`echo "$TOKEN" | vapor auth login`, CI,
            // automation) fall back to the documented stdin read instead of
            // binding a loopback listener and blocking forever on a browser
            // redirect that will never come. `--browser` is the caller
            // saying a person is at the browser (the app's Sign in button).
            use std::io::IsTerminal;
            let interactive_gdrive = profile.provider_kind
                == vapor_shared::constants::provider::GDRIVE
                && token.is_none()
                && (browser || std::io::stdin().is_terminal());
            let token = if interactive_gdrive {
                auth_cmd::run_gdrive_pkce_flow()?
            } else {
                resolve_auth_token(token)?
            };
            auth_cmd::login_into(store.as_ref(), &profile, &token).map_err(|e| e.to_string())?;
            if persistent {
                println!(
                    "Signed profile {} in to {}.",
                    profile.id, profile.provider_kind
                );
            } else {
                eprintln!(
                    "vapor: warning: {fallback_reason}; the token was kept in process memory \
                     only and will not survive restart."
                );
                println!(
                    "Signed profile {} in to {} (in this process only).",
                    profile.id, profile.provider_kind
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        AuthAction::Logout { profile } => {
            let profile =
                auth_cmd::select_profile(&config, profile.as_deref()).map_err(|e| e.to_string())?;
            let removed =
                auth_cmd::logout_from(store.as_ref(), &profile.id).map_err(|e| e.to_string())?;
            if removed {
                println!("Signed profile {} out.", profile.id);
            } else {
                println!("Profile {} was not signed in.", profile.id);
            }
            Ok(ExitCode::SUCCESS)
        }
        AuthAction::Status { profile, json } => {
            let report = auth_cmd::status_from(store.as_ref(), &config, profile.as_deref())
                .map_err(|e| e.to_string())?;
            if !persistent {
                eprintln!(
                    "vapor: note: {fallback_reason}; the states below reflect this process only."
                );
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
            } else {
                println!("{}", auth_cmd::render_status(&report));
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
            // Trim and reject empty like the stdin path: an explicit
            // `--token ""` (or unset `$TOKEN`) otherwise stores a useless
            // empty credential that reports as "bound but broken".
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err("no token provided".to_string());
            }
            eprintln!(
                "vapor: warning: passing --token on the command line exposes the secret to \
                 shell history and process listings; prefer piping it via stdin (`--token -`)."
            );
            Ok(trimmed.to_string())
        }
    }
}

fn dispatch_service(action: ServiceAction) -> Result<ExitCode, String> {
    let mut supervise = false;
    let mut check_loop: Option<Duration> = None;
    let (command, json) = match action {
        ServiceAction::Bootstrap { json } => (ServiceCommand::Bootstrap, json),
        ServiceAction::Install { json, supervise: s } => {
            supervise = s;
            (ServiceCommand::Install, json)
        }
        ServiceAction::Uninstall { json, keep_running } => {
            (ServiceCommand::Uninstall { keep_running }, json)
        }
        ServiceAction::Start { json } => (ServiceCommand::Start, json),
        ServiceAction::Stop { json } => (ServiceCommand::Stop, json),
        ServiceAction::Restart { json } => (ServiceCommand::Restart, json),
        ServiceAction::Status { json } => (ServiceCommand::Status, json),
        ServiceAction::Check {
            json,
            keep_looping,
            interval,
        } => {
            if keep_looping {
                check_loop = Some(Duration::from_secs(interval.unwrap_or(
                    vapor_shared::constants::service::HEALTH_TICK_INTERVAL_SECONDS,
                )));
            } else if interval.is_some() {
                return Err("--interval only means something with --loop".to_string());
            }
            (ServiceCommand::Check, json)
        }
        ServiceAction::Acknowledge { json } => (ServiceCommand::Acknowledge, json),
    };

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let config_path = resolve_configuration_path();
        let daemon_binary = locate_daemon_binary().ok_or_else(|| {
            "vapord binary not found near the running CLI; install via the macOS app or set PATH"
                .to_string()
        })?;
        let (manager, installer) =
            service_cmd::build_native(config_path, daemon_binary).map_err(|e| e.to_string())?;
        if let Some(interval) = check_loop {
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = stop.clone();
            vapor_platform::ProcessSupervisor::register_shutdown_handler(
                &vapor_platform::NativeProcessSupervisor::new(),
                move || flag.store(true, std::sync::atomic::Ordering::SeqCst),
            )
            .map_err(|e| e.to_string())?;
            service_cmd::check_loop(&manager, installer.as_ref(), interval, json, &stop)
                .map_err(|e| e.to_string())?;
            return Ok(ExitCode::SUCCESS);
        }
        let supervisor = service_cmd::supervisor_installer().map_err(|e| e.to_string())?;
        let mut outcome =
            service_cmd::dispatch(command, &manager, installer.as_ref(), Instant::now())
                .map_err(|e| e.to_string())?;
        match &mut outcome {
            service_cmd::ServiceCommandOutcome::Status(report) => {
                report.supervisor_installed = vapor_platform::ServiceInstaller::status(&supervisor)
                    .map(|status| status != vapor_platform::ServiceStatus::NotInstalled)
                    .unwrap_or(false);
            }
            service_cmd::ServiceCommandOutcome::Action(_) if supervise => {
                vapor_platform::ServiceInstaller::install_and_enable(&supervisor)
                    .map_err(|e| format!("supervisor install failed: {e}"))?;
            }
            service_cmd::ServiceCommandOutcome::Action(_)
                if matches!(command, ServiceCommand::Uninstall { .. })
                    && vapor_platform::ServiceInstaller::status(&supervisor).is_ok_and(
                        |status| status != vapor_platform::ServiceStatus::NotInstalled,
                    ) =>
            {
                vapor_platform::ServiceInstaller::disable_and_uninstall(&supervisor)
                    .map_err(|e| format!("supervisor uninstall failed: {e}"))?;
            }
            _ => {}
        }
        if json {
            let serialized = serde_json::to_string_pretty(&service_cmd::render_json(&outcome))
                .map_err(|e| e.to_string())?;
            println!("{serialized}");
        } else {
            println!("{}", service_cmd::render_text(&outcome));
        }
        Ok(ExitCode::SUCCESS)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (command, json, supervise, check_loop);
        Err(
            "`vapor service` needs a native service manager; Windows lands with that surface"
                .to_string(),
        )
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn locate_daemon_binary() -> Option<PathBuf> {
    use vapor_cli::commands::daemon_binary::{self, DaemonBinarySource};
    let daemon = daemon_binary::locate()?;
    if daemon.source == DaemonBinarySource::SearchPath {
        eprintln!(
            "vapor: warning: using vapord from PATH ({}) instead of a sibling of this \
             binary; the service definition will pin this path.",
            daemon.path.display()
        );
    }
    Some(daemon.path)
}
