//! The soak loop: provision, seed, then cycle through phases until the
//! planned duration elapses. Every phase writes on one side (or both,
//! on disjoint paths), waits for the daemon to go quiet, and asks the
//! model whether both trees hold what they must. Faults fire between
//! operations or around a phase. The first violation freezes the run
//! unless told otherwise.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vapor_e2e::cli::Cli;
use vapor_e2e::daemon::{Daemon, DaemonKind, Signal};
use vapor_e2e::db::StateDb;
use vapor_e2e::diskimage::{DiskImage, ImageFs};
use vapor_e2e::host::{Host, Provider};
use vapor_e2e::logs;
use vapor_e2e::oracle::{FileFacts, OracleOptions, TreeOracle};
use vapor_e2e::runner::{self, RunPaths};
use vapor_e2e::sandbox::{Home, Sandbox};
use vapor_e2e::scenario::{SHUTDOWN_GRACE, STARTUP_TIMEOUT};
use vapor_e2e::wait;

use crate::Failure;
use crate::faults::{FaultKind, FaultPlan};
use crate::health::HealthMonitor;
use crate::model::{Expected, Model, SyncMode, Violation};
use crate::report::{
    FaultRecord, PhaseRecord, Report, RunState, SloChecks, Status, unix_now, write_json,
};
use crate::rng::Rng;
use crate::throttle_file::{Station, ThrottleScript};
use crate::workload::{LoadShape, Op, Side, SizeClass, apply, content_for, sha256_hex, size_for};

/// Daemon warnings a soak expects to see; anything else is listed in
/// the report for a human to triage (never a violation by itself).
const EXPECTED_WARNINGS: &[&str] = &[
    "Resolved concurrent divergence by keeping both versions",
    "Kept both versions on download-apply",
    "Remote changed since last sync; preserving it over a local deletion",
    "Mass-deletion guard tripped",
    "Cloud sync directory became unavailable",
    "Sync root is missing",
    "Cloud sync directory",
    "Configuration keys changed",
    "collides with a differently-cased local file",
    "differ only by case",
    "Reconcile found a file/directory type mismatch",
    "Remote changes cursor expired",
    "Cleared startup reconstruction barrier",
    "Recorded filesystem watcher error",
    "Emitting a watch event optimistically",
    // The throttle walk drives the daemon into Suspended on purpose.
    "Updated throttle state",
];

/// RSS budget from the performance SLO doc (SLO-3, storm scenario).
const RSS_BUDGET_BYTES: u64 = 350 * 1024 * 1024;
/// CPU average budget under active load (SLO-2).
const CPU_BUDGET_PERCENT: f64 = 5.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThrottleMode {
    Static,
    Walk,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub repo_root: PathBuf,
    pub seed: u64,
    pub duration: Duration,
    pub mode: SyncMode,
    pub load: LoadShape,
    pub faults: FaultPlan,
    pub throttle: ThrottleMode,
    pub daemon_kind: DaemonKind,
    pub skip_build: bool,
    /// Build and run the release profile of the product binaries.
    pub release: bool,
    pub keep: bool,
    pub continue_on_violation: bool,
    pub status_interval: Duration,
    /// Mount the cloud root on a throwaway disk image of this size.
    pub cloud_image_mb: Option<u32>,
    pub cloud_image_fs: ImageFs,
    /// Longest a phase may take to converge.
    pub converge_deadline: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PhaseKind {
    Seed,
    LocalChurn,
    CloudChurn,
    BothChurn,
    Conflicts,
    Quiet,
}

impl PhaseKind {
    fn name(self) -> &'static str {
        match self {
            PhaseKind::Seed => "seed",
            PhaseKind::LocalChurn => "local-churn",
            PhaseKind::CloudChurn => "cloud-churn",
            PhaseKind::BothChurn => "both-churn",
            PhaseKind::Conflicts => "conflicts",
            PhaseKind::Quiet => "quiet",
        }
    }

    const CYCLE: &'static [PhaseKind] = &[
        PhaseKind::LocalChurn,
        PhaseKind::CloudChurn,
        PhaseKind::BothChurn,
        PhaseKind::Conflicts,
        PhaseKind::Quiet,
    ];
}

pub struct Driver {
    cfg: Config,
    rng: Rng,
    model: Model,
    paths: RunPaths,
    sandbox: Sandbox,
    home: Home,
    daemon: Option<Daemon>,
    cli: Cli,
    db: StateDb,
    health: HealthMonitor,
    throttle: Option<ThrottleScript>,
    image: Option<DiskImage>,
    started: Instant,
    run_id: String,
    status_path: PathBuf,
    report_path: PathBuf,
    model_path: PathBuf,
    /// Append-only record of every operation, fault, and phase
    /// boundary, one JSON object per line, for triage.
    op_log: fs::File,
    last_status_write: Instant,
    last_health_sample: Instant,
    phases: Vec<PhaseRecord>,
    faults: Vec<FaultRecord>,
    violations: Vec<Violation>,
    daemon_errors: Vec<String>,
    warnings_seen: std::collections::BTreeSet<String>,
    ops_done: u64,
    crashes: u32,
    phase_index: usize,
    phase_name: String,
    last_verdict: Option<String>,
    state: RunState,
    message: String,
    /// Set while the cloud root is parked so the workload never writes
    /// into a directory that does not exist.
    cloud_parked: Option<PathBuf>,
    /// The cloud root's permissions before the permissions fault took
    /// them away; `Some` while the root is unreadable.
    cloud_locked_mode: Option<u32>,
}

