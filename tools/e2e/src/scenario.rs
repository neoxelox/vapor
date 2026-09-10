//! The scenario model: what a scenario declares, and the context it
//! runs in. A scenario is one behavior, proven black-box, with its own
//! sandbox. The context owns every daemon the scenario starts and runs
//! the shared epilogue (clean shutdown, tree oracle, log hygiene,
//! failed-intent check) after the scenario body returns.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cli::Cli;
use crate::daemon::{Daemon, DaemonKind};
use crate::db::StateDb;
use crate::host::{Host, Need};
use crate::logs;
use crate::oracle::{OracleOptions, TreeOracle};
use crate::sandbox::{Home, Sandbox};
use crate::wait;
use crate::{Failure, ensure};

/// How long a daemon gets to reach `Running` after spawn.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a converge wait gets by default.
pub const CONVERGE_TIMEOUT: Duration = Duration::from_secs(30);
/// Grace period for a clean SIGTERM shutdown.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
/// How long the queue must stay empty with no new intent for `settle`
/// to call a home quiet. Longer than the code/text and document
/// debounce windows; scenarios that touch files of the "other" class
/// (no or unknown extension, 4 s window) pass a longer quiet window.
pub const SETTLE_QUIET: Duration = Duration::from_millis(3_000);
/// Concurrent transfers per direction a sandbox daemon gets.
pub const SANDBOX_CONCURRENT_TRANSFERS: usize = 2;

/// Enqueue-counter snapshot; see `Ctx::mark`.
#[derive(Clone, Copy, Debug)]
pub struct Mark {
    pub high_water: i64,
}

/// What the scenario's verdict means for the run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Expect {
    /// Must pass.
    Pass,
    /// Documents a known gap: the scenario asserts the behavior the
    /// product should have and is expected to fail until the named
    /// work lands. An unexpected pass is reported so the marker gets
    /// removed. The string names the tracked task.
    KnownGap(&'static str),
}

pub type ScenarioFn = fn(&mut Ctx) -> Result<(), Failure>;

#[derive(Clone, Copy, Debug)]
pub struct Scenario {
    /// Stable id (`S03`, `R02`) used by `--only` and in reports.
    pub id: &'static str,
    /// Kebab-case name.
    pub name: &'static str,
    /// One line stating the invariant a pass proves.
    pub proves: &'static str,
    pub needs: &'static [Need],
    pub expect: Expect,
    pub run: ScenarioFn,
}

/// Which trees the epilogue compares.
#[derive(Clone, Debug)]
pub enum OracleMode {
    /// Every home the scenario started a daemon for, local vs cloud.
    AllHomes,
    /// Only these homes (by label).
    Homes(Vec<String>),
    /// No comparison, with the reason a reader will see.
    Skip(String),
}

/// Paths and binaries the run provides to every scenario.
#[derive(Clone, Debug)]
pub struct RunPaths {
    pub repo_root: PathBuf,
    pub cli_bin: PathBuf,
    pub vapord_bin: PathBuf,
    /// The daemon flavour scenarios start by default.
    pub daemon_kind: DaemonKind,
}

pub struct Ctx {
    pub host: Host,
    pub paths: RunPaths,
    pub sandbox: Sandbox,
    pub primary: Home,
    daemons: Vec<Daemon>,
    homes: BTreeMap<String, Home>,
    allowed_warnings: Vec<String>,
    allowed_errors: Vec<String>,
    /// Homes whose `failed_intents` may be non-zero at the end.
    tolerate_failed_intents: Vec<String>,
    oracle_mode: OracleMode,
    oracle_options: OracleOptions,
    pub notes: Vec<String>,
    /// Resources (disk images) that must outlive the daemons and are
    /// released after the epilogue.
    keep_alive: Vec<Box<dyn std::any::Any>>,
}

impl Ctx {
    pub fn new(host: Host, paths: RunPaths, sandbox: Sandbox) -> Result<Self, Failure> {
        let primary = sandbox.home("primary")?;
        let mut homes = BTreeMap::new();
        homes.insert(primary.label.clone(), primary.clone());
        Ok(Self {
            host,
            paths,
            sandbox,
            primary,
            daemons: Vec::new(),
            homes,
            allowed_warnings: Vec::new(),
            allowed_errors: Vec::new(),
            tolerate_failed_intents: Vec::new(),
            oracle_mode: OracleMode::AllHomes,
            oracle_options: OracleOptions {
                extra_ignore_rules: Vec::new(),
                compare_mode: cfg!(unix),
            },
            notes: Vec::new(),
            keep_alive: Vec::new(),
        })
    }

