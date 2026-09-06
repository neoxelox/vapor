//! What a run tells the outside: the status file it rewrites while it
//! runs and the report it writes at the end (or at the first
//! violation). Both are JSON; an agent reads them instead of the
//! daemon.

use std::path::Path;

use crate::Failure;
use crate::health::{HealthSample, HealthSummary};
use crate::model::Violation;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunState {
    Starting,
    Running,
    /// The first violation froze the workload; the daemon is still up
    /// and the sandbox is preserved for investigation.
    Frozen,
    Finished,
    /// The driver itself could not continue (a setup failure).
    Failed,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FaultRecord {
    pub kind: String,
    pub phase: String,
    pub at_seconds: f64,
    pub detail: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PhaseRecord {
    pub index: usize,
    pub name: String,
    pub ops: usize,
    pub seconds: f64,
    pub converge_seconds: f64,
    pub violations: Vec<Violation>,
    pub faults: Vec<String>,
    /// Daemon warnings the driver did not expect, unique, capped.
    pub unexpected_warnings: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Status {
    pub schema_version: u32,
    pub run_id: String,
    pub state: RunState,
    pub message: String,
    pub seed: u64,
    pub mode: String,
    pub load: String,
    pub faults: Vec<String>,
    pub throttle: String,
    pub sandbox: String,
    pub daemon: String,
    pub daemon_pid: Option<u32>,
    pub started_at_unix: u64,
    pub updated_at_unix: u64,
    pub elapsed_seconds: f64,
    pub planned_seconds: f64,
    pub phase_index: usize,
    pub phase_name: String,
    pub phases_done: usize,
    pub ops_done: u64,
    pub files_live: usize,
    pub crashes: u32,
    pub faults_injected: usize,
    pub violations_total: usize,
    pub last_verdict: Option<String>,
    pub health_latest: Option<HealthSample>,
    pub health: HealthSummary,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SloChecks {
    /// SLO-3: daemon RSS p95 stays under the budget.
    pub rss_p95_within_budget: bool,
    pub rss_budget_bytes: u64,
    /// SLO-2: average CPU under load stays under the budget.
    pub cpu_avg_within_budget: bool,
    pub cpu_budget_percent: f64,
    /// SLO-5: every injected crash was followed by a converged phase
    /// with no loss.
    pub crashes_recovered_without_loss: bool,
    /// Every phase converged inside its deadline.
    pub every_phase_converged: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Report {
    pub status: Status,
    pub phases: Vec<PhaseRecord>,
    pub faults: Vec<FaultRecord>,
    pub violations: Vec<Violation>,
    pub slo: SloChecks,
    pub daemon_errors: Vec<String>,
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), Failure> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_string_pretty(value)?)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}