impl Driver {
    /// Provisions the sandbox and starts the daemon. Nothing is written
    /// to the trees yet.
    pub fn provision(cfg: Config) -> Result<Self, Failure> {
        if !cfg.skip_build {
            runner::build_product_profile(&cfg.repo_root, cfg.release)?;
        }
        let paths = runner::product_paths_profile(&cfg.repo_root, cfg.daemon_kind, cfg.release)?;
        let run_id = runner::new_run_id("soak");
        let root = runner::e2e_root(&cfg.repo_root).join(&run_id);
        let sandbox = Sandbox::create(&root)?;
        let _host = Host::detect(&root, false, Provider::Filesystem);
        let mut home = sandbox.home("primary")?;

        let image = match cfg.cloud_image_mb {
            Some(size_mb) => {
                let image = DiskImage::create(&root, "cloud-volume", size_mb, cfg.cloud_image_fs)?;
                home.cloud = image.mount_point.join("Vapor");
                Some(image)
            }
            None => None,
        };
        let throttle = match cfg.throttle {
            ThrottleMode::Static => None,
            ThrottleMode::Walk => {
                let script = ThrottleScript::new(&root.join("throttle-inputs.json"));
                script.set(Station::Idle)?;
                home.extra_env.insert(
                    vapor_shared::constants::env::VAPOR_THROTTLE_INPUTS.to_string(),
                    script.env_value(),
                );
                Some(script)
            }
        };
        let cli = Cli::new(&paths.cli_bin, &home);
        cli.config_set("localSyncDirectory", &home.local.to_string_lossy())?;
        cli.config_set("cloudSyncDirectory", &home.cloud.to_string_lossy())?;
        cli.config_set("syncMode", cfg.mode.label())?;
        let db = StateDb::at(&home.state_db());
        let now = Instant::now();
        let status_path = root.join("soak-status.json");
        let report_path = root.join("soak-report.json");
        let model_path = root.join("model.json");
        let op_log = fs::File::options()
            .create(true)
            .append(true)
            .open(root.join("ops.jsonl"))?;
        let mut driver = Self {
            rng: Rng::new(cfg.seed),
            model: Model::new(cfg.seed, cfg.mode),
            cfg,
            paths,
            sandbox,
            home,
            daemon: None,
            cli,
            db,
            health: HealthMonitor::new(),
            throttle,
            image,
            started: now,
            run_id,
            status_path,
            report_path,
            model_path,
            op_log,
            last_status_write: now - Duration::from_secs(3600),
            last_health_sample: now - Duration::from_secs(3600),
            phases: Vec::new(),
            faults: Vec::new(),
            violations: Vec::new(),
            daemon_errors: Vec::new(),
            warnings_seen: std::collections::BTreeSet::new(),
            ops_done: 0,
            crashes: 0,
            phase_index: 0,
            phase_name: "provision".to_string(),
            last_verdict: None,
            state: RunState::Starting,
            message: "provisioning".to_string(),
            cloud_parked: None,
            cloud_locked_mode: None,
        };
        driver.start_daemon()?;
        driver.state = RunState::Running;
        driver.write_status(true)?;
        println!(
            "[soak] run {} — sandbox {}",
            driver.run_id,
            driver.sandbox.root.display()
        );
        println!("[soak] status: {}", driver.status_path.display());
        Ok(driver)
    }

    pub fn sandbox_root(&self) -> &Path {
        &self.sandbox.root
    }

    // ----- daemon control -----

    fn start_daemon(&mut self) -> Result<(), Failure> {
        let daemon = Daemon::spawn(
            self.cfg.daemon_kind,
            &self.paths.cli_bin,
            &self.paths.vapord_bin,
            &self.home,
            &self.sandbox.root,
        )?;
        self.daemon = Some(daemon);
        self.health.reset_process();
        // A daemon started while the cloud root is parked or locked
        // blocks (Error) until the root is usable again.
        let expected = if self.cloud_unavailable() {
            "Error"
        } else {
            "Running"
        };
        self.cli.wait_run_state(expected, STARTUP_TIMEOUT)?;
        Ok(())
    }

    /// The cloud root cannot take the driver's writes right now: parked
    /// away or stripped of its permissions by a phase fault.
    fn cloud_unavailable(&self) -> bool {
        self.cloud_parked.is_some() || self.cloud_locked_mode.is_some()
    }

    fn daemon_pid(&mut self) -> Option<u32> {
        let daemon = self.daemon.as_mut()?;
        daemon.is_alive().then(|| daemon.pid())
    }

    fn stop_daemon(&mut self) -> Result<(), Failure> {
        if let Some(mut daemon) = self.daemon.take() {
            daemon.terminate(SHUTDOWN_GRACE)?;
        }
        Ok(())
    }

    // ----- status and health -----

    fn write_status(&mut self, force: bool) -> Result<(), Failure> {
        if !force && self.last_status_write.elapsed() < self.cfg.status_interval {
            return Ok(());
        }
        if force || self.last_health_sample.elapsed() >= Duration::from_secs(10) {
            let with_files = self.health.samples.len().is_multiple_of(6);
            if let Some(pid) = self.daemon_pid() {
                self.health.sample(pid, with_files);
            }
            self.last_health_sample = Instant::now();
        }
        let status = self.status();
        write_json(&self.status_path, &status)?;
        self.last_status_write = Instant::now();
        Ok(())
    }