    // ----- homes and processes -----

    /// Provisions an additional home under this scenario's sandbox.
    pub fn new_home(&mut self, label: &str) -> Result<Home, Failure> {
        ensure!(
            !self.homes.contains_key(label),
            "home label {label:?} already used in this scenario"
        );
        let home = self.sandbox.home(label)?;
        self.homes.insert(label.to_string(), home.clone());
        Ok(home)
    }

    pub fn register_home(&mut self, home: Home) {
        self.homes.insert(home.label.clone(), home);
    }

    pub fn home(&self, label: &str) -> Option<&Home> {
        self.homes.get(label)
    }

    /// The CLI bound to the primary home.
    pub fn cli(&self) -> Cli {
        Cli::new(&self.paths.cli_bin, &self.primary)
    }

    pub fn cli_for(&self, home: &Home) -> Cli {
        Cli::new(&self.paths.cli_bin, home)
    }

    pub fn db(&self) -> StateDb {
        StateDb::at(&self.primary.state_db())
    }

    pub fn db_for(&self, home: &Home) -> StateDb {
        StateDb::at(&home.state_db())
    }

    /// Writes the sync scope for a home through `vapor config set`, the
    /// way a user would.
    pub fn configure_scope(&self, home: &Home) -> Result<(), Failure> {
        let cli = self.cli_for(home);
        cli.config_set("localSyncDirectory", &home.local.to_string_lossy())?;
        cli.config_set("cloudSyncDirectory", &home.cloud.to_string_lossy())?;
        // A sandbox is a small computer: two transfers at a time, so
        // the scenarios sharing the host do not each spin up a core's
        // worth of workers. The workgate's own scaling is Tier 1's.
        cli.config_set(
            "resourceLimits",
            &format!(
                "{{\"maxConcurrentTransfers\": {}}}",
                SANDBOX_CONCURRENT_TRANSFERS
            ),
        )?;
        Ok(())
    }

    /// Starts the run's default daemon flavour for the primary home and
    /// waits for `Running`.
    pub fn start_daemon(&mut self) -> Result<usize, Failure> {
        let home = self.primary.clone();
        self.start_daemon_in(&home, self.paths.daemon_kind, true)
    }

    /// Starts a daemon for `home`. With `wait_running`, fails unless the
    /// daemon answers `Running` within [`STARTUP_TIMEOUT`]. Returns an
    /// index for later control.
    pub fn start_daemon_in(
        &mut self,
        home: &Home,
        kind: DaemonKind,
        wait_running: bool,
    ) -> Result<usize, Failure> {
        self.homes
            .entry(home.label.clone())
            .or_insert_with(|| home.clone());
        let daemon = Daemon::spawn(
            kind,
            &self.paths.cli_bin,
            &self.paths.vapord_bin,
            home,
            &self.sandbox.root,
        )?;
        self.daemons.push(daemon);
        let index = self.daemons.len() - 1;
        if wait_running {
            let cli = self.cli_for(home);
            if let Err(error) = cli.wait_run_state("Running", STARTUP_TIMEOUT) {
                let alive = self.daemons[index].is_alive();
                return Err(Failure::new(format!(
                    "{error} (daemon alive: {alive}; see {})",
                    self.daemons[index].output_path.display()
                )));
            }
        }
        Ok(index)
    }

    pub fn daemon(&mut self, index: usize) -> &mut Daemon {
        &mut self.daemons[index]
    }

    /// Clean SIGTERM shutdown; fails if the daemon did not exit within
    /// the grace period.
    pub fn stop_daemon(&mut self, index: usize) -> Result<(), Failure> {
        let status = self.daemons[index].terminate(SHUTDOWN_GRACE)?;
        ensure!(
            status.success(),
            "daemon {} exited {status} on SIGTERM instead of cleanly",
            self.daemons[index].label
        );
        Ok(())
    }

    /// Simulated crash.
    pub fn kill_daemon(&mut self, index: usize) -> Result<(), Failure> {
        self.daemons[index].kill().map(|_| ())
    }

