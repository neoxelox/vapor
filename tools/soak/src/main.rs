//! `vapor-soak`: the Tier S command. `scripts/soak.sh` wraps it.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use vapor_e2e::daemon::DaemonKind;
use vapor_e2e::diskimage::ImageFs;
use vapor_e2e::runner;
use vapor_soak::driver::{Config, Driver, ThrottleMode};
use vapor_soak::faults::FaultPlan;
use vapor_soak::model::SyncMode;
use vapor_soak::workload::LoadShape;

#[derive(Parser, Debug)]
#[command(
    name = "vapor-soak",
    about = "Vapor long-run workload and model-checked oracle"
)]
struct Args {
    /// Repository root (default: discovered from the executable path).
    #[arg(long, global = true)]
    repo_root: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ModeArg {
    TwoWay,
    PullOnly,
    PushOnly,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ThrottleArg {
    /// Neutral inputs for the whole run.
    Static,
    /// The driver walks the daemon through every throttle state.
    Walk,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DaemonArg {
    VaporRun,
    Vapord,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ImageFsArg {
    Apfs,
    ApfsCaseSensitive,
    Exfat,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run a soak.
    Run {
        /// Planned length, e.g. `30m`, `2h`, `90s`.
        #[arg(long, default_value = "30m")]
        duration: String,
        /// Workload seed; the same seed replays the same operations.
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long, value_enum, default_value_t = ModeArg::TwoWay)]
        mode: ModeArg,
        /// Load shape: mixed, trickle, coding, bulk, large.
        #[arg(long, default_value = "mixed")]
        load: String,
        /// Faults: none, crash, all, or a comma-separated list
        /// (crash, crash-mid-transfer, freeze, pause-resume,
        /// cloud-root-vanish, throttle-walk, config-reload, disk-full).
        #[arg(long, default_value = "none")]
        faults: String,
        #[arg(long, value_enum, default_value_t = ThrottleArg::Static)]
        throttle: ThrottleArg,
        #[arg(long, value_enum, default_value_t = DaemonArg::VaporRun)]
        daemon: DaemonArg,
        /// Put the cloud root on a throwaway disk image of this size
        /// (macOS; needed for disk-full and case-sensitive cells).
        #[arg(long)]
        cloud_image_mb: Option<u32>,
        #[arg(long, value_enum, default_value_t = ImageFsArg::Apfs)]
        cloud_image_fs: ImageFsArg,
        #[arg(long)]
        skip_build: bool,
        /// Build and run the release profile (what users get; the CPU
        /// budget only means something against it).
        #[arg(long)]
        release: bool,
        /// Preserve the sandbox after a clean run.
        #[arg(long)]
        keep: bool,
        /// Record violations and keep going instead of freezing.
        #[arg(long)]
        continue_on_violation: bool,
        /// Longest a phase may take to converge, e.g. `20m`.
        #[arg(long, default_value = "20m")]
        converge_deadline: String,
    },
    /// Re-run the oracle on a preserved sandbox.
    Verify { sandbox: PathBuf },
    /// Print a run's status file in one line.
    Status { status_file: PathBuf },
}

fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let (number, unit) = text.split_at(
        text.trim_end_matches(|c: char| c.is_ascii_alphabetic())
            .len(),
    );
    let value: f64 = number
        .parse()
        .map_err(|_| format!("bad duration {text:?}; use 90s, 30m, 2h"))?;
    let seconds = match unit {
        "s" | "" => value,
        "m" => value * 60.0,
        "h" => value * 3600.0,
        _ => return Err(format!("bad duration unit in {text:?}; use s, m, h")),
    };
    Ok(Duration::from_secs_f64(seconds))
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
            eprintln!("[soak] {error}");
            return ExitCode::from(2);
        }
    };
    match args.command {
        Cmd::Run {
            duration,
            seed,
            mode,
            load,
            faults,
            throttle,
            daemon,
            cloud_image_mb,
            cloud_image_fs,
            skip_build,
            release,
            keep,
            continue_on_violation,
            converge_deadline,
        } => {
            let duration = match parse_duration(&duration) {
                Ok(d) => d,
                Err(error) => {
                    eprintln!("[soak] {error}");
                    return ExitCode::from(2);
                }
            };
            let converge_deadline = match parse_duration(&converge_deadline) {
                Ok(d) => d,
                Err(error) => {
                    eprintln!("[soak] {error}");
                    return ExitCode::from(2);
                }
            };
            let Some(load) = LoadShape::named(&load) else {
                eprintln!(
                    "[soak] unknown load {load:?}; one of {}",
                    LoadShape::known_names().join(", ")
                );
                return ExitCode::from(2);
            };
            let faults = match FaultPlan::parse(&faults) {
                Ok(plan) => plan,
                Err(error) => {
                    eprintln!("[soak] {error}");
                    return ExitCode::from(2);
                }
            };
            let cfg = Config {
                repo_root,
                seed,
                duration,
                mode: match mode {
                    ModeArg::TwoWay => SyncMode::TwoWay,
                    ModeArg::PullOnly => SyncMode::PullOnly,
                    ModeArg::PushOnly => SyncMode::PushOnly,
                },
                load,
                faults,
                throttle: match throttle {
                    ThrottleArg::Static => ThrottleMode::Static,
                    ThrottleArg::Walk => ThrottleMode::Walk,
                },
                daemon_kind: match daemon {
                    DaemonArg::VaporRun => DaemonKind::CliRun,
                    DaemonArg::Vapord => DaemonKind::Vapord,
                },
                skip_build,
                release,
                keep,
                continue_on_violation,
                status_interval: Duration::from_secs(5),
                cloud_image_mb,
                cloud_image_fs: match cloud_image_fs {
                    ImageFsArg::Apfs => ImageFs::Apfs,
                    ImageFsArg::ApfsCaseSensitive => ImageFs::ApfsCaseSensitive,
                    ImageFsArg::Exfat => ImageFs::ExFat,
                },
                converge_deadline,
            };
            let mut driver = match Driver::provision(cfg) {
                Ok(driver) => driver,
                Err(error) => {
                    eprintln!("[soak] provisioning failed: {error}");
                    return ExitCode::from(2);
                }
            };
            match driver.run() {
                Ok(report) if report.violations.is_empty() => ExitCode::SUCCESS,
                Ok(_) => ExitCode::from(1),
                Err(_) => ExitCode::from(1),
            }
        }
        Cmd::Verify { sandbox } => match vapor_soak::driver::verify(&sandbox) {
            Ok(violations) if violations.is_empty() => {
                println!("[soak] clean: both trees hold what the model expects");
                ExitCode::SUCCESS
            }
            Ok(violations) => {
                for violation in &violations {
                    println!(
                        "[soak] {} {}: {}",
                        violation.kind, violation.path, violation.detail
                    );
                }
                ExitCode::from(1)
            }
            Err(error) => {
                eprintln!("[soak] {error}");
                ExitCode::from(2)
            }
        },
        Cmd::Status { status_file } => {
            match std::fs::read_to_string(&status_file)
                .ok()
                .and_then(|text| serde_json::from_str::<vapor_soak::report::Status>(&text).ok())
            {
                Some(status) => {
                    println!(
                        "{:?} phase {} {} | {} ops, {} files, {} faults, {} violations | {:.0}/{:.0}s | rss {} MiB cpu {:.1}% | {}",
                        status.state,
                        status.phase_index,
                        status.phase_name,
                        status.ops_done,
                        status.files_live,
                        status.faults_injected,
                        status.violations_total,
                        status.elapsed_seconds,
                        status.planned_seconds,
                        status
                            .health_latest
                            .as_ref()
                            .map(|h| h.rss_bytes / (1024 * 1024))
                            .unwrap_or(0),
                        status.health.cpu_avg_percent,
                        status.message
                    );
                    ExitCode::SUCCESS
                }
                None => {
                    eprintln!("[soak] cannot read {}", status_file.display());
                    ExitCode::from(2)
                }
            }
        }
    }
}