    fn status(&mut self) -> Status {
        Status {
            schema_version: 1,
            run_id: self.run_id.clone(),
            state: self.state,
            message: self.message.clone(),
            seed: self.cfg.seed,
            mode: self.cfg.mode.label().to_string(),
            load: self.cfg.load.name.clone(),
            faults: self
                .cfg
                .faults
                .kinds
                .iter()
                .map(|kind| kind.label().to_string())
                .collect(),
            throttle: match self.cfg.throttle {
                ThrottleMode::Static => "static".to_string(),
                ThrottleMode::Walk => "walk".to_string(),
            },
            sandbox: self.sandbox.root.display().to_string(),
            daemon: match self.cfg.daemon_kind {
                DaemonKind::CliRun => "vapor-run".to_string(),
                DaemonKind::Vapord => "vapord".to_string(),
            },
            daemon_pid: self.daemon_pid(),
            started_at_unix: unix_now().saturating_sub(self.started.elapsed().as_secs()),
            updated_at_unix: unix_now(),
            elapsed_seconds: self.started.elapsed().as_secs_f64(),
            planned_seconds: self.cfg.duration.as_secs_f64(),
            phase_index: self.phase_index,
            phase_name: self.phase_name.clone(),
            phases_done: self.phases.len(),
            ops_done: self.ops_done,
            files_live: self.model.live_files(),
            crashes: self.crashes,
            faults_injected: self.faults.len(),
            violations_total: self.violations.len(),
            last_verdict: self.last_verdict.clone(),
            health_latest: self.health.samples.last().cloned(),
            health: self.health.summary(),
        }
    }

    fn write_report(&mut self) -> Result<(), Failure> {
        let health = self.health.summary();
        let crash_phases_clean = self
            .phases
            .iter()
            .filter(|phase| phase.faults.iter().any(|f| f.starts_with("crash")))
            .all(|phase| phase.violations.is_empty());
        let report = Report {
            status: self.status(),
            phases: self.phases.clone(),
            faults: self.faults.clone(),
            violations: self.violations.clone(),
            slo: SloChecks {
                rss_p95_within_budget: health.rss_p95_bytes <= RSS_BUDGET_BYTES,
                rss_budget_bytes: RSS_BUDGET_BYTES,
                cpu_avg_within_budget: health.cpu_avg_percent <= CPU_BUDGET_PERCENT,
                cpu_budget_percent: CPU_BUDGET_PERCENT,
                crashes_recovered_without_loss: crash_phases_clean,
                every_phase_converged: !self
                    .violations
                    .iter()
                    .any(|violation| violation.kind == "no-convergence"),
            },
            daemon_errors: self.daemon_errors.clone(),
        };
        write_json(&self.report_path, &report)?;
        self.model.save(&self.model_path)?;
        Ok(())
    }

    fn note(&mut self, text: impl Into<String>) {
        self.message = text.into();
        println!(
            "[soak] {:>7.1}s  {}",
            self.started.elapsed().as_secs_f64(),
            self.message
        );
        self.log_event(serde_json::json!({ "note": self.message }));
    }

    fn log_event(&mut self, mut event: serde_json::Value) {
        use std::io::Write;
        if let Some(object) = event.as_object_mut() {
            object.insert(
                "t".to_string(),
                serde_json::json!(self.started.elapsed().as_secs_f64()),
            );
            object.insert("phase".to_string(), serde_json::json!(self.phase_name));
        }
        let _ = writeln!(self.op_log, "{event}");
    }

    // ----- the run -----

    pub fn run(&mut self) -> Result<Report, Failure> {
        let result = self.run_phases();
        match &result {
            Ok(()) => {
                self.state = RunState::Finished;
                self.message = format!(
                    "finished: {} phases, {} ops, {} faults, {} violations",
                    self.phases.len(),
                    self.ops_done,
                    self.faults.len(),
                    self.violations.len()
                );
                let _ = self.stop_daemon();
            }
            Err(error) => {
                if self.state != RunState::Frozen {
                    self.state = RunState::Failed;
                    self.message = format!("driver failed: {error}");
                }
                // The preserved sandbox must stay inspectable: leave the
                // cloud volume mounted (`./scripts/clean.sh` detaches it).
                if let Some(image) = self.image.take() {
                    std::mem::forget(image);
                }
            }
        }
        self.write_report()?;
        self.write_status(true)?;
        println!("[soak] {}", self.message);
        println!("[soak] report: {}", self.report_path.display());
        let report_text = fs::read_to_string(&self.report_path).unwrap_or_default();
        if self.state == RunState::Finished && self.violations.is_empty() && !self.cfg.keep {
            // The report outlives the sandbox it describes.
            let kept = runner::e2e_root(&self.cfg.repo_root).join("soak-last-report.json");
            let _ = fs::copy(&self.report_path, &kept);
            println!("[soak] report kept at {}", kept.display());
            self.image.take();
            let _ = vapor_e2e::sandbox::remove_tree(&self.sandbox.root);
        } else {
            println!(
                "[soak] sandbox preserved at {} (daemon {})",
                self.sandbox.root.display(),
                if self.state == RunState::Frozen {
                    "still running"
                } else {
                    "stopped"
                }
            );
        }
        let report: Report = serde_json::from_str(&report_text)?;
        result.map(|()| report)
    }