    // ----- waiting -----

    pub fn wait_until(
        &self,
        timeout: Duration,
        description: &str,
        probe: impl FnMut() -> bool,
    ) -> Result<(), Failure> {
        wait::wait_until(timeout, description, probe)
    }

    /// Snapshot of the primary home's enqueue counter, taken before the
    /// scenario makes changes, so `converge_from` can wait for the
    /// intents those changes produce.
    pub fn mark(&self) -> Mark {
        self.mark_home(&self.primary)
    }

    pub fn mark_home(&self, home: &Home) -> Mark {
        Mark {
            high_water: self.db_for(home).enqueue_high_water().unwrap_or(0),
        }
    }

    /// Waits until at least `expected` intents were enqueued since
    /// `mark`, then hints a drain and waits for the queue to empty.
    /// The first wait matters: a queue that is empty because the
    /// debounce window has not elapsed is not a converged queue.
    pub fn converge_from(
        &self,
        mark: &Mark,
        expected: i64,
        timeout: Duration,
    ) -> Result<(), Failure> {
        let home = self.primary.clone();
        self.converge_home_from(&home, mark, expected, timeout)
    }

    pub fn converge_home_from(
        &self,
        home: &Home,
        mark: &Mark,
        expected: i64,
        timeout: Duration,
    ) -> Result<(), Failure> {
        let db = self.db_for(home);
        let baseline = mark.high_water;
        wait::wait_until(
            timeout,
            &format!(
                "{expected} durable intent(s) to be captured on home '{}'",
                home.label
            ),
            || {
                db.enqueue_high_water()
                    .is_some_and(|current| current - baseline >= expected)
            },
        )?;
        self.drain_home(home, timeout)
    }

    /// Hints a drain and waits for the queue to empty. Only meaningful
    /// once the intents are known to be enqueued (see `converge_from`
    /// and `settle`).
    pub fn drain_home(&self, home: &Home, timeout: Duration) -> Result<(), Failure> {
        let cli = self.cli_for(home);
        let db = self.db_for(home);
        cli.flush_now();
        wait::wait_until(
            timeout,
            &format!(
                "durable queue of home '{}' to drain to zero pending intents",
                home.label
            ),
            || db.queue_drained(),
        )
    }

    /// Waits until the primary home is quiet: no intent enqueued and
    /// the queue empty for `SETTLE_QUIET`, within `timeout`. Use when
    /// the number of intents a change produces is not predictable
    /// (renames, directory operations, reconciles).
    pub fn settle(&self, timeout: Duration) -> Result<(), Failure> {
        let home = self.primary.clone();
        self.settle_home(&home, SETTLE_QUIET, timeout)
    }

    pub fn settle_home(
        &self,
        home: &Home,
        quiet: Duration,
        timeout: Duration,
    ) -> Result<(), Failure> {
        let db = self.db_for(home);
        let cli = self.cli_for(home);
        let timeout = timeout.mul_f64(f64::from(wait::deadline_scale_percent()) / 100.0);
        let deadline = std::time::Instant::now() + timeout;
        loop {
            cli.flush_now();
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            ensure!(
                !remaining.is_zero(),
                "home '{}' did not settle within {}s (queue {:?}, enqueued {:?})",
                home.label,
                timeout.as_secs(),
                db.pending_intents(),
                db.enqueue_high_water()
            );
            wait::wait_until(
                remaining,
                &format!("durable queue of home '{}' to drain", home.label),
                || db.queue_drained(),
            )?;
            let high_water = db.enqueue_high_water();
            let quiet_deadline = std::time::Instant::now() + quiet;
            let mut still_quiet = true;
            while std::time::Instant::now() < quiet_deadline {
                std::thread::sleep(wait::POLL_INTERVAL);
                if db.enqueue_high_water() != high_water || !db.queue_drained() {
                    still_quiet = false;
                    break;
                }
            }
            if still_quiet {
                return Ok(());
            }
        }
    }

    /// Waits until the file exists (any kind).
    pub fn wait_exists(&self, path: &Path, timeout: Duration) -> Result<(), Failure> {
        wait::wait_until(timeout, &format!("{} to exist", path.display()), || {
            path.exists()
        })
    }

