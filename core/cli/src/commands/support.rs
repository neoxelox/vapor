//! `vapor support-bundle`: diagnostics history + support
//! export.
//!
//! Collects everything a maintainer needs to triage a report into one
//! shareable directory: the runtime config, the daemon logs, and — when
//! the daemon is reachable — live status / per-intent diagnostics / the
//! activity timeline, plus a manifest describing what was (and was not)
//! captured.
//!
//! Privacy posture: the config carries no secrets (tokens live only in
//! the platform secret store, §6) and the daemon logs are written
//! through the redacting logger. The bundle therefore contains no
//! credentials by construction; the manifest states this so users know
//! what they are sharing.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Live captures from the running daemon, already serialized to
/// pretty-printed JSON by the caller. `None` means the daemon was
/// unreachable — the bundle is still produced from on-disk artifacts.
pub struct LiveCaptures {
    pub status_json: String,
    pub diagnostics_json: String,
    pub timeline_json: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportBundleReport {
    pub bundle_dir: PathBuf,
    /// Bundle-relative paths of everything captured.
    pub artifacts: Vec<String>,
    pub daemon_reachable: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BundleManifest<'a> {
    created_at_ms: u64,
    vapor_version: &'a str,
    os: &'a str,
    arch: &'a str,
    daemon_reachable: bool,
    artifacts: &'a [String],
    /// Human-facing note on what the bundle can and cannot contain.
    redaction: &'a str,
}

/// Collects the bundle under `output_root/vapor-support-{timestamp}/`.
///
/// Pure filesystem work over an explicit `vapor_dir` — no IPC, no
/// network — so tests drive it hermetically; `main.rs` supplies the
/// live captures.
pub fn collect_support_bundle(
    vapor_dir: &Path,
    output_root: &Path,
    live: Option<LiveCaptures>,
    timestamp_ms: u64,
) -> io::Result<SupportBundleReport> {
    let bundle_dir = output_root.join(format!("vapor-support-{timestamp_ms}"));
    fs::create_dir_all(&bundle_dir)?;
    let mut artifacts = Vec::new();

    // Runtime config (no secrets by design; tokens live in the secret
    // store).
    let config_path = vapor_dir.join(vapor_shared::constants::runtime::CONFIGURATION_FILE_NAME);
    if config_path.is_file() {
        fs::copy(
            &config_path,
            bundle_dir.join(vapor_shared::constants::runtime::CONFIGURATION_FILE_NAME),
        )?;
        artifacts.push(vapor_shared::constants::runtime::CONFIGURATION_FILE_NAME.to_string());
    }

    // Daemon/CLI logs (written through the redacting logger).
    let logs_dir = vapor_dir.join(vapor_shared::constants::runtime::LOGS_DIRECTORY_NAME);
    if logs_dir.is_dir() {
        let bundle_logs = bundle_dir.join(vapor_shared::constants::runtime::LOGS_DIRECTORY_NAME);
        fs::create_dir_all(&bundle_logs)?;
        let mut entries: Vec<_> = fs::read_dir(&logs_dir)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name();
            fs::copy(entry.path(), bundle_logs.join(&name))?;
            artifacts.push(format!(
                "{}/{}",
                vapor_shared::constants::runtime::LOGS_DIRECTORY_NAME,
                name.to_string_lossy()
            ));
        }
    }

    // Live daemon captures (status / diagnostics / timeline).
    let daemon_reachable = live.is_some();
    if let Some(live) = live {
        for (name, contents) in [
            ("status.json", &live.status_json),
            ("diagnostics.json", &live.diagnostics_json),
            ("timeline.json", &live.timeline_json),
        ] {
            fs::write(bundle_dir.join(name), contents)?;
            artifacts.push(name.to_string());
        }
    }

    // The manifest lists itself, so its artifact array matches both the
    // report and the actual bundle contents.
    artifacts.push("manifest.json".to_string());
    let manifest = BundleManifest {
        created_at_ms: timestamp_ms,
        vapor_version: vapor_daemon::build_info::VERSION,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        daemon_reachable,
        artifacts: &artifacts,
        redaction: "config carries no credentials (tokens live in the platform secret \
                    store) and logs are written through the redacting logger; review \
                    file paths in logs/diagnostics before sharing if they are sensitive",
    };
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|error| io::Error::other(format!("cannot serialize manifest: {error}")))?;
    fs::write(bundle_dir.join("manifest.json"), manifest_json)?;

    Ok(SupportBundleReport {
        bundle_dir,
        artifacts,
        daemon_reachable,
    })
}

