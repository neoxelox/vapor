//! Runs the scenario list: builds the product binaries, provisions one
//! sandbox per scenario, applies the `needs` filter, runs the body and
//! the epilogue, and writes the report. A failed scenario keeps its
//! sandbox and prints diagnostics; a green run removes everything
//! unless asked to keep it.

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::daemon::DaemonKind;
use crate::host::{Host, Need, Provider};
use crate::report::{HostSummary, RunReport, ScenarioResult, Summary, Verdict, console_line};
use crate::sandbox::{self, Sandbox};
pub use crate::scenario::RunPaths;

use crate::scenario::{Ctx, Expect, Scenario};
use crate::{Failure, scenarios};

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub repo_root: PathBuf,
    /// Only these scenario ids (empty = all).
    pub only: Vec<String>,
    pub keep: bool,
    pub skip_build: bool,
    pub full: bool,
    pub provider: Provider,
    pub daemon_kind: DaemonKind,
    /// Where to write the JSON report (default: inside the run root).
    pub json_path: Option<PathBuf>,
    /// Scenarios run at once. Each has its own sandbox, runtime
    /// directory, and socket, so they are independent; what they share
    /// is the host's CPU and disk, which the deadline scale accounts
    /// for. Scenarios that mount disk images run one at a time among
    /// themselves, and the host-mutating service round-trip runs alone
    /// after everything else.
    pub jobs: usize,
}

/// One scenario per core. A scenario is mostly a daemon waiting on its
/// own timers, so the host stays far from saturated: twelve at once
/// on twelve cores used a third of the CPU and moved no scenario's
/// clock.
pub fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|cores| cores.get())
        .unwrap_or(1)
        .max(1)
}

/// Deadlines grow by half per extra sandbox sharing the host.
fn deadline_scale_percent(jobs: usize) -> u32 {
    let extra = u32::try_from(jobs.saturating_sub(1)).unwrap_or(u32::MAX);
    100u32.saturating_add(extra.saturating_mul(50)).min(400)
}

pub fn e2e_root(repo_root: &Path) -> PathBuf {
    repo_root.join(".vapor").join("e2e")
}

/// Short run id: keeps every sandbox path under the Unix socket-address
/// budget so scenarios can assert canonical socket placement.
pub fn new_run_id(prefix: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{prefix}-{:06}-{}",
        now % 1_000_000,
        std::process::id() % 100_000
    )
}

/// Builds `vapor` and `vapord` in debug mode.
pub fn build_product(repo_root: &Path) -> Result<(), Failure> {
    build_product_profile(repo_root, false)
}

/// Builds `vapor` and `vapord`; `release` selects the optimized
/// profile the shipped binaries use (the soak's SLO cells need it,
/// since a debug daemon burns CPU no user would see).
pub fn build_product_profile(repo_root: &Path, release: bool) -> Result<(), Failure> {
    println!(
        "[e2e] building vapor + vapord (cargo build{} -p vapor-cli -p vapor-daemon)",
        if release { " --release" } else { "" }
    );
    let mut command = Command::new("cargo");
    command.args(["build", "--quiet", "-p", "vapor-cli", "-p", "vapor-daemon"]);
    if release {
        command.arg("--release");
    }
    let status = command
        .arg("--manifest-path")
        .arg(repo_root.join("Cargo.toml"))
        .status()
        .map_err(|error| Failure::new(format!("cannot run cargo: {error}")))?;
    if !status.success() {
        return Err(Failure::new("cargo build of vapor + vapord failed"));
    }
    Ok(())
}

pub fn product_paths(repo_root: &Path, daemon_kind: DaemonKind) -> Result<RunPaths, Failure> {
    product_paths_profile(repo_root, daemon_kind, false)
}

