//! Runs the scenario list: builds the product binaries, provisions one
//! sandbox per scenario, applies the `needs` filter, runs the body and
//! the epilogue, and writes the report. A failed scenario keeps its
//! sandbox and prints diagnostics; a green run removes everything
//! unless asked to keep it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::daemon::DaemonKind;
use crate::host::{Host, Provider};
use crate::report::{HostSummary, RunReport, ScenarioResult, Summary, Verdict, console_line};
use crate::sandbox::{self, Sandbox};
use crate::scenario::{Ctx, Expect, RunPaths, Scenario};
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
    println!("[e2e] building vapor + vapord (cargo build -p vapor-cli -p vapor-daemon)");
    let status = Command::new("cargo")
        .args(["build", "--quiet", "-p", "vapor-cli", "-p", "vapor-daemon"])
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
    let debug = repo_root.join("target").join("debug");
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
            "vapor binary missing at {} (run without --skip-build)",
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

    let mut any_preserved = false;
    for scenario in scenarios {
        let result = run_one(&scenario, &host, &paths, &run_root, options.keep);
        if result.sandbox.is_some() {
            any_preserved = true;
        }
        println!("{}", console_line(&result));
        report.scenarios.push(result);
    }
    report.recompute_summary();

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

    let preserve = keep || result.verdict.is_red();
    if preserve {
        result.sandbox = Some(scenario_root.display().to_string());
    } else {
        let _ = sandbox::remove_tree(&scenario_root);
    }
    if result.verdict.is_red() || matches!(result.verdict, Verdict::KnownGap) {
        for reason in &result.reasons {
            println!("[e2e]   {}: {reason}", scenario.id);
        }
    }
    if result.verdict.is_red()
        && let Some(diagnostics) = diagnostics
    {
        println!("[e2e] ---- diagnostics {} ----", scenario.id);
        for line in diagnostics.lines() {
            println!("[e2e]   {line}");
        }
        println!("[e2e] ---------------------");
    }
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