    pub fn wait_absent(&self, path: &Path, timeout: Duration) -> Result<(), Failure> {
        wait::wait_until(
            timeout,
            &format!("{} to be removed", path.display()),
            || !path.exists(),
        )
    }

    /// Waits until both files exist and hold identical bytes.
    pub fn wait_same_content(&self, a: &Path, b: &Path, timeout: Duration) -> Result<(), Failure> {
        wait::wait_until(
            timeout,
            &format!("{} and {} to hold the same bytes", a.display(), b.display()),
            || match (fs::read(a), fs::read(b)) {
                (Ok(left), Ok(right)) => left == right,
                _ => false,
            },
        )
    }

    // ----- epilogue configuration -----

    /// Declares a warning substring this scenario expects in a daemon
    /// log (a keep-both resolution, a restart-required key).
    pub fn allow_warning(&mut self, pattern: &str) {
        self.allowed_warnings.push(pattern.to_string());
    }

    pub fn allow_error(&mut self, pattern: &str) {
        self.allowed_errors.push(pattern.to_string());
    }

    pub fn tolerate_failed_intents(&mut self, home_label: &str) {
        self.tolerate_failed_intents.push(home_label.to_string());
    }

    pub fn set_oracle(&mut self, mode: OracleMode) {
        self.oracle_mode = mode;
    }

    pub fn skip_oracle(&mut self, reason: &str) {
        self.oracle_mode = OracleMode::Skip(reason.to_string());
    }

    pub fn oracle_ignore(&mut self, rule: &str) {
        self.oracle_options
            .extra_ignore_rules
            .push(rule.to_string());
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.notes.push(text.into());
    }

    /// Holds a resource until after the epilogue has stopped every
    /// daemon (a mounted disk image the daemon's roots live on).
    pub fn keep_alive(&mut self, resource: Box<dyn std::any::Any>) {
        self.keep_alive.push(resource);
    }

    /// Releases held resources; called by the runner after diagnostics.
    pub fn release(&mut self) {
        self.daemons.clear();
        self.keep_alive.clear();
    }

    // ----- epilogue -----

