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
        auth as auth_cmd, config as config_cmd, conflicts as conflicts_cmd,
        decisions as decisions_cmd, doctor as doctor_cmd, ipc as ipc_cmd, run as run_cmd,
    },
    resolve_configuration_path,
};

#[derive(Parser, Debug)]
#[command(
    name = "vapor",
    // Include the git commit so `vapor --version` matches `vapor version`
    // and `vapord --version` (clap prepends the binary name).
    version = vapor_daemon::build_info::VERSION_WITH_COMMIT,
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
        // Reserved/always-on: `vapor run` is always foreground. Hidden so
        // its presence does not imply a background/daemonize mode exists.
        // Still parses (the e2e harness passes it).
        #[arg(long, hide = true)]
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
    Doctor {
        /// Emit the report as JSON (`{"checks": [...], "worst_status": ...}`).
        #[arg(long)]
        json: bool,
    },
    /// Manage the platform-native service installation.
    Service {
        /// Install / drive the per-user service definition (default).
        #[arg(long, conflicts_with = "system")]
        user: bool,
        /// Reserved for a future system-wide install flow; rejected
        /// today with an actionable error so scripts that opt in early
        /// fail loudly instead of being treated as unknown.
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
    /// Print the diagnostics timeline.
    Timeline {
        #[arg(long)]
        json: bool,
    },
    /// Per-intent "why stuck" diagnostics from the running daemon.
    Diagnostics {
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
    /// Export a shareable support bundle: config, logs, and (when the
    /// daemon is running) live status / diagnostics / timeline.
    SupportBundle {
        /// Directory to create the bundle under (defaults to
        /// `<vapor_dir>/support`).
        #[arg(long)]
        output: Option<std::path::PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// List and resolve keep-both sync conflicts.
    Conflicts {
        #[command(subcommand)]
        action: ConflictsAction,
    },
    /// List and answer the questions the daemon parked (an irreversible
    /// action on ambiguous evidence). Works with or without a running
    /// daemon; the daemon applies an answer on its next tick.
    Decisions {
        #[command(subcommand)]
        action: DecisionsAction,
    },
}

#[derive(Subcommand, Debug)]
enum DecisionsAction {
    /// Open decisions of every enabled profile (`--all` includes
    /// answered ones).
    List {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        all: bool,
    },
    /// One decision with its question, options, and evidence.
    Show {
        id: i64,
        /// Profile the id belongs to (needed only when several profiles
        /// hold the same id).
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Answer a decision with one of its option keys.
    Resolve {
        id: i64,
        #[arg(long)]
        choose: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum ConflictsAction {
    /// Scan every enabled profile's local root for unresolved
    /// `~conflict-` copies. Works with or without a running daemon.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Resolve one conflict: keep either the canonical file or the
    /// conflict copy; the discarded version is deleted and the change
    /// syncs like any other edit.
    Resolve {
        /// Path to the `…~conflict-…` copy (as printed by `list`).
        path: std::path::PathBuf,
        /// Which version survives under the canonical name:
        /// 'canonical' or 'copy'.
        #[arg(long)]
        keep: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum AuthAction {
    /// Store a token for `provider`. Omit `--token` (or pass
    /// `--token -`) to read the token from stdin — except for `gdrive`,
    /// where omitting `--token` starts the OAuth-PKCE browser flow
    /// instead. Reading from stdin keeps the secret out of shell
    /// history and process listings.
    Login {
        provider: String,
        #[arg(long)]
        token: Option<String>,
        /// Profile the credential belongs to; defaults to the
        /// implicit `default` profile.
        #[arg(long, default_value = "default")]
        profile: String,
    },
    /// Remove the stored token for `provider`.
    Logout {
        provider: String,
        #[arg(long, default_value = "default")]
        profile: String,
    },
    /// List bound providers for a profile (never reveals the token
    /// value).
    Status {
        #[arg(long, default_value = "default")]
        profile: String,
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
    /// Start the installed daemon through the service manager.
    Start {
        #[arg(long)]
        json: bool,
    },
    /// Stop the daemon; the service definition stays installed.
    Stop {
        #[arg(long)]
        json: bool,
    },
    /// Stop and start the daemon, which is how a changed `vapor.json`
    /// takes effect.
    Restart {
        #[arg(long)]
        json: bool,
    },
    /// Report whether the service is installed, running, or paused by
    /// the crash-loop guard.
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
        Command::Pause => dispatch_ack("pause", ipc_cmd::pause()),
        Command::Resume => dispatch_ack("resume", ipc_cmd::resume()),
        Command::FlushNow => dispatch_ack("flush-now", ipc_cmd::flush_now()),
        Command::Reconcile => dispatch_ack("reconcile", ipc_cmd::reconcile()),
        Command::Timeline { json } => dispatch_timeline(json),
        Command::Diagnostics { json } => dispatch_diagnostics(json),
        Command::Logs { tail } => dispatch_logs(tail),
        Command::Auth { action } => dispatch_auth(action),
        Command::SupportBundle { output, json } => dispatch_support_bundle(output, json),
        Command::Conflicts { action } => dispatch_conflicts(action),
        Command::Decisions { action } => dispatch_decisions(action),
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
            } else {
                println!(
                    "Recorded {choose} for decision #{id}; the daemon applies it on its next tick (or at its next start)."
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn dispatch_conflicts(action: ConflictsAction) -> Result<ExitCode, String> {
    match action {
        ConflictsAction::List { json } => {
            let loaded = vapor_shared::config::load_from(&resolve_configuration_path());
            if let Some(issue) = loaded.load_issue {
                eprintln!("vapor: warning: {issue}");
            }
            let report = conflicts_cmd::list_conflicts(&loaded.config);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
            } else {
                println!("{}", conflicts_cmd::render_list(&report));
            }
            Ok(ExitCode::SUCCESS)
        }
        ConflictsAction::Resolve { path, keep, json } => {
            let keep = conflicts_cmd::KeepSide::parse(&keep)?;
            let report = conflicts_cmd::resolve_conflict(&path, keep)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
            } else {
                println!("{}", conflicts_cmd::render_resolution(&report));
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn dispatch_support_bundle(
    output: Option<std::path::PathBuf>,
    json: bool,
) -> Result<ExitCode, String> {
    use vapor_cli::commands::support;

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
    let output_root = output.unwrap_or_else(|| vapor_dir.join("support"));
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .map_err(|_| "system clock is before the Unix epoch".to_string())?;
    let report = support::collect_support_bundle(&vapor_dir, &output_root, live, timestamp_ms)
        .map_err(|e| format!("cannot collect support bundle: {e}"))?;

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
    let store = auth_cmd::build_native_store();
    let persistent = store.is_persistent();
    match action {
        AuthAction::Login {
            provider,
            token,
            profile,
        } => {
            // Google Drive without an explicit --token runs the full
            // OAuth-PKCE browser flow — but only interactively. When stdin
            // is not a TTY (`echo "$TOKEN" | vapor auth login gdrive`, CI,
            // automation) fall back to the documented stdin read instead of
            // binding a loopback listener and blocking forever on a browser
            // redirect that will never come.
            use std::io::IsTerminal;
            let interactive_gdrive = provider == vapor_shared::constants::provider::GDRIVE
                && token.is_none()
                && std::io::stdin().is_terminal();
            let token = if interactive_gdrive {
                auth_cmd::run_gdrive_pkce_flow()?
            } else {
                resolve_auth_token(token)?
            };
            auth_cmd::login_into(store.as_ref(), &profile, &provider, &token)
                .map_err(|e| e.to_string())?;
            if persistent {
                println!("auth login: stored token for {provider} (profile {profile})");
            } else {
                eprintln!(
                    "vapor: warning: no native secret store on this OS yet; the token was \
                     kept in process memory only and will not survive restart."
                );
                println!(
                    "auth login: stored token for {provider} (profile {profile}, process-local only)"
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        AuthAction::Logout { provider, profile } => {
            auth_cmd::logout_from(store.as_ref(), &profile, &provider)
                .map_err(|e| e.to_string())?;
            println!("auth logout: removed token for {provider} (profile {profile})");
            Ok(ExitCode::SUCCESS)
        }
        AuthAction::Status { profile } => {
            let entries =
                auth_cmd::status_from(store.as_ref(), &profile).map_err(|e| e.to_string())?;
            if !persistent {
                eprintln!(
                    "vapor: note: no native secret store on this OS yet; `bound` states \
                     below reflect process-local memory only."
                );
            }
            for entry in entries {
                let state = if entry.bound { "bound" } else { "not bound" };
                println!("{} ({}): {}", entry.provider, entry.profile, state);
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
        Err("`vapor service` currently supports macOS only; Linux and Windows land with those surfaces".to_string())
    }
}

#[cfg(target_os = "macos")]
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