/// Render the report for the human (non-`--json`) path.
pub fn render_report(report: &SupportBundleReport) -> String {
    let mut out = format!(
        "support bundle written to {}\n",
        report.bundle_dir.display()
    );
    out.push_str(&format!(
        "daemon: {}\n",
        if report.daemon_reachable {
            "reachable (live status/diagnostics/timeline captured)"
        } else {
            "not reachable (bundle contains on-disk artifacts only)"
        }
    ));
    for artifact in &report.artifacts {
        out.push_str(&format!("  + {artifact}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn seed_vapor_dir(root: &Path) {
        fs::create_dir_all(root.join("logs")).expect("logs dir");
        fs::write(root.join("vapor.json"), "{\"languageCode\":\"en\"}").expect("config");
        fs::write(root.join("logs/vapord.logs"), "line-1\n").expect("log file");
        fs::write(root.join("logs/vapor-cli.logs"), "line-2\n").expect("log file");
    }

    #[test]
    fn bundle_collects_config_logs_and_manifest_without_a_daemon() {
        let vapor_dir = TempDir::new().expect("vapor dir");
        let output = TempDir::new().expect("output dir");
        seed_vapor_dir(vapor_dir.path());

        let report = collect_support_bundle(vapor_dir.path(), output.path(), None, 1_234)
            .expect("bundle collects");

        assert!(!report.daemon_reachable);
        assert!(report.bundle_dir.ends_with("vapor-support-1234"));
        assert!(report.bundle_dir.join("vapor.json").is_file());
        assert!(report.bundle_dir.join("logs/vapord.logs").is_file());
        assert!(report.bundle_dir.join("logs/vapor-cli.logs").is_file());
        assert!(report.bundle_dir.join("manifest.json").is_file());
        assert!(report.artifacts.contains(&"vapor.json".to_string()));
        assert!(report.artifacts.contains(&"manifest.json".to_string()));

        let manifest =
            fs::read_to_string(report.bundle_dir.join("manifest.json")).expect("manifest");
        let parsed: serde_json::Value = serde_json::from_str(&manifest).expect("manifest json");
        assert_eq!(parsed["daemonReachable"], serde_json::Value::Bool(false));
        assert_eq!(parsed["createdAtMs"], serde_json::json!(1_234));
        assert!(parsed["artifacts"].as_array().expect("array").len() >= 4);
    }

    #[test]
    fn live_captures_land_as_status_diagnostics_and_timeline_files() {
        let vapor_dir = TempDir::new().expect("vapor dir");
        let output = TempDir::new().expect("output dir");
        seed_vapor_dir(vapor_dir.path());

        let live = LiveCaptures {
            status_json: "{\"runState\":\"Running\"}".to_string(),
            diagnostics_json: "{\"intents\":[]}".to_string(),
            timeline_json: "{\"entries\":[]}".to_string(),
        };
        let report = collect_support_bundle(vapor_dir.path(), output.path(), Some(live), 99)
            .expect("bundle collects");

        assert!(report.daemon_reachable);
        for name in ["status.json", "diagnostics.json", "timeline.json"] {
            assert!(report.bundle_dir.join(name).is_file(), "missing {name}");
            assert!(report.artifacts.contains(&name.to_string()));
        }
    }

    #[test]
    fn missing_config_and_logs_are_tolerated() {
        let vapor_dir = TempDir::new().expect("vapor dir");
        let output = TempDir::new().expect("output dir");

        let report = collect_support_bundle(vapor_dir.path(), output.path(), None, 7)
            .expect("bundle collects from an empty runtime dir");

        // Only the manifest is guaranteed.
        assert_eq!(report.artifacts, vec!["manifest.json".to_string()]);
        assert!(report.bundle_dir.join("manifest.json").is_file());
    }

    #[test]
    fn report_json_shape_is_camel_case_for_the_json_flag() {
        let report = SupportBundleReport {
            bundle_dir: PathBuf::from("/tmp/vapor-support-1"),
            artifacts: vec!["manifest.json".to_string()],
            daemon_reachable: false,
        };
        let value = serde_json::to_value(&report).expect("serializes");
        assert!(value.get("bundleDir").is_some());
        assert!(value.get("daemonReachable").is_some());
        assert!(value.get("artifacts").is_some());
    }
}