pub fn product_paths_profile(
    repo_root: &Path,
    daemon_kind: DaemonKind,
    release: bool,
) -> Result<RunPaths, Failure> {
    // The same directory cargo just built into, so a run with
    // `CARGO_TARGET_DIR` set (a Linux container over a macOS checkout)
    // finds its own binaries and not the host's.
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root.join("target"));
    let debug = target.join(if release { "release" } else { "debug" });
    let exe = |name: &str| {
        if cfg!(windows) {
            debug.join(format!("{name}.exe"))
        } else {
            debug.join(name)
        }
    };
    let cli_bin = exe("vapor");
    let vapord_bin = exe("vapord");
    if !cli_bin.is_file() {
        return Err(Failure::new(format!(
            "vapor binary missing at {} (run without --skip-build, or build that profile)",
            cli_bin.display()
        )));
    }
    if !vapord_bin.is_file() {
        return Err(Failure::new(format!(
            "vapord binary missing at {} (doctor's sibling probe and the daemon scenarios need it)",
            vapord_bin.display()
        )));
    }
    Ok(RunPaths {
        repo_root: repo_root.to_path_buf(),
        cli_bin,
        vapord_bin,
        daemon_kind,
    })
}

fn select(all: &[Scenario], only: &[String]) -> Result<Vec<Scenario>, Failure> {
    if only.is_empty() {
        return Ok(all.to_vec());
    }
    let mut selected = Vec::new();
    for wanted in only {
        let wanted_upper = wanted.to_ascii_uppercase();
        match all.iter().find(|scenario| {
            scenario.id.eq_ignore_ascii_case(&wanted_upper) || scenario.name == wanted
        }) {
            Some(scenario) => selected.push(*scenario),
            None => {
                return Err(Failure::new(format!(
                    "unknown scenario {wanted:?}; run `vapor-e2e list`"
                )));
            }
        }
    }
    Ok(selected)
}

/// Runs the suite and returns the report. The process exit code is the
/// caller's decision (`report.is_green()`).
pub fn run(options: &RunOptions) -> Result<RunReport, Failure> {
    let run_id = new_run_id("run");
    let run_root = e2e_root(&options.repo_root).join(&run_id);
    fs::create_dir_all(&run_root)?;
    println!("[e2e] sandbox: {}", run_root.display());

    if !options.skip_build {
        build_product(&options.repo_root)?;
    }
    let paths = product_paths(&options.repo_root, options.daemon_kind)?;
    let host = Host::detect(&run_root, options.full, options.provider);
    if options.full
        && !host.launchd
        && let Some(reason) = &host.launchd_blocker
    {
        return Err(Failure::new(format!("--full refused: {reason}")));
    }

    let scenarios = select(&scenarios::all(), &options.only)?;
    let mut report = RunReport {
        schema_version: 1,
        run_id: run_id.clone(),
        provider: options.provider.label().to_string(),
        daemon: match options.daemon_kind {
            DaemonKind::CliRun => "vapor-run".to_string(),
            DaemonKind::Vapord => "vapord".to_string(),
        },
        full: options.full,
        host: HostSummary {
            os: host.os.to_string(),
            arch: host.arch.to_string(),
            native_watcher: host.native_watcher,
            case_insensitive_fs: host.case_insensitive_fs,
        },
        sandbox_root: run_root.display().to_string(),
        scenarios: Vec::new(),
        summary: Summary::default(),
    };

    let jobs = options.jobs.max(1);
    crate::wait::set_deadline_scale_percent(deadline_scale_percent(jobs));
    if jobs > 1 {
        println!(
            "[e2e] {jobs} scenarios at a time; deadlines scaled to {}%",
            crate::wait::deadline_scale_percent()
        );
    }
    let started = Instant::now();
    let last_durations =
        RunReport::durations_from(&e2e_root(&options.repo_root).join("last-result.json"));
    let results = run_all(
        &scenarios,
        jobs,
        &last_durations,
        &host,
        &paths,
        &run_root,
        options.keep,
    );
    let any_preserved = results.iter().any(|result| result.sandbox.is_some());
    report.scenarios = results;
    report.recompute_summary(started.elapsed().as_secs_f64());

    let json_path = options
        .json_path
        .clone()
        .unwrap_or_else(|| run_root.join("e2e-result.json"));
    // When the run root is about to be removed, the report must live
    // outside it.
    let json_path = if !options.keep && !any_preserved && json_path.starts_with(&run_root) {
        e2e_root(&options.repo_root).join("last-result.json")
    } else {
        json_path
    };
    report.write(&json_path)?;

    let summary = &report.summary;
    println!(
        "[e2e] {} — {} passed, {} failed, {} skipped, {} known gaps, {} unexpected passes ({:.1}s)",
        if report.is_green() { "OK" } else { "RED" },
        summary.passed,
        summary.failed,
        summary.skipped,
        summary.known_gaps,
        summary.unexpected_passes,
        summary.seconds
    );
    println!("[e2e] report: {}", json_path.display());
    if options.keep || any_preserved {
        println!(
            "[e2e] sandbox preserved at {} (remove with ./scripts/clean.sh)",
            run_root.display()
        );
    } else {
        sandbox::remove_tree(&run_root)?;
    }
    Ok(report)
}

