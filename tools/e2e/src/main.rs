//! `vapor-e2e`: the Tier E2E command. `scripts/e2e.sh` wraps it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use vapor_e2e::daemon::{Daemon, DaemonKind};
use vapor_e2e::host::{Host, Provider};
use vapor_e2e::oracle::{OracleOptions, verify_trees};
use vapor_e2e::runner::{self, RunOptions};
use vapor_e2e::sandbox::Sandbox;
use vapor_e2e::scenario::{Ctx, RunPaths};
use vapor_e2e::scenarios;

#[derive(Parser, Debug)]
#[command(name = "vapor-e2e", about = "Vapor end-to-end verification harness")]
struct Args {
    /// Repository root (default: discovered from the executable path).
    #[arg(long, global = true)]
    repo_root: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProviderArg {
    Filesystem,
    Gdrive,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DaemonArg {
    /// `vapor run`: the daemon composed inside the CLI (default).
    VaporRun,
    /// The shipped `vapord` binary.
    Vapord,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the scenario suite.
    Run {
        /// Scenario ids or names to run (repeatable, comma-separated).
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Preserve every sandbox after a green run.
        #[arg(long)]
        keep: bool,
        /// Reuse target/debug/vapor and vapord instead of building.
        #[arg(long)]
        skip_build: bool,
        /// Include the host-mutating service round-trip (installs a
        /// real LaunchAgent; disposable runners only).
        #[arg(long)]
        full: bool,
        /// Cloud provider the run uses.
        #[arg(long, value_enum, default_value_t = ProviderArg::Filesystem)]
        provider: ProviderArg,
        /// Which daemon flavour scenarios start.
        #[arg(long, value_enum, default_value_t = DaemonArg::VaporRun)]
        daemon: DaemonArg,
        /// Write the JSON report here (default: inside the run root).
        #[arg(long)]
        json: Option<PathBuf>,
    },
    /// List scenarios with their needs.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Provision a sandbox with a running daemon for manual work.
    Sandbox {
        #[arg(long)]
        skip_build: bool,
        #[arg(long, value_enum, default_value_t = DaemonArg::VaporRun)]
        daemon: DaemonArg,
    },
    /// Stop daemons of manual sandboxes and remove them.
    SandboxStop {
        /// A sandbox directory; default: every `sbx-*` under .vapor/e2e.
        path: Option<PathBuf>,
    },
    /// Compare two roots with the tree oracle.
    VerifyTrees {
        local: PathBuf,
        cloud: PathBuf,
        /// Extra ignore rule (repeatable).
        #[arg(long)]
        ignore: Vec<String>,
    },
}

fn main() -> ExitCode {
    let args = Args::parse();
    let repo_root = match args
        .repo_root
        .clone()
        .map(Ok)
        .unwrap_or_else(runner::discover_repo_root)
    {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[e2e] {error}");
            return ExitCode::from(2);
        }
    };
    match args.command {
        Cmd::Run {
            only,
            keep,
            skip_build,
            full,
            provider,
            daemon,
            json,
        } => {
            let options = RunOptions {
                repo_root,
                only,
                keep,
                skip_build,
                full,
                provider: match provider {
                    ProviderArg::Filesystem => Provider::Filesystem,
                    ProviderArg::Gdrive => Provider::Gdrive,
                },
                daemon_kind: match daemon {
                    DaemonArg::VaporRun => DaemonKind::CliRun,
                    DaemonArg::Vapord => DaemonKind::Vapord,
                },
                json_path: json,
            };
            match runner::run(&options) {
                Ok(report) if report.is_green() => ExitCode::SUCCESS,
                Ok(_) => ExitCode::from(1),
                Err(error) => {
                    eprintln!("[e2e] {error}");
                    ExitCode::from(2)
                }
            }
        }
        Cmd::List { json } => {
            let all = scenarios::all();
            if json {
                let rows: Vec<serde_json::Value> = all
                    .iter()
                    .map(|scenario| {
                        serde_json::json!({
                            "id": scenario.id,
                            "name": scenario.name,
                            "proves": scenario.proves,
                            "needs": scenario.needs.iter().map(|need| need.label()).collect::<Vec<_>>(),
                            "known_gap": match scenario.expect {
                                vapor_e2e::scenario::Expect::Pass => serde_json::Value::Null,
                                vapor_e2e::scenario::Expect::KnownGap(task) => serde_json::Value::String(task.to_string()),
                            },
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&rows).expect("serializable")
                );
            } else {
                for scenario in all {
                    let needs: Vec<&str> = scenario.needs.iter().map(|need| need.label()).collect();
                    let gap = match scenario.expect {
                        vapor_e2e::scenario::Expect::Pass => String::new(),
                        vapor_e2e::scenario::Expect::KnownGap(task) => {
                            format!("  [known gap: {task}]")
                        }
                    };
                    println!(
                        "{} {:<34} {}{}{}",
                        scenario.id,
                        scenario.name,
                        scenario.proves,
                        if needs.is_empty() {
                            String::new()
                        } else {
                            format!("  (needs: {})", needs.join(", "))
                        },
                        gap
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Cmd::Sandbox { skip_build, daemon } => match manual_sandbox(&repo_root, skip_build, daemon)
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("[e2e] {error}");
                ExitCode::from(1)
            }
        },
        Cmd::SandboxStop { path } => match sandbox_stop(&repo_root, path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("[e2e] {error}");
                ExitCode::from(1)
            }
        },
        Cmd::VerifyTrees {
            local,
            cloud,
            ignore,
        } => {
            let options = OracleOptions {
                extra_ignore_rules: ignore,
                compare_mode: cfg!(unix),
            };
            match verify_trees(&local, &cloud, &options) {
                Ok(report) => {
                    println!("{}", report.summary(50));
                    for skipped in &report.skipped {
                        println!("skipped special file: {skipped}");
                    }
                    if report.is_clean() {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(1)
                    }
                }
                Err(error) => {
                    eprintln!("[e2e] {error}");
                    ExitCode::from(2)
                }
            }
        }
    }
}

/// Builds, provisions `sbx-<id>`, configures the scope, starts a
/// daemon, prints a cheat-sheet, and leaves the daemon running. The PID
/// is written to `<sandbox>/daemon.pid` so `sandbox-stop` can find it.
fn manual_sandbox(
    repo_root: &Path,
    skip_build: bool,
    daemon: DaemonArg,
) -> Result<(), vapor_e2e::Failure> {
    if !skip_build {
        runner::build_product(repo_root)?;
    }
    let kind = match daemon {
        DaemonArg::VaporRun => DaemonKind::CliRun,
        DaemonArg::Vapord => DaemonKind::Vapord,
    };
    let paths: RunPaths = runner::product_paths(repo_root, kind)?;
    let root = runner::e2e_root(repo_root).join(runner::new_run_id("sbx"));
    let sandbox = Sandbox::create(&root)?;
    let host = Host::detect(&root, false, Provider::Filesystem);
    let ctx = Ctx::new(host, paths.clone(), sandbox)?;
    let home = ctx.primary.clone();
    ctx.configure_scope(&home)?;
    let daemon = Daemon::spawn(kind, &paths.cli_bin, &paths.vapord_bin, &home, &root)?;
    let pid = daemon.pid();
    // The daemon outlives this process on purpose.
    std::mem::forget(daemon);
    fs::write(root.join("daemon.pid"), pid.to_string())?;
    ctx.cli()
        .wait_run_state("Running", vapor_e2e::scenario::STARTUP_TIMEOUT)?;
    println!(
        "[e2e] sandbox ready — daemon running (pid {pid})

  export VAPOR_DIR=\"{home_dir}\"
  vapor={cli}

  watched local root:   {local}
  cloud root:           {cloud}
  daemon log:           {log}
  state DB (read-only): sqlite3 -readonly \"{db}\" 'SELECT * FROM queue_intents;'

  $vapor status --json          live daemon state
  $vapor logs --tail 50         recent daemon log lines
  $vapor pause / resume         flip work admission
  $vapor flush-now              hint a queue drain
  $vapor reconcile              request a whole-scope reconcile
  $vapor doctor                 sanity checks
  echo hi > \"{local}/f.txt\"    feed the watcher a change
  {e2e} verify-trees \"{local}\" \"{cloud}\"   compare the two trees

  stop daemon + remove sandbox:  {e2e} sandbox-stop \"{root}\"
  (or ./scripts/clean.sh for all of .vapor)",
        home_dir = home.dir.display(),
        cli = paths.cli_bin.display(),
        local = home.local.display(),
        cloud = home.cloud.display(),
        log = home.daemon_log().display(),
        db = home.state_db().display(),
        e2e = std::env::current_exe()
            .map(|exe| exe.display().to_string())
            .unwrap_or_else(|_| "vapor-e2e".to_string()),
        root = root.display(),
    );
    Ok(())
}

fn sandbox_stop(repo_root: &Path, path: Option<PathBuf>) -> Result<(), vapor_e2e::Failure> {
    let targets: Vec<PathBuf> = match path {
        Some(path) => vec![path],
        None => {
            let root = runner::e2e_root(repo_root);
            fs::read_dir(&root)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("sbx-"))
                })
                .collect()
        }
    };
    if targets.is_empty() {
        println!("[e2e] no manual sandbox to stop");
        return Ok(());
    }
    for target in targets {
        let pid_file = target.join("daemon.pid");
        if let Ok(text) = fs::read_to_string(&pid_file)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            terminate_pid(pid);
            println!("[e2e] stopped daemon pid {pid} of {}", target.display());
        }
        vapor_e2e::sandbox::remove_tree(&target)?;
        println!("[e2e] removed {}", target.display());
    }
    Ok(())
}

fn terminate_pid(pid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: pid came from a file this tool wrote; a stale pid at
        // worst signals a process we do not own, which the kernel refuses
        // for another user and which we accept as a best-effort cleanup
        // for the same user.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        for _ in 0..40 {
            // SAFETY: signal 0 only probes existence.
            if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        // SAFETY: as above.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status();
    }
}