    /// Stops every daemon still running (clean SIGTERM; a daemon that
    /// ignores it fails the scenario), then compares the trees and scans
    /// the logs. Returns every finding so a scenario that passed its own
    /// assertions can still fail on the shared invariants.
    pub fn epilogue(&mut self) -> Vec<Failure> {
        let mut findings = Vec::new();
        for index in 0..self.daemons.len() {
            if self.daemons[index].is_alive()
                && let Err(error) = self.daemons[index].terminate(SHUTDOWN_GRACE)
            {
                findings.push(Failure::new(format!("epilogue: {error}")));
            }
        }

        let started_labels: Vec<String> = self
            .daemons
            .iter()
            .map(|daemon| daemon.label.clone())
            .collect();

        match self.oracle_mode.clone() {
            OracleMode::Skip(reason) => self.notes.push(format!("oracle skipped: {reason}")),
            OracleMode::AllHomes | OracleMode::Homes(_) => {
                let labels: Vec<String> = match &self.oracle_mode {
                    OracleMode::Homes(labels) => labels.clone(),
                    _ => started_labels.clone(),
                };
                match TreeOracle::new(&self.oracle_options) {
                    Err(error) => findings.push(error),
                    Ok(oracle) => {
                        let mut seen = std::collections::BTreeSet::new();
                        for label in labels {
                            if !seen.insert(label.clone()) {
                                continue;
                            }
                            let Some(home) = self.homes.get(&label).cloned() else {
                                continue;
                            };
                            match oracle.compare(&home.local, &home.cloud) {
                                Err(error) => findings.push(Failure::new(format!(
                                    "oracle on home '{label}': {error}"
                                ))),
                                Ok(report) => {
                                    if !report.is_clean() {
                                        findings.push(Failure::new(format!(
                                            "oracle on home '{label}': {}",
                                            report.summary(10)
                                        )));
                                    } else {
                                        self.notes.push(format!(
                                            "oracle '{label}': {}",
                                            report.summary(0)
                                        ));
                                    }
                                    for skipped in report.skipped {
                                        self.notes.push(format!(
                                            "oracle '{label}' skipped special file {skipped}"
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        for (label, home) in self.homes.clone() {
            let log_path = home.daemon_log();
            if log_path.exists() {
                let report = logs::scan(&log_path, &self.allowed_warnings, &self.allowed_errors);
                for line in &report.errors {
                    findings.push(Failure::new(format!(
                        "log hygiene on home '{label}': ERROR line: {line}"
                    )));
                }
                for line in &report.unexpected_warnings {
                    findings.push(Failure::new(format!(
                        "log hygiene on home '{label}': unexpected WARNING: {line}"
                    )));
                }
            }
            if !self.tolerate_failed_intents.contains(&label) {
                let db = StateDb::at(&home.state_db());
                if db.exists() {
                    match db.failed_intents() {
                        Some(0) | None => {}
                        Some(count) => findings.push(Failure::new(format!(
                            "home '{label}' ended with {count} failed intent(s): {}",
                            db.rows("SELECT path_text, kind, last_error FROM failed_intents;")
                                .join("; ")
                        ))),
                    }
                }
            }
        }
        findings
    }

    /// Everything a reader needs to debug a failure: status and
    /// diagnostics of every live daemon, queue rows, log tails.
    pub fn diagnostics(&mut self) -> String {
        let mut out = Vec::new();
        out.push(format!("sandbox: {}", self.sandbox.root.display()));
        for (label, home) in self.homes.clone() {
            out.push(format!("---- home '{label}' ({})", home.dir.display()));
            let daemon_alive = self
                .daemons
                .iter_mut()
                .any(|daemon| daemon.label == label && daemon.is_alive());
            if daemon_alive {
                let cli = self.cli_for(&home);
                for args in [["status", "--json"], ["diagnostics", "--json"]] {
                    if let Ok(output) = cli.run(&args) {
                        out.push(format!("vapor {}:", args.join(" ")));
                        out.extend(output.combined().lines().map(|line| format!("  {line}")));
                    }
                }
            }
            let db = StateDb::at(&home.state_db());
            if db.exists() {
                let rows = db.rows("SELECT id, path_text, kind, state, attempt_count, last_error FROM queue_intents;");
                out.push(format!("queue_intents ({} rows):", rows.len()));
                out.extend(rows.into_iter().map(|row| format!("  {row}")));
                let failed = db.rows("SELECT path_text, kind, last_error FROM failed_intents;");
                if !failed.is_empty() {
                    out.push(format!("failed_intents ({} rows):", failed.len()));
                    out.extend(failed.into_iter().map(|row| format!("  {row}")));
                }
            }
            let log_path = home.daemon_log();
            if log_path.exists() {
                out.push("last 60 daemon log lines (polling chatter left out):".to_string());
                out.extend(
                    logs::tail_of_interest(&log_path, 60)
                        .into_iter()
                        .map(|line| format!("  {line}")),
                );
            }
            let output_path = self.sandbox.root.join(format!("{label}-daemon.out"));
            let tail = logs::tail(&output_path, 20);
            if !tail.is_empty() {
                out.push("daemon stdout/stderr tail:".to_string());
                out.extend(tail.into_iter().map(|line| format!("  {line}")));
            }
        }
        out.join("\n")
    }
}

/// Writes `bytes` to `path`, creating parent directories.
pub fn write_file(path: &Path, bytes: impl AsRef<[u8]>) -> Result<(), Failure> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}

/// Reads a file as a string, or fails with the path.
pub fn read_string(path: &Path) -> Result<String, Failure> {
    fs::read_to_string(path)
        .map_err(|error| Failure::new(format!("cannot read {}: {error}", path.display())))
}

/// `true` when some file under `root` whose name starts with
/// `<stem>~conflict-` exists.
pub fn conflict_copy_exists(root: &Path, stem: &str) -> bool {
    conflict_copies(root, stem).next().is_some()
}

pub fn conflict_copies(root: &Path, stem: &str) -> impl Iterator<Item = PathBuf> {
    let prefix = format!("{stem}~conflict-");
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(move |entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| entry.path())
}

/// `true` when `haystack_root` (recursively) contains a file whose
/// contents equal `needle`.
pub fn tree_contains_content(root: &Path, needle: &[u8]) -> bool {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() && fs::read(&path).is_ok_and(|bytes| bytes == needle) {
                return true;
            }
        }
    }
    false
}