/// Runs every scenario, `jobs` at a time, and returns the results in
/// catalog order. Disk-image scenarios take a lock among themselves
/// (one `hdiutil` at a time); a scenario that needs launchd waits
/// until every other one is done and then runs alone, since it
/// mutates host state.
fn run_all(
    scenarios: &[Scenario],
    jobs: usize,
    last_durations: &std::collections::BTreeMap<String, f64>,
    host: &Host,
    paths: &RunPaths,
    run_root: &Path,
    keep: bool,
) -> Vec<ScenarioResult> {
    let exclusive = |scenario: &Scenario| scenario.needs.contains(&Need::Launchd);
    let mut shared: Vec<(usize, Scenario)> = scenarios
        .iter()
        .enumerate()
        .filter(|(_, scenario)| !exclusive(scenario))
        .map(|(index, scenario)| (index, *scenario))
        .collect();
    // Longest first, by the previous run's clock; a scenario without a
    // record goes ahead of the known short ones. Catalog order breaks
    // ties, so a run with no history keeps it.
    shared.sort_by(|(a_index, a), (b_index, b)| {
        let seconds = |scenario: &Scenario| last_durations.get(scenario.id).copied();
        let a_seconds = seconds(a).unwrap_or(f64::MAX);
        let b_seconds = seconds(b).unwrap_or(f64::MAX);
        b_seconds
            .partial_cmp(&a_seconds)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a_index.cmp(b_index))
    });
    let queue = Mutex::new(VecDeque::from(shared));
    let results: Mutex<Vec<(usize, ScenarioResult)>> = Mutex::new(Vec::new());
    let disk_images = Mutex::new(());
    let workers = jobs.min(scenarios.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let next = queue.lock().expect("queue").pop_front();
                    let Some((index, scenario)) = next else {
                        break;
                    };
                    let guard = scenario
                        .needs
                        .contains(&Need::DiskImage)
                        .then(|| disk_images.lock().expect("disk image lock"));
                    let result = run_one(&scenario, host, paths, run_root, keep);
                    drop(guard);
                    results.lock().expect("results").push((index, result));
                }
            });
        }
    });
    for (index, scenario) in scenarios.iter().enumerate() {
        if exclusive(scenario) {
            let result = run_one(scenario, host, paths, run_root, keep);
            results.lock().expect("results").push((index, result));
        }
    }
    let mut results = results.into_inner().expect("results");
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

/// Failures, notes, and diagnostics of one scenario run.
type Outcome = (Vec<Failure>, Vec<String>, Option<String>);

