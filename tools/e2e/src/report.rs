//! Machine-readable result of a run. Written as JSON next to the
//! sandbox and printed as one line per scenario, so an agent can parse
//! the outcome instead of grepping `PASS`.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Pass,
    Fail,
    Skip,
    /// Expected failure of a scenario marked as a known gap.
    KnownGap,
    /// A known-gap scenario passed: the marker must be removed.
    UnexpectedPass,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Skip => "SKIP",
            Verdict::KnownGap => "KNOWN-GAP",
            Verdict::UnexpectedPass => "FIXED?",
        }
    }

    /// Whether this verdict makes the run red.
    pub fn is_red(self) -> bool {
        matches!(self, Verdict::Fail | Verdict::UnexpectedPass)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub id: String,
    pub name: String,
    pub proves: String,
    pub verdict: Verdict,
    /// Why it was skipped, or the failure message(s).
    #[serde(default)]
    pub reasons: Vec<String>,
    pub seconds: f64,
    /// Preserved sandbox path, when the run kept it.
    #[serde(default)]
    pub sandbox: Option<String>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostSummary {
    pub os: String,
    pub arch: String,
    pub native_watcher: bool,
    pub case_insensitive_fs: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Summary {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub known_gaps: usize,
    pub unexpected_passes: usize,
    pub seconds: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunReport {
    pub schema_version: u32,
    pub run_id: String,
    pub provider: String,
    pub daemon: String,
    pub full: bool,
    pub host: HostSummary,
    pub sandbox_root: String,
    pub scenarios: Vec<ScenarioResult>,
    pub summary: Summary,
}

impl RunReport {
    pub fn is_green(&self) -> bool {
        self.summary.failed == 0 && self.summary.unexpected_passes == 0
    }

    pub fn recompute_summary(&mut self) {
        let mut summary = Summary::default();
        for result in &self.scenarios {
            match result.verdict {
                Verdict::Pass => summary.passed += 1,
                Verdict::Fail => summary.failed += 1,
                Verdict::Skip => summary.skipped += 1,
                Verdict::KnownGap => summary.known_gaps += 1,
                Verdict::UnexpectedPass => summary.unexpected_passes += 1,
            }
            summary.seconds += result.seconds;
        }
        self.summary = summary;
    }

    pub fn write(&self, path: &Path) -> Result<(), crate::Failure> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let rendered = serde_json::to_string_pretty(self)?;
        std::fs::write(path, rendered)?;
        Ok(())
    }
}

/// One console line per scenario, stable enough to read in CI logs.
pub fn console_line(result: &ScenarioResult) -> String {
    let head = format!(
        "[e2e] {} {} — {}",
        result.verdict.label(),
        result.id,
        match result.verdict {
            Verdict::Pass | Verdict::UnexpectedPass => result.proves.clone(),
            Verdict::Fail | Verdict::KnownGap => result
                .reasons
                .first()
                .cloned()
                .unwrap_or_else(|| result.proves.clone()),
            Verdict::Skip => result
                .reasons
                .first()
                .cloned()
                .unwrap_or_else(|| "skipped".to_string()),
        }
    );
    if matches!(result.verdict, Verdict::Skip) {
        head
    } else {
        format!("{head} ({:.1}s)", result.seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_counts_every_verdict_and_red_means_fail_or_unexpected_pass() {
        let mut report = RunReport {
            schema_version: 1,
            run_id: "r".into(),
            provider: "filesystem".into(),
            daemon: "vapor-run".into(),
            full: false,
            host: HostSummary {
                os: "macos".into(),
                arch: "aarch64".into(),
                native_watcher: true,
                case_insensitive_fs: true,
            },
            sandbox_root: "/tmp/x".into(),
            scenarios: [
                Verdict::Pass,
                Verdict::Skip,
                Verdict::KnownGap,
                Verdict::UnexpectedPass,
            ]
            .into_iter()
            .enumerate()
            .map(|(index, verdict)| ScenarioResult {
                id: format!("S{index}"),
                name: "n".into(),
                proves: "p".into(),
                verdict,
                reasons: Vec::new(),
                seconds: 1.0,
                sandbox: None,
                notes: Vec::new(),
            })
            .collect(),
            summary: Summary::default(),
        };
        report.recompute_summary();
        assert_eq!(report.summary.passed, 1);
        assert_eq!(report.summary.skipped, 1);
        assert_eq!(report.summary.known_gaps, 1);
        assert_eq!(report.summary.unexpected_passes, 1);
        assert!(
            !report.is_green(),
            "an unexpected pass must make the run red"
        );
        assert!(Verdict::Fail.is_red());
        assert!(!Verdict::KnownGap.is_red());
    }
}