    fn run_phases(&mut self) -> Result<(), Failure> {
        self.run_phase(PhaseKind::Seed)?;
        let mut cycle = 0usize;
        while self.started.elapsed() < self.cfg.duration {
            for kind in PhaseKind::CYCLE {
                if self.started.elapsed() >= self.cfg.duration {
                    break;
                }
                self.run_phase(*kind)?;
            }
            cycle += 1;
            self.note(format!("cycle {cycle} complete"));
        }
        Ok(())
    }

    fn ops_for(&self, kind: PhaseKind) -> usize {
        let base = match self.cfg.load.name.as_str() {
            "bulk" => 1_500,
            "large" => 6,
            "trickle" => 40,
            _ => 120,
        };
        match kind {
            PhaseKind::Seed => base.max(20),
            PhaseKind::Quiet => 0,
            PhaseKind::Conflicts => (base / 10).clamp(2, 40),
            _ => base,
        }
    }

    fn run_phase(&mut self, kind: PhaseKind) -> Result<(), Failure> {
        self.phase_index += 1;
        self.phase_name = kind.name().to_string();
        let index = self.phase_index;
        let started = Instant::now();
        let mut faults_this_phase = Vec::new();
        self.note(format!("phase {index} {} starting", kind.name()));

        // Phase-level faults.
        let phase_faults: Vec<FaultKind> = self
            .cfg
            .faults
            .kinds
            .iter()
            .copied()
            .filter(|kind| !kind.is_per_op())
            .filter(|_| self.rng.chance(35))
            .collect();
        for fault in &phase_faults {
            self.begin_phase_fault(*fault, &mut faults_this_phase)?;
        }

        let ops = self.ops_for(kind);
        let ops_done_before = self.ops_done;
        match kind {
            PhaseKind::Seed => {
                let seeding = self.cfg.load.seeding();
                let shape = std::mem::replace(&mut self.cfg.load, seeding);
                let result = self.churn(Side::Local, ops, None, &mut faults_this_phase);
                self.cfg.load = shape;
                result?;
            }
            PhaseKind::LocalChurn => self.churn(Side::Local, ops, None, &mut faults_this_phase)?,
            PhaseKind::CloudChurn => self.churn(Side::Cloud, ops, None, &mut faults_this_phase)?,
            PhaseKind::BothChurn => {
                // Disjoint halves of the owned set, interleaved. A
                // subtree removal would take the other side's files with
                // it, so both sides work file by file here.
                let delete_tree = self.cfg.load.delete_tree;
                self.cfg.load.delete_tree = 0;
                let owned = self.model.owned_paths();
                let (local_half, cloud_half): (Vec<String>, Vec<String>) = owned
                    .into_iter()
                    .enumerate()
                    .fold((Vec::new(), Vec::new()), |(mut l, mut c), (i, p)| {
                        if i % 2 == 0 {
                            l.push(p)
                        } else {
                            c.push(p)
                        }
                        (l, c)
                    });
                for step in 0..ops {
                    let (side, subset) = if step.is_multiple_of(2) {
                        (Side::Local, &local_half)
                    } else {
                        (Side::Cloud, &cloud_half)
                    };
                    self.one_op(side, Some(subset), &mut faults_this_phase)?;
                }
                self.cfg.load.delete_tree = delete_tree;
            }
            PhaseKind::Conflicts => self.conflicts(ops)?,
            PhaseKind::Quiet => {
                let until = Instant::now() + Duration::from_secs(20);
                while Instant::now() < until {
                    self.write_status(false)?;
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }

        for fault in &phase_faults {
            self.end_phase_fault(*fault)?;
        }

        // Quiescence, then the oracle.
        let converge_started = Instant::now();
        let converged = self.converge();
        let converge_seconds = converge_started.elapsed().as_secs_f64();
        let mut violations = match converged {
            Ok(()) => Vec::new(),
            Err(error) => vec![Violation {
                kind: "no-convergence".to_string(),
                path: String::new(),
                detail: error.message,
            }],
        };
        match self.model.check(&self.home.local, &self.home.cloud) {
            Ok(found) => violations.extend(found),
            Err(error) => violations.push(Violation {
                kind: "oracle-error".to_string(),
                path: String::new(),
                detail: error.message,
            }),
        }
        let (errors, unexpected) = self.scan_daemon_log();
        for line in errors {
            if !self.daemon_errors.contains(&line) {
                self.daemon_errors.push(line.clone());
                violations.push(Violation {
                    kind: "daemon-error".to_string(),
                    path: String::new(),
                    detail: line,
                });
            }
        }

        // Adopt what the daemon legitimately created (conflict copies,
        // restored files) so later phases can touch them.
        if violations.is_empty() {
            let oracle = TreeOracle::new(&OracleOptions {
                extra_ignore_rules: Vec::new(),
                compare_mode: cfg!(unix),
            })?;
            let (local_tree, _) = oracle.snapshot(&self.home.local)?;
            self.model.absorb_conflicts(&local_tree);
            self.model.adopt_known_content(&local_tree);
        }

        let record = PhaseRecord {
            index,
            name: kind.name().to_string(),
            ops: (self.ops_done - ops_done_before) as usize,
            seconds: started.elapsed().as_secs_f64(),
            converge_seconds,
            violations: violations.clone(),
            faults: faults_this_phase,
            unexpected_warnings: unexpected,
        };
        self.last_verdict = Some(if violations.is_empty() {
            format!("phase {index} {} clean", kind.name())
        } else {
            format!(
                "phase {index} {}: {} violation(s)",
                kind.name(),
                violations.len()
            )
        });
        self.phases.push(record);
        self.violations.extend(violations.iter().cloned());
        self.model.save(&self.model_path)?;
        self.write_status(true)?;
        self.note(format!(
            "phase {index} {} done: {} ops, converged in {:.1}s, {} violation(s)",
            kind.name(),
            self.ops_done - ops_done_before,
            converge_seconds,
            violations.len()
        ));
        if !violations.is_empty() {
            for violation in &violations {
                println!(
                    "[soak]   {} {}: {}",
                    violation.kind, violation.path, violation.detail
                );
            }
            if !self.cfg.continue_on_violation {
                self.state = RunState::Frozen;
                self.message = format!(
                    "frozen after phase {index} {}: {} violation(s); daemon left running, sandbox preserved",
                    kind.name(),
                    violations.len()
                );
                return Err(Failure::new(self.message.clone()));
            }
        }
        Ok(())
    }

    // ----- workload -----

    fn churn(
        &mut self,
        side: Side,
        ops: usize,
        subset: Option<&[String]>,
        faults: &mut Vec<String>,
    ) -> Result<(), Failure> {
        for _ in 0..ops {
            self.one_op(side, subset, faults)?;
            if self.started.elapsed() >= self.cfg.duration + Duration::from_secs(60) {
                break;
            }
        }
        Ok(())
    }

    fn side_root(&self, side: Side) -> PathBuf {
        match side {
            Side::Local => self.home.local.clone(),
            Side::Cloud => self.home.cloud.clone(),
        }
    }

    fn one_op(
        &mut self,
        side: Side,
        subset: Option<&[String]>,
        faults: &mut Vec<String>,
    ) -> Result<(), Failure> {
        if side == Side::Cloud && self.cloud_unavailable() {
            return Ok(());
        }
        let owned: Vec<String> = match subset {
            Some(subset) => subset
                .iter()
                .filter(|path| self.model.files.contains_key(*path))
                .cloned()
                .collect(),
            None => self.model.owned_paths(),
        };
        let dirs = self.model.directories();
        let live = self.model.live_files();
        let op = crate::workload::next_op(&mut self.rng, &self.cfg.load, &owned, &dirs, live);
        let version = self.model.allocate_version();
        let size = match &op {
            Op::Create { size, .. } | Op::Overwrite { size, .. } => {
                size_for(*size, &self.cfg.load, &mut self.rng)
            }
            _ => 0,
        };
        let root = self.side_root(side);
        fs::create_dir_all(&root)?;
        match apply(&root, &op, self.cfg.seed, version, size) {
            Ok(_) => {}
            Err(error) => {
                // The trees move underneath the generator (a delete the
                // daemon applied, a rename in flight); an op that no
                // longer applies is skipped, not a failure of the run.
                self.note(format!(
                    "skipped {} on {}: {error}",
                    op.kind(),
                    side.label()
                ));
                self.log_event(serde_json::json!({
                    "side": side.label(), "op": op, "version": version, "skipped": error.message,
                }));
                self.model.record(side, &op, None);
                return Ok(());
            }
        }
        let written = self.facts_after(&root, &op, version)?;
        self.log_event(serde_json::json!({
            "side": side.label(), "op": op, "version": version,
            "sha256": written.as_ref().map(|w| w.sha256.clone()),
            "size": written.as_ref().map(|w| w.size),
        }));
        self.model.record(side, &op, written);
        self.ops_done += 1;
        if let Some(fault) = self.cfg.faults.pick_per_op(&mut self.rng) {
            self.inject_per_op(fault, side, subset, faults)?;
        }
        self.write_status(false)?;
        if self.cfg.load.op_interval_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.cfg.load.op_interval_ms));
        }
        Ok(())
    }

    /// Reads back what an op produced, as the model must remember it.
    fn facts_after(&self, root: &Path, op: &Op, version: u64) -> Result<Option<Expected>, Failure> {
        let path = match op {
            Op::Create { path, .. }
            | Op::Overwrite { path, .. }
            | Op::SameSizeEdit { path }
            | Op::Append { path, .. }
            | Op::Chmod { path, .. } => path,
            _ => return Ok(None),
        };
        let mut full = root.to_path_buf();
        for segment in path.split('/') {
            full.push(segment);
        }
        let bytes = fs::read(&full)?;
        let executable = executable_bit(&full);
        Ok(Some(Expected {
            version,
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
            executable,
            writer: match root == self.home.local {
                true => Side::Local,
                false => Side::Cloud,
            },
        }))
    }

    /// Edits the same paths on both sides at once; both payloads must
    /// survive somewhere.
    fn conflicts(&mut self, count: usize) -> Result<(), Failure> {
        if self.cfg.mode != SyncMode::TwoWay {
            // One-way modes have no keep-both; the churn phases already
            // cover reverts.
            return Ok(());
        }
        let owned = self.model.owned_paths();
        if owned.is_empty() {
            return Ok(());
        }
        for _ in 0..count.min(owned.len()) {
            let path = self.rng.pick(&owned).clone();
            if self.model.contested.contains_key(&path) || !self.model.files.contains_key(&path) {
                continue;
            }
            let local_version = self.model.allocate_version();
            let cloud_version = self.model.allocate_version();
            let size = size_for(SizeClass::Small, &self.cfg.load, &mut self.rng);
            let local_bytes = content_for(self.cfg.seed, &path, local_version, size);
            let cloud_bytes = content_for(self.cfg.seed, &path, cloud_version, size + 7);
            let local_full = join_relative(&self.home.local, &path);
            let cloud_full = join_relative(&self.home.cloud, &path);
            if self.cloud_unavailable() {
                return Ok(());
            }
            let write = |full: &Path, bytes: &[u8]| -> Result<(), Failure> {
                let temp = full.with_extension("soak-tmp");
                fs::write(&temp, bytes)?;
                fs::rename(&temp, full)?;
                Ok(())
            };
            if write(&local_full, &local_bytes).is_err()
                || write(&cloud_full, &cloud_bytes).is_err()
            {
                continue;
            }
            let expected = |version: u64, bytes: &[u8], writer: Side| Expected {
                version,
                size: bytes.len() as u64,
                sha256: sha256_hex(bytes),
                executable: false,
                writer,
            };
            self.model.record_contested(
                &path,
                expected(local_version, &local_bytes, Side::Local),
                expected(cloud_version, &cloud_bytes, Side::Cloud),
            );
            self.ops_done += 2;
            self.write_status(false)?;
            std::thread::sleep(Duration::from_millis(self.cfg.load.op_interval_ms.max(200)));
        }
        Ok(())
    }

    // ----- quiescence -----

    /// Waits until the daemon has nothing left to do: queue empty and no
    /// new intent for a quiet window, a requested reconcile, and the
    /// same again. Handles the states the workload legitimately
    /// provokes (the mass-deletion guard, a parked cloud root).
    fn converge(&mut self) -> Result<(), Failure> {
        let deadline = Instant::now() + self.cfg.converge_deadline;
        self.ensure_daemon_working(deadline)?;
        self.settle(deadline, "first drain")?;
        if self.cli.reconcile().is_err() {
            self.ensure_daemon_working(deadline)?;
            self.cli.reconcile()?;
        }
        self.settle(deadline, "post-reconcile drain")?;
        Ok(())
    }

    fn settle(&mut self, deadline: Instant, label: &str) -> Result<(), Failure> {
        // Longer than the longest debounce window, so a burst that is
        // still stabilizing never reads as quiet.
        let quiet = Duration::from_secs(10);
        loop {
            self.ensure_daemon_working(deadline)?;
            self.cli.flush_now();
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Failure::new(format!(
                    "{label}: the daemon did not go quiet within {}s (queue {:?}, enqueued {:?}, state {:?})",
                    self.cfg.converge_deadline.as_secs(),
                    self.db.pending_intents(),
                    self.db.enqueue_high_water(),
                    self.cli.run_state()
                )));
            }
            let drained_by = Instant::now() + remaining;
            let mut last_note = Instant::now();
            while !self.db.queue_drained() {
                if Instant::now() >= drained_by {
                    break;
                }
                self.ensure_daemon_working(deadline)?;
                if last_note.elapsed() > Duration::from_secs(60) {
                    // Say what the daemon is waiting on, so a watcher
                    // reading the status file sees a retrying intent
                    // instead of a silent stall.
                    let rows = self.db.rows(
                        "SELECT kind, path_text, attempt_count, last_error FROM queue_intents \
                         WHERE attempt_count > 0 ORDER BY attempt_count DESC LIMIT 3;",
                    );
                    if !rows.is_empty() {
                        self.note(format!(
                            "{label}: still waiting on {} queued intent(s); retrying: {}",
                            self.db.pending_intents().unwrap_or(-1),
                            rows.join(" || ")
                        ));
                    }
                    last_note = Instant::now();
                }
                self.write_status(false)?;
                std::thread::sleep(wait::POLL_INTERVAL);
            }
            let high_water = self.db.enqueue_high_water();
            let quiet_until = Instant::now() + quiet;
            let mut still_quiet = true;
            while Instant::now() < quiet_until {
                std::thread::sleep(wait::POLL_INTERVAL);
                self.write_status(false)?;
                if self.db.enqueue_high_water() != high_water || !self.db.queue_drained() {
                    still_quiet = false;
                    break;
                }
            }
            if still_quiet {
                return Ok(());
            }
        }
    }

    /// Restarts a dead daemon, answers the mass-deletion decisions the
    /// workload provokes, and waits for the daemon to be running, so
    /// the run never hangs on a state the workload itself caused.
    fn ensure_daemon_working(&mut self, deadline: Instant) -> Result<(), Failure> {
        if self.daemon_pid().is_none() {
            self.note("daemon is not running; restarting it");
            self.start_daemon()?;
        }
        self.answer_decisions()?;
        // While the cloud root is parked the daemon holds the profile
        // (Error, with a root-missing decision open), and while it is
        // unreadable sync blocks (Error, no decision); that is the
        // product working as designed, not a daemon to wait on.
        let expected = if self.cloud_unavailable() {
            "Error"
        } else {
            "Running"
        };
        let started = Instant::now();
        while self.cli.run_state().as_deref() != Some(expected) {
            if Instant::now() >= deadline || started.elapsed() > STARTUP_TIMEOUT {
                return Err(Failure::new(format!(
                    "daemon never returned to {expected} (state {:?})",
                    self.cli.run_state()
                )));
            }
            std::thread::sleep(wait::POLL_INTERVAL);
        }
        Ok(())
    }

    /// The workload's subtree removals are deliberate, so every open
    /// mass-deletion decision is answered `apply`; each one is recorded
    /// as a `guard-trip`, the product working as designed. A
    /// `root-missing` question about the cloud root while the driver
    /// has it parked is expected and left open: it withdraws itself when
    /// the root is restored. Any other open decision is a finding: the
    /// driver does not know the right answer, and the run must not
    /// guess.
    fn answer_decisions(&mut self) -> Result<(), Failure> {
        let Ok(report) = self.cli.json(&["decisions", "list", "--json"]) else {
            return Ok(());
        };
        let open: Vec<serde_json::Value> = report["decisions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|decision| decision["choice"].is_null())
            .cloned()
            .collect();
        for decision in open {
            let id = decision["id"].as_i64().unwrap_or_default();
            let kind = decision["kind"].as_str().unwrap_or_default().to_string();
            if kind == "root-missing"
                && decision["evidence"]["side"] == "cloud"
                && self.cloud_parked.is_some()
            {
                continue;
            }
            if kind != "mass-deletion" {
                return Err(Failure::new(format!(
                    "the daemon opened a {kind} decision (#{id}) the driver cannot answer: {}",
                    decision["question"].as_str().unwrap_or_default()
                )));
            }
            self.record_fault_event(
                "guard-trip",
                format!(
                    "mass-deletion guard held {} deletion(s) behind decision #{id}; applying ({})",
                    decision["heldIntents"].as_u64().unwrap_or_default(),
                    decision["question"].as_str().unwrap_or_default()
                ),
            );
            self.cli
                .ok(&["decisions", "resolve", &id.to_string(), "--choose", "apply"])?;
        }
        Ok(())
    }

    fn scan_daemon_log(&mut self) -> (Vec<String>, Vec<String>) {
        let report = logs::scan(
            &self.home.daemon_log(),
            &EXPECTED_WARNINGS
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>(),
            &[],
        );
        let mut unexpected = Vec::new();
        for line in report.unexpected_warnings {
            // Strip the timestamp so repeats of one message dedupe.
            let message = line
                .split_once("[WARNING]")
                .map(|(_, rest)| rest)
                .unwrap_or(&line)
                .trim()
                .to_string();
            if self.warnings_seen.insert(message.clone()) && unexpected.len() < 20 {
                unexpected.push(message);
            }
        }
        (report.errors, unexpected)
    }

    // ----- faults -----

    fn record_fault_event(&mut self, kind: &str, detail: String) {
        self.faults.push(FaultRecord {
            kind: kind.to_string(),
            phase: self.phase_name.clone(),
            at_seconds: self.started.elapsed().as_secs_f64(),
            detail,
        });
    }

    /// Fires one per-op fault. Extra operations a fault performs stay
    /// on the phase's own side and path subset: the model's one-writer
    /// rule must hold through a fault too.
    fn inject_per_op(
        &mut self,
        fault: FaultKind,
        side: Side,
        subset: Option<&[String]>,
        faults: &mut Vec<String>,
    ) -> Result<(), Failure> {
        match fault {
            FaultKind::Crash | FaultKind::CrashMidTransfer => {
                if fault == FaultKind::CrashMidTransfer {
                    // Wait (briefly) for a transfer to be in flight.
                    let until = Instant::now() + Duration::from_secs(8);
                    while Instant::now() < until {
                        let busy = self.db.scalar_i64(
                            "SELECT COUNT(*) FROM queue_intents WHERE state = 'leased';",
                        );
                        if busy.is_some_and(|count| count > 0) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
                let pending_before = self.db.pending_intents().unwrap_or(-1);
                if let Some(daemon) = self.daemon.as_mut() {
                    daemon.kill()?;
                }
                self.crashes += 1;
                self.note(format!(
                    "{}: SIGKILL with {pending_before} queued intent(s)",
                    fault.label()
                ));
                self.start_daemon()?;
                let pending_after = self.db.pending_intents().unwrap_or(-1);
                self.record_fault_event(
                    fault.label(),
                    format!("queued before {pending_before}, after restart {pending_after}"),
                );
                faults.push(fault.label().to_string());
            }
            FaultKind::Freeze => {
                let seconds = 3 + self.rng.below(12);
                if let Some(daemon) = self.daemon.as_mut() {
                    daemon.signal(Signal::Stop)?;
                }
                self.note(format!("freeze: SIGSTOP for {seconds}s"));
                let until = Instant::now() + Duration::from_secs(seconds);
                while Instant::now() < until {
                    self.write_status(false)?;
                    std::thread::sleep(Duration::from_millis(250));
                }
                if let Some(daemon) = self.daemon.as_mut() {
                    daemon.signal(Signal::Cont)?;
                }
                self.record_fault_event("freeze", format!("{seconds}s"));
                faults.push("freeze".to_string());
            }
            FaultKind::PauseResume => {
                self.cli.pause()?;
                self.note("pause-resume: paused, working on");
                let held = 2 + self.rng.below_usize(6);
                for _ in 0..held {
                    // No nested faults while paused.
                    let saved = std::mem::take(&mut self.cfg.faults.kinds);
                    let result = self.one_op(side, subset, faults);
                    self.cfg.faults.kinds = saved;
                    result?;
                }
                self.cli.resume()?;
                self.record_fault_event("pause-resume", format!("{held} ops while paused"));
                faults.push("pause-resume".to_string());
            }
            FaultKind::ConfigReload => {
                self.cli
                    .config_set("resourceLimits", r#"{"bandwidthPercent": 5}"#)?;
                self.note("config-reload: bandwidth ceiling lowered to 5%");
                let saved = std::mem::take(&mut self.cfg.faults.kinds);
                let result = self.one_op(side, subset, faults);
                self.cfg.faults.kinds = saved;
                result?;
                self.cli
                    .config_set("resourceLimits", r#"{"bandwidthPercent": 25}"#)?;
                self.record_fault_event("config-reload", "bandwidthPercent 5 then 25".to_string());
                faults.push("config-reload".to_string());
            }
            _ => {}
        }
        Ok(())
    }

    fn begin_phase_fault(
        &mut self,
        fault: FaultKind,
        faults: &mut Vec<String>,
    ) -> Result<(), Failure> {
        match fault {
            FaultKind::CloudRootVanish => {
                let parked = self.sandbox.root.join("parked-cloud");
                if self.home.cloud.exists() && fs::rename(&self.home.cloud, &parked).is_ok() {
                    self.cloud_parked = Some(parked);
                    self.note("cloud-root-vanish: cloud root parked for this phase");
                    faults.push("cloud-root-vanish".to_string());
                }
            }
            FaultKind::CloudRootPermissions if self.cloud_parked.is_none() => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = fs::metadata(&self.home.cloud)?.permissions().mode();
                    fs::set_permissions(&self.home.cloud, fs::Permissions::from_mode(0o000))?;
                    self.cloud_locked_mode = Some(mode);
                    self.note("cloud-root-permissions: cloud root unreadable for this phase");
                    self.record_fault_event("cloud-root-permissions", "locked".to_string());
                    faults.push("cloud-root-permissions".to_string());
                }
            }
            FaultKind::ThrottleWalk => {
                if let Some(script) = &self.throttle {
                    let station = *self.rng.pick(Station::WALK);
                    script.set(station)?;
                    self.note(format!("throttle-walk: inputs set to {}", station.label()));
                    self.record_fault_event("throttle-walk", station.label().to_string());
                    faults.push(format!("throttle-walk:{}", station.label()));
                }
            }
            FaultKind::DiskFull if self.image.is_some() && self.cloud_parked.is_none() => {
                let filler = self.home.cloud.join(".soak-filler.bin");
                let mut file = fs::File::create(&filler)?;
                use std::io::Write;
                let chunk = vec![0xA5u8; 1024 * 1024];
                let mut written = 0u64;
                while file.write_all(&chunk).is_ok() {
                    written += chunk.len() as u64;
                    if written > 4 * 1024 * 1024 * 1024 {
                        break;
                    }
                }
                let _ = file.sync_all();
                drop(file);
                self.note(format!(
                    "disk-full: cloud volume filled with {} MiB",
                    written / (1024 * 1024)
                ));
                self.record_fault_event(
                    "disk-full",
                    format!("{} MiB filler", written / (1024 * 1024)),
                );
                faults.push("disk-full".to_string());
            }
            _ => {}
        }
        Ok(())
    }

    fn end_phase_fault(&mut self, fault: FaultKind) -> Result<(), Failure> {
        match fault {
            FaultKind::CloudRootVanish => {
                if let Some(parked) = self.cloud_parked.take() {
                    if self.home.cloud.exists() {
                        // An adopted root is never re-created on Vapor's
                        // own; anything here is a finding.
                        return Err(Failure::new(format!(
                            "the daemon re-created the parked cloud root at {}",
                            self.home.cloud.display()
                        )));
                    }
                    fs::rename(&parked, &self.home.cloud)?;
                    self.note("cloud-root-vanish: cloud root restored");
                    self.record_fault_event("cloud-root-vanish", "restored".to_string());
                    // The daemon notices on its root-check cadence and
                    // lifts the hold on its own; give it that long.
                    if self.daemon_pid().is_some() {
                        self.cli
                            .wait_run_state("Running", Duration::from_secs(90))?;
                    }
                }
            }
            FaultKind::CloudRootPermissions => {
                #[cfg(unix)]
                if let Some(mode) = self.cloud_locked_mode.take() {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&self.home.cloud, fs::Permissions::from_mode(mode))?;
                    self.note("cloud-root-permissions: permissions restored");
                    self.record_fault_event("cloud-root-permissions", "restored".to_string());
                    // An unreadable root is retried at the ensure cadence,
                    // never asked about; the daemon recovers on its own.
                    if self.daemon_pid().is_some() {
                        self.cli
                            .wait_run_state("Running", Duration::from_secs(90))?;
                    }
                }
            }
            FaultKind::ThrottleWalk => {
                if let Some(script) = &self.throttle {
                    script.set(Station::Idle)?;
                }
            }
            FaultKind::DiskFull => {
                let filler = self.home.cloud.join(".soak-filler.bin");
                if filler.exists() {
                    fs::remove_file(&filler)?;
                    self.note("disk-full: filler removed");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn join_relative(root: &Path, relative: &str) -> PathBuf {
    let mut full = root.to_path_buf();
    for segment in relative.split('/') {
        full.push(segment);
    }
    full
}

fn executable_bit(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Re-runs the oracle on a preserved sandbox from its saved model.
pub fn verify(sandbox: &Path) -> Result<Vec<Violation>, Failure> {
    let model = Model::load(&sandbox.join("model.json"))?;
    let status_text = fs::read_to_string(sandbox.join("soak-status.json"))?;
    let status: Status = serde_json::from_str(&status_text)?;
    let _ = status;
    let local = sandbox.join("local");
    let cloud = if sandbox.join("cloud-volume-mnt").join("Vapor").exists() {
        sandbox.join("cloud-volume-mnt").join("Vapor")
    } else {
        sandbox.join("cloud").join("Vapor")
    };
    let _ = FileFacts {
        size: 0,
        executable: false,
        sha256: String::new(),
    };
    model.check(&local, &cloud)
}