fn run_one(
    scenario: &Scenario,
    host: &Host,
    paths: &RunPaths,
    run_root: &Path,
    keep: bool,
) -> ScenarioResult {
    let mut result = ScenarioResult {
        id: scenario.id.to_string(),
        name: scenario.name.to_string(),
        proves: scenario.proves.to_string(),
        verdict: Verdict::Skip,
        reasons: Vec::new(),
        seconds: 0.0,
        sandbox: None,
        notes: Vec::new(),
    };
    for need in scenario.needs {
        if let Err(reason) = host.check(*need) {
            result.reasons.push(reason);
            return result;
        }
    }

    let started = Instant::now();
    let scenario_root = run_root.join(scenario.id);
    let outcome = (|| -> Result<Outcome, Failure> {
        let sandbox = Sandbox::create(&scenario_root)?;
        let mut ctx = Ctx::new(host.clone(), paths.clone(), sandbox)?;
        let body = (scenario.run)(&mut ctx);
        let mut failures = Vec::new();
        if let Err(error) = body {
            failures.push(error);
        }
        failures.extend(ctx.epilogue());
        let diagnostics = if failures.is_empty() {
            None
        } else {
            Some(ctx.diagnostics())
        };
        let notes = ctx.notes.clone();
        ctx.release();
        Ok((failures, notes, diagnostics))
    })();
    result.seconds = started.elapsed().as_secs_f64();

    let (failures, notes, diagnostics) = match outcome {
        Ok(triple) => triple,
        Err(error) => (vec![error], Vec::new(), None),
    };
    result.notes = notes;

    let failed = !failures.is_empty();
    result.verdict = match (scenario.expect, failed) {
        (Expect::Pass, false) => Verdict::Pass,
        (Expect::Pass, true) => Verdict::Fail,
        (Expect::KnownGap(_), true) => Verdict::KnownGap,
        (Expect::KnownGap(_), false) => Verdict::UnexpectedPass,
    };
    result.reasons = failures.iter().map(|f| f.message.clone()).collect();
    if let Expect::KnownGap(task) = scenario.expect {
        match result.verdict {
            Verdict::KnownGap => result.notes.push(format!("known gap tracked as {task}")),
            Verdict::UnexpectedPass => result.reasons.push(format!(
                "known-gap scenario passed; remove the marker ({task})"
            )),
            _ => {}
        }
    }

    // A known gap is a failure someone will want to look at too.
    let preserve = keep || result.verdict.is_red() || matches!(result.verdict, Verdict::KnownGap);
    if preserve {
        result.sandbox = Some(scenario_root.display().to_string());
    } else {
        let _ = sandbox::remove_tree(&scenario_root);
    }
    // One lock for the whole block, so parallel scenarios never
    // interleave their lines.
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if result.verdict.is_red() || matches!(result.verdict, Verdict::KnownGap) {
        for reason in &result.reasons {
            let _ = writeln!(out, "[e2e]   {}: {reason}", scenario.id);
        }
    }
    if result.verdict.is_red()
        && let Some(diagnostics) = diagnostics
    {
        let _ = writeln!(out, "[e2e] ---- diagnostics {} ----", scenario.id);
        for line in diagnostics.lines() {
            let _ = writeln!(out, "[e2e]   {line}");
        }
        let _ = writeln!(out, "[e2e] ---------------------");
    }
    let _ = writeln!(out, "{}", console_line(&result));
    result
}

/// Finds the repository root from the running executable
/// (`<root>/target/<profile>/vapor-e2e`) or the working directory.
pub fn discover_repo_root() -> Result<PathBuf, Failure> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        candidates.extend(exe.ancestors().map(Path::to_path_buf));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.extend(cwd.ancestors().map(Path::to_path_buf));
    }
    for candidate in candidates {
        let manifest = candidate.join("Cargo.toml");
        if manifest.is_file()
            && fs::read_to_string(&manifest).is_ok_and(|text| text.contains("[workspace]"))
            && candidate.join("scripts").join("e2e.sh").is_file()
        {
            return Ok(candidate);
        }
    }
    Err(Failure::new(
        "cannot find the repository root; pass --repo-root",
    ))
}
