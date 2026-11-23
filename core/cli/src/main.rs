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
        init as init_cmd, ipc as ipc_cmd, profiles as profiles_cmd, run as run_cmd,
        trash as trash_cmd,
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
    /// Set Vapor up on this device: the folders to sync, the sign-in,
    /// and the background service. A terminal asks for each answer;
    /// with no terminal every answer is an option. Run it again at any
    /// time: it does only what is left.
    Init {
        /// The cloud provider: `filesystem` or `gdrive`.
        #[arg(long, value_name = "PROVIDER")]
        provider: Option<String>,
        /// The folder on this device. Needed when no terminal can ask
        /// for it.
        #[arg(long, value_name = "PATH")]
        local: Option<String>,
        /// The folder in the cloud. Needed when no terminal can ask
        /// for it.
        #[arg(long, value_name = "PATH")]
        cloud: Option<String>,
        /// `two-way` (the default), `pull-only`, or `push-only`. A
        /// one-way mode overwrites and removes files on one side, and
        /// needs a confirmation before it runs.
        #[arg(long, value_name = "MODE")]
        mode: Option<String>,
        /// Read the sign-in token from stdin instead of opening the
        /// browser: `--token -`. A token is never taken from the
        /// command line.
        #[arg(long, value_name = "-")]
        token: Option<String>,
        /// Whether Vapor starts at login and runs in the background
        /// (`on` when left out with no terminal). `off` records the
        /// answer and leaves the service as it is.
        #[arg(long, value_name = "on|off")]
        auto_launch: Option<Switch>,
        /// Lingering keeps the daemon running after the last logout.
        /// Vapor turns it on where no desktop keeps a session open;
        /// `off` leaves the setting alone, `on` sets it on a desktop too.
        #[cfg(target_os = "linux")]
        #[arg(long, value_name = "on|off")]
        linger: Option<Switch>,
        /// Print JSON. Never asks a question.
        #[arg(long)]
        json: bool,
    },
    /// Show what the daemon is doing and why.
    Status {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Pause syncing. Changes are still recorded and sync on resume.
    Pause {
        /// Pause this profile only. Without it, every profile pauses.
        #[arg(long)]
        profile: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Resume syncing.
    Resume {
        /// Resume this profile only. Without it, every profile resumes.
        #[arg(long)]
        profile: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Compare both folders now and sync the differences, without
    /// waiting for the device to go idle.
    Reconcile {
        /// Compare the folders of this profile only. Without it, every
        /// profile compares its folders.
        #[arg(long)]
        profile: Option<String>,
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
    /// Set up, list, and remove sync profiles. A profile is one folder
    /// on this device kept in sync with one folder in the cloud.
    Profiles {
        #[command(subcommand)]
        action: ProfilesAction,
    },
    /// List the files and folders on this device that the ignore rules
    /// keep out of sync, each with its rule. Works with or without a
    /// running daemon.
    Ignored {
        /// Only this profile (default: every profile).
        #[arg(long, value_name = "ID")]
        profile: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
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
    /// Install, start, stop, and supervise the background daemon, as a
    /// service of this user.
    Service {
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
        /// `<vapor_dir>/reports`, which keeps the five newest).
        #[arg(long, value_name = "DIR")]
        output: Option<std::path::PathBuf>,
        /// Keep folder paths and file names as they are. Without this
        /// the report replaces each name by a code, so it can be shared.
        #[arg(long)]
        keep_paths: bool,
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
    /// Delete everything in the trash, for good. Asks first on a
    /// terminal; anywhere else it needs `--yes`.
    Empty {
        /// Only this profile's trash (default: every profile).
        #[arg(long, value_name = "ID")]
        profile: Option<String>,
        /// Do not ask for a confirmation.
        #[arg(long)]
        yes: bool,
        /// Print JSON. Needs `--yes`.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum ProfilesAction {
    /// List the profiles, with what stops one from running.
    List {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Add a profile. With no option on a terminal it asks for each
    /// value; an option that is left out takes the suggested value. The
    /// first profile needs no id.
    Add {
        /// A short name for the profile: lowercase letters, digits,
        /// `-` and `_`. Needed from the second profile on.
        #[arg(long, value_name = "ID")]
        id: Option<String>,
        /// The name the surfaces show (default: the id).
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// The cloud provider: `filesystem` or `gdrive`.
        #[arg(long, value_name = "PROVIDER")]
        provider: Option<String>,
        /// The folder on this device. Needed when no terminal can ask
        /// for it (there the suggestion is `~/Vapor`).
        #[arg(long, value_name = "PATH")]
        local: Option<String>,
        /// The folder in the cloud. Needed when no terminal can ask
        /// for it (there the suggestion is `/Vapor` on Google Drive,
        /// `~/cloud/Vapor` for the filesystem provider).
        #[arg(long, value_name = "PATH")]
        cloud: Option<String>,
        /// `two-way` (the default), `pull-only`, or `push-only`. A
        /// one-way mode overwrites and removes files on one side, and
        /// needs a confirmation before it runs.
        #[arg(long, value_name = "MODE")]
        mode: Option<String>,
        /// Print JSON. Never asks a question.
        #[arg(long)]
        json: bool,
    },
    /// Remove a profile: Vapor stops syncing its folders and forgets
    /// its sync state and its sign-in. The files on both sides stay.
    /// The daemon must be stopped.
    Remove {
        id: String,
        /// Do not ask for a confirmation.
        #[arg(long)]
        yes: bool,
        /// Print JSON. Needs `--yes`.
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
        /// Read a token from stdin instead of signing in: `--token -`.
        /// A token is never taken from the command line, where the
        /// process list and the shell history would show it.
        #[arg(long, value_name = "-")]
        token: Option<String>,
        /// Open the browser even when stdin is not a terminal. The app's
        /// Sign in button uses it; without it a login with no terminal
        /// reads the token from stdin.
        #[arg(long, conflicts_with = "token")]
        browser: bool,
    },
    /// Remove a profile's stored credentials. With no profile yet, it
    /// removes the sign-in that `vapor init` made before it wrote one.
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
    /// Turn auto-launch on, install the service and the supervisor
    /// that restarts a crashed daemon, and start the daemon.
    Install {
        /// Lingering keeps the daemon running after the last logout.
        /// Vapor turns it on where no desktop keeps a session open;
        /// `off` leaves the setting alone, `on` sets it on a desktop too.
        #[cfg(target_os = "linux")]
        #[arg(long, value_name = "on|off")]
        linger: Option<Switch>,
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
    /// crash-loop limits. The supervisor job runs this; `status` reads
    /// the same state and changes nothing.
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

/// The answer to a yes-or-no option.
#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum Switch {
    On,
    Off,
}

impl Switch {
    fn is_on(self) -> bool {
        self == Self::On
    }
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
                    println!("{}", config_cmd::apply_hint(&key, &value));
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
        Command::Init {
            provider,
            local,
            cloud,
            mode,
            token,
            auto_launch,
            #[cfg(target_os = "linux")]
            linger,
            json,
        } => {
            #[cfg(not(target_os = "linux"))]
            let linger: Option<Switch> = None;
            dispatch_init(
                init_cmd::InitRequest {
                    provider,
                    local,
                    cloud,
                    mode,
                    auto_launch: auto_launch.map(Switch::is_on),
                    linger: linger.map(Switch::is_on),
                },
                token,
                json,
            )
        }
        Command::Service { action } => dispatch_service(action),
        Command::Status { json } => dispatch_status(json),
        Command::Pause { profile, json } => {
            dispatch_ack_json("pause", ipc_cmd::pause(profile.as_deref()), json)
        }
        Command::Resume { profile, json } => {
            dispatch_ack_json("resume", ipc_cmd::resume(profile.as_deref()), json)
        }
        Command::Reconcile { profile, json } => {
            dispatch_ack_json("reconcile", ipc_cmd::reconcile(profile.as_deref()), json)
        }
        Command::Timeline { json } => dispatch_timeline(json),
        Command::Diagnostics { json } => dispatch_diagnostics(json),
        Command::Logs { tail } => dispatch_logs(tail),
        Command::Auth { action } => dispatch_auth(action),
        Command::Report {
            output,
            keep_paths,
            json,
        } => dispatch_report(output, keep_paths, json),
        Command::Decisions { action } => dispatch_decisions(action),
        Command::Trash { action } => dispatch_trash(action),
        Command::Profiles { action } => dispatch_profiles(action),
        Command::Ignored { profile, json } => {
            use vapor_cli::commands::ignored;
            let loaded = vapor_shared::config::load_from(&resolve_configuration_path());
            if let Some(issue) = loaded.load_issue {
                eprintln!("vapor: warning: {issue}");
            }
            let report =
                ignored::list_ignored(&loaded.config, profile.as_deref(), ignored::LIST_LIMIT)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
            } else {
                println!("{}", ignored::render(&report));
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// The real device behind `vapor init`: the browser or stdin for a
/// sign-in, the provider for a look into the account, and the service
/// manager.
struct NativeInitHost {
    interactive: bool,
    token_on_stdin: bool,
    config_path: std::path::PathBuf,
}

impl init_cmd::InitHost for NativeInitHost {
    fn sign_in(&mut self, provider: &str) -> Option<Result<String, String>> {
        if self.token_on_stdin {
            return Some(resolve_auth_token(Some("-".to_string())));
        }
        if self.interactive && provider == vapor_shared::constants::provider::GDRIVE {
            return Some(auth_cmd::run_gdrive_pkce_flow());
        }
        None
    }

    fn cloud_folder(
        &mut self,
        provider: &str,
        profile_id: &str,
        cloud_root: &str,
    ) -> init_cmd::CloudFolder {
        use init_cmd::CloudFolder;
        let provider = match vapor_providers::select_provider_for_profile(provider, profile_id) {
            Ok(provider) => provider,
            Err(error) => return CloudFolder::Unchecked(error.to_string()),
        };
        match provider.root_identity(cloud_root) {
            Ok(_) => CloudFolder::Exists,
            Err(error) if error.kind == vapor_shared::ProviderErrorKind::NotFound => {
                CloudFolder::New
            }
            Err(error) => CloudFolder::Unchecked(error.to_string()),
        }
    }

    fn service(&mut self) -> init_cmd::ServiceState {
        use init_cmd::ServiceState;
        let auto_launch = config_cmd::get(
            &self.config_path,
            vapor_shared::constants::config::KEY_AUTO_LAUNCH,
        );
        if matches!(
            auto_launch.as_ref().map(|value| value.as_deref()),
            Ok(Some("false"))
        ) {
            return ServiceState::Off;
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            // The service is in place with both jobs: the daemon's, and
            // the supervisor's that restarts it. A daemon binary that
            // is missing, or a service manager that gives no answer,
            // reads as not installed: the install is what then says
            // what is wrong.
            let registered = |status: Result<vapor_platform::ServiceStatus, _>| {
                status.is_ok_and(|status| status != vapor_platform::ServiceStatus::NotInstalled)
            };
            let daemon = locate_daemon_binary()
                .and_then(|daemon| service_cmd::build_native(self.config_path.clone(), daemon).ok())
                .is_some_and(|(_, installer)| {
                    registered(vapor_platform::ServiceInstaller::status(installer.as_ref()))
                });
            let supervisor = service_cmd::supervisor_installer().is_ok_and(|supervisor| {
                registered(vapor_platform::ServiceInstaller::status(&supervisor))
            });
            let installed = daemon && supervisor;
            if installed {
                ServiceState::Installed
            } else {
                ServiceState::NotInstalled
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            ServiceState::Unavailable(
                "Vapor has no background service on this platform yet; `vapor run` runs the daemon"
                    .to_string(),
            )
        }
    }

    fn install_service(&mut self, linger: Option<bool>) -> Result<Option<String>, String> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let daemon = locate_daemon_binary()
                .ok_or_else(|| "vapord binary not found near the running CLI".to_string())?;
            let (manager, installer) = service_cmd::build_native(self.config_path.clone(), daemon)
                .map_err(|e| e.to_string())?;
            let supervisor = service_cmd::supervisor_installer().map_err(|e| e.to_string())?;
            let outcome = service_cmd::install_with_supervisor(
                ServiceCommand::Install,
                &manager,
                installer.as_ref(),
                &supervisor,
                running_daemon_build().as_deref(),
                Instant::now(),
            )
            .map_err(|e| e.to_string())?;
            if matches!(
                outcome,
                service_cmd::ServiceCommandOutcome::Action(
                    vapor_lifecycle::DaemonLifecycleActionResult::RelaunchDeferred(_)
                )
            ) {
                // A crash-loop pause holds the start; the line names
                // the command that clears it.
                return Err(service_cmd::render_text(&outcome));
            }
            #[cfg(target_os = "linux")]
            {
                Ok(linger_note(&service_cmd::apply_linger(linger)))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = linger;
                Ok(None)
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = linger;
            Err("Vapor has no background service on this platform yet".to_string())
        }
    }

    fn decline_service(&mut self) -> Result<(), String> {
        config_cmd::set(
            &self.config_path,
            vapor_shared::constants::config::KEY_AUTO_LAUNCH,
            "false",
        )
        .map_err(|e| e.to_string())
    }

    fn note(&mut self, line: &str) {
        eprintln!("{line}");
    }
}

fn dispatch_init(
    request: init_cmd::InitRequest,
    token: Option<String>,
    json: bool,
) -> Result<ExitCode, String> {
    use std::io::IsTerminal;
    if token.as_deref().is_some_and(|value| value != "-") {
        return Err(
            "a token on the command line is visible in the process list and the shell \
             history; pipe it on stdin instead: `<command that prints the token> | vapor init \
             --token -`"
                .to_string(),
        );
    }
    let path = resolve_configuration_path();
    // A person at a terminal is asked; a script and `--json` never are.
    let interactive = !json && std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let (store, fallback_reason) = auth_cmd::build_native_store();
    let mut prompter = profiles_cmd::TerminalPrompter;
    let mut host = NativeInitHost {
        interactive,
        token_on_stdin: token.is_some(),
        config_path: path.clone(),
    };
    let mut report = init_cmd::run(
        &path,
        &profiles_cmd::ProfileStorage::of_this_install(),
        &init_cmd::Secrets {
            store: store.as_ref(),
            problem: fallback_reason.as_deref(),
        },
        &request,
        interactive.then_some(&mut prompter as &mut dyn profiles_cmd::Prompter),
        &mut host,
        std::time::SystemTime::now(),
    )
    .map_err(|e| e.to_string())?;

    let step_state = |name: &str| {
        report
            .steps
            .iter()
            .find(|step| step.step == name)
            .map(|step| step.state)
    };
    use vapor_shared::constants::init as names;
    // A daemon that this run started needs a moment before it answers,
    // and a daemon that was up before the profile existed needs one to
    // read the configuration again. Until then it says that there is
    // no profile, which is not what this run leaves behind. With no
    // daemon to wait for (the service is off, or it did not start),
    // one look is enough.
    let has_profile = !report.profiles.is_empty();
    let started_here = step_state(names::STEP_SERVICE) == Some(names::STATE_DONE);
    let started = std::time::Instant::now();
    loop {
        let status = ipc_cmd::status().ok();
        let wait = match &status {
            None if started_here => Duration::from_secs(3),
            None => Duration::ZERO,
            Some(status)
                if has_profile
                    && status.run.state == vapor_shared::constants::run_state::UNCONFIGURED =>
            {
                Duration::from_secs(5)
            }
            Some(_) => Duration::ZERO,
        };
        if let Some(status) = status {
            report.daemon = Some(init_cmd::DaemonLine {
                state: status.run.state,
                reason: status.run.reason,
            });
        }
        if started.elapsed() >= wait {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", report.render_text());
    }
    Ok(if report.initialized {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn dispatch_profiles(action: ProfilesAction) -> Result<ExitCode, String> {
    use std::io::IsTerminal;
    let path = resolve_configuration_path();
    let print_json = |value: serde_json::Value| -> Result<(), String> {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
        );
        Ok(())
    };
    match action {
        ProfilesAction::List { json } => {
            let report = profiles_cmd::list(&path);
            if json {
                print_json(serde_json::to_value(&report).map_err(|e| e.to_string())?)?;
            } else {
                print!("{}", report.render_text());
            }
            Ok(ExitCode::SUCCESS)
        }
        ProfilesAction::Add {
            id,
            name,
            provider,
            local,
            cloud,
            mode,
            json,
        } => {
            let mut request = profiles_cmd::AddRequest {
                id,
                name,
                provider,
                local,
                cloud,
                mode,
            };
            // A person at a terminal is asked; a script, the app, and
            // `--json` never are.
            let interactive =
                !json && std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
            let mut prompter = profiles_cmd::TerminalPrompter;
            if interactive {
                let taken = profiles_cmd::profile_ids(&path);
                request = profiles_cmd::ask_for_missing(request, &taken, &mut prompter)
                    .map_err(|e| e.to_string())?;
            }
            let outcome = profiles_cmd::add(
                &path,
                &profiles_cmd::ProfileStorage::of_this_install(),
                &request,
                interactive.then_some(&mut prompter as &mut dyn profiles_cmd::Prompter),
                std::time::SystemTime::now(),
            )
            .map_err(|e| e.to_string())?;
            if json {
                print_json(serde_json::to_value(&outcome).map_err(|e| e.to_string())?)?;
            } else {
                print!("{}", outcome.render_text());
            }
            Ok(ExitCode::SUCCESS)
        }
        ProfilesAction::Remove { id, yes, json } => {
            let interactive = !json && std::io::stdin().is_terminal();
            if !yes && !interactive {
                return Err(
                    "removing a profile needs a confirmation; pass --yes to give it".to_string(),
                );
            }
            // A daemon holds the lock of its runtime directory for its
            // whole life, whether or not it answers on the socket. The
            // lock stays held here until the removal is over, so no
            // daemon starts in the middle of it.
            let daemon_lock =
                vapor_daemon::singleton::SingletonLock::acquire_for_current_vapor_dir();
            let daemon_running = match &daemon_lock {
                Ok(_) => false,
                Err(vapor_daemon::singleton::SingletonLockError::AlreadyRunning(_)) => true,
                Err(error) => {
                    return Err(format!("cannot tell whether a daemon is running: {error}"));
                }
            };
            let open_secrets = || -> std::sync::Arc<dyn vapor_platform::SecretStore> {
                std::sync::Arc::from(auth_cmd::build_native_store().0)
            };
            let mut confirm = |profile: &profiles_cmd::ProfileRow| {
                if yes {
                    return Ok(true);
                }
                profiles_cmd::Prompter::confirm(
                    &mut profiles_cmd::TerminalPrompter,
                    &profiles_cmd::remove_question(profile),
                )
            };
            let outcome = profiles_cmd::remove(
                &path,
                &profiles_cmd::ProfileStorage::of_this_install(),
                &id,
                daemon_running,
                &open_secrets,
                &mut confirm,
            )
            .map_err(|e| e.to_string())?;
            if json {
                print_json(serde_json::to_value(&outcome).map_err(|e| e.to_string())?)?;
            } else {
                print!("{}", outcome.render_text());
            }
            Ok(ExitCode::SUCCESS)
        }
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
    let trash_root = vapor_shared::runtime_paths::trash_directory();
    match action {
        TrashAction::List { json } => {
            let report = trash_cmd::list_trash(&loaded.config, &trash_root);
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
            let report = trash_cmd::restore(&loaded.config, &trash_root, &id, profile.as_deref())?;
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
        TrashAction::Empty { profile, yes, json } => {
            // The trash can be the last copy of a file. Emptying it is
            // the one command here that cannot be undone, so it never
            // runs on a guess: a person confirms, or the caller says yes.
            if !yes {
                use std::io::IsTerminal;
                if json || !std::io::stdin().is_terminal() {
                    return Err(
                        "emptying the trash deletes its files for good; pass --yes to confirm"
                            .to_string(),
                    );
                }
                let (count, size) =
                    trash_cmd::empty_preview(&loaded.config, &trash_root, profile.as_deref());
                if count == 0 {
                    println!("The trash is empty.");
                    return Ok(ExitCode::SUCCESS);
                }
                let confirmed = profiles_cmd::Prompter::confirm(
                    &mut profiles_cmd::TerminalPrompter,
                    &trash_cmd::empty_question(count, size),
                )
                .map_err(|e| e.to_string())?;
                if !confirmed {
                    println!("Nothing was removed.");
                    return Ok(ExitCode::SUCCESS);
                }
            }
            let report = trash_cmd::empty(&loaded.config, &trash_root, profile.as_deref());
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

fn dispatch_report(
    output: Option<std::path::PathBuf>,
    keep_paths: bool,
    json: bool,
) -> Result<ExitCode, String> {
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
    let default_root = vapor_dir.join(vapor_shared::constants::runtime::REPORTS_DIRECTORY_NAME);
    let output_root = output.unwrap_or_else(|| default_root.clone());
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .map_err(|_| "system clock is before the Unix epoch".to_string())?;
    let paths = if keep_paths {
        support::Paths::Keep
    } else {
        support::Paths::Redact
    };
    let report = support::write_support_report(&vapor_dir, &output_root, live, timestamp_ms, paths)
        .map_err(|e| format!("cannot write the support report: {e}"))?;
    // Only the directory Vapor owns is pruned; one the user named is
    // theirs to keep in order.
    if output_root == default_root {
        support::prune_old_reports(&default_root);
    }

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
        eprintln!("{command}: not accepted; {}", ack.note);
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
        if timeline.truncated {
            println!("(older events are left out: the timeline is too long for one answer)");
        }
        for entry in &timeline.entries {
            println!(
                "[{}] {}: {}",
                vapor_cli::commands::when::clock(entry.timestamp_ms),
                entry.kind,
                entry.message
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
            // Google Drive without `--token -` runs the full
            // OAuth-PKCE browser flow, but only interactively. When stdin
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
                auth_cmd::logout_target(&config, profile.as_deref()).map_err(|e| e.to_string())?;
            let removed =
                auth_cmd::logout_from(store.as_ref(), &profile).map_err(|e| e.to_string())?;
            if removed {
                println!("Signed profile {profile} out.");
            } else {
                println!("Profile {profile} was not signed in.");
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

/// Reads the login token from stdin: one line, with `--token -` or with
/// no flag when stdin is not a terminal. A value given to `--token` is
/// refused and never stored: an argument is visible in the process list
/// and stays in the shell history.
fn resolve_auth_token(token: Option<String>) -> Result<String, String> {
    if let Some(value) = token.as_deref()
        && value != "-"
    {
        return Err(
            "a token on the command line is visible in the process list and the shell \
             history; pipe it on stdin instead: `<command that prints the token> | vapor auth \
             login --token -`"
                .to_string(),
        );
    }
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

fn dispatch_service(action: ServiceAction) -> Result<ExitCode, String> {
    let mut check_loop: Option<Duration> = None;
    #[cfg(target_os = "linux")]
    let mut linger_choice: Option<bool> = None;
    let (command, json) = match action {
        ServiceAction::Bootstrap { json } => (ServiceCommand::Bootstrap, json),
        ServiceAction::Install {
            json,
            #[cfg(target_os = "linux")]
            linger,
        } => {
            #[cfg(target_os = "linux")]
            {
                linger_choice = linger.map(Switch::is_on);
            }
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
            // A supervisor that starts after an update is the new
            // build: a daemon of the old one still runs, and is replaced
            // here. The supervisor of the old build ended itself when
            // it saw its binary replaced, which is what started this one.
            // An error here (a daemon slow to exit) does not end the
            // supervisor: the replacement left the daemon meant to run,
            // and the ticks below start it.
            if let Err(error) = service_cmd::replace_daemon_of_another_build(
                &manager,
                installer.as_ref(),
                running_daemon_build().as_deref(),
                Instant::now(),
            ) {
                eprintln!("vapor: warning: replacing the daemon of another build: {error}");
            }
            let binary = vapor_cli::commands::daemon_binary::cli_path();
            let started_as = binary.as_deref().map(service_cmd::BinaryStamp::of);
            let updated = || match (&binary, &started_as) {
                (Some(path), Some(stamp)) => stamp.replaced_by(&service_cmd::BinaryStamp::of(path)),
                _ => false,
            };
            service_cmd::check_loop(
                &manager,
                installer.as_ref(),
                service_cmd::SupervisorWatch::every(interval),
                json,
                &stop,
                &updated,
                &|| ipc_cmd::daemon_answers(),
            )
            .map_err(|e| e.to_string())?;
            return Ok(ExitCode::SUCCESS);
        }
        let supervisor = service_cmd::supervisor_installer().map_err(|e| e.to_string())?;
        let mut outcome = if matches!(command, ServiceCommand::Bootstrap | ServiceCommand::Install)
        {
            service_cmd::install_with_supervisor(
                command,
                &manager,
                installer.as_ref(),
                &supervisor,
                running_daemon_build().as_deref(),
                Instant::now(),
            )
        } else {
            service_cmd::dispatch(command, &manager, installer.as_ref(), Instant::now())
        }
        .map_err(|e| e.to_string())?;
        match &mut outcome {
            service_cmd::ServiceCommandOutcome::Status(report) => {
                report.supervisor_installed = vapor_platform::ServiceInstaller::status(&supervisor)
                    .map(|status| status != vapor_platform::ServiceStatus::NotInstalled)
                    .unwrap_or(false);
                if report.status == vapor_platform::ServiceStatus::Running {
                    report.answers = Some(ipc_cmd::daemon_answers());
                }
            }
            // A start is done when a daemon answers, not when the
            // service manager took the request: a daemon that hangs
            // keeps its job, and a start against it changes nothing.
            service_cmd::ServiceCommandOutcome::Action(result)
                if matches!(command, ServiceCommand::Start | ServiceCommand::Restart)
                    && matches!(
                        result,
                        vapor_lifecycle::DaemonLifecycleActionResult::Started
                            | vapor_lifecycle::DaemonLifecycleActionResult::Unchanged
                    )
                    && !daemon_answers_within(Duration::from_secs(10)) =>
            {
                return Err(
                    "the daemon's job runs, but the daemon gives no answer on its socket, so \
                     nothing syncs. `vapor service restart` ends it and starts a new one; \
                     `vapor logs` shows what it did last"
                        .to_string(),
                );
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
        #[cfg(target_os = "linux")]
        if command == ServiceCommand::Install {
            // On stderr: stdout carries the answer a surface parses.
            if let Some(note) = linger_note(&service_cmd::apply_linger(linger_choice)) {
                eprintln!("vapor: {note}");
            }
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
        let _ = (command, json, check_loop);
        Err(
            "`vapor service` needs a native service manager; Windows lands with that surface"
                .to_string(),
        )
    }
}

/// Whether a daemon answers on the socket within `wait`: a daemon that
/// was just started needs a moment to open it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn daemon_answers_within(wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if ipc_cmd::daemon_answers() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The build of the daemon that answers on the socket, or `None` when
/// none answers. A daemon that refuses the handshake speaks another IPC
/// version, so it is another build too.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn running_daemon_build() -> Option<String> {
    match ipc_cmd::status() {
        Ok(status) => Some(status.daemon),
        Err(vapor_cli::commands::IpcCliError::Client(
            vapor_ipc::ClientError::IncompatibleVersion(_),
        )) => Some("another IPC version".to_string()),
        Err(_) => None,
    }
}

/// What to tell the user about lingering after an install, when there
/// is something to tell.
#[cfg(target_os = "linux")]
fn linger_note(outcome: &service_cmd::LingerOutcome) -> Option<String> {
    match outcome {
        service_cmd::LingerOutcome::On => {
            Some("lingering is on, so the daemon keeps running after you log out".to_string())
        }
        service_cmd::LingerOutcome::Off => None,
        service_cmd::LingerOutcome::Refused(reason) => Some(format!(
            "warning: the system did not turn lingering on ({reason}); the daemon stops when \
             you log out. An administrator can turn it on: sudo loginctl enable-linger $USER"
        )),
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
