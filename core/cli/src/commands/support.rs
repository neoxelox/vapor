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
/// pretty-printed JSON by the caller. Each endpoint is captured
/// independently: a daemon that answers `status` but fails `timeline`
/// (shutdown mid-capture) still contributes the captures that succeeded,
/// and the per-endpoint failures are recorded rather than discarded.
pub struct LiveCaptures {
    pub status_json: Option<String>,
    pub diagnostics_json: Option<String>,
    pub timeline_json: Option<String>,
    /// Per-endpoint capture failures (`"endpoint: error"`), surfaced in
    /// the manifest so partial reachability is visible to the maintainer.
    pub capture_errors: Vec<String>,
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
    /// Per-endpoint live-capture failures (empty when all succeeded or the
    /// daemon was fully unreachable).
    capture_errors: &'a [String],
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
    let bundle_dir = create_unique_bundle_dir(output_root, timestamp_ms)?;
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

    // Live daemon captures, each independent. "Reachable" is decided by
    // the status call alone — a later endpoint failing must not erase the
    // captures that did land or misreport the daemon as unreachable.
    let capture_errors = live
        .as_ref()
        .map(|live| live.capture_errors.clone())
        .unwrap_or_default();
    let mut daemon_reachable = false;
    if let Some(live) = live {
        daemon_reachable = live.status_json.is_some();
        for (name, contents) in [
            ("status.json", &live.status_json),
            ("diagnostics.json", &live.diagnostics_json),
            ("timeline.json", &live.timeline_json),
        ] {
            if let Some(contents) = contents {
                fs::write(bundle_dir.join(name), contents)?;
                artifacts.push(name.to_string());
            }
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
        capture_errors: &capture_errors,
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

/// Creates a fresh bundle directory, disambiguating a name collision
/// (same-millisecond invocations, or a repeated pre-epoch timestamp) with
/// a numeric suffix. `create_dir` fails on an existing directory (unlike
/// `create_dir_all`), so two bundles can never silently merge into one
/// and leave the manifest describing contents that are not all there.
fn create_unique_bundle_dir(output_root: &Path, timestamp_ms: u64) -> io::Result<PathBuf> {
    fs::create_dir_all(output_root)?;
    let base = format!("vapor-support-{timestamp_ms}");
    for suffix in 0..1_000 {
        let name = if suffix == 0 {
            base.clone()
        } else {
            format!("{base}-{suffix}")
        };
        let candidate = output_root.join(name);
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many support bundles share this timestamp",
    ))
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
            status_json: Some("{\"runState\":\"Running\"}".to_string()),
            diagnostics_json: Some("{\"intents\":[]}".to_string()),
            timeline_json: Some("{\"entries\":[]}".to_string()),
            capture_errors: Vec::new(),
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
    fn partial_live_capture_keeps_successes_and_records_failures() {
        let vapor_dir = TempDir::new().expect("vapor dir");
        let output = TempDir::new().expect("output dir");
        seed_vapor_dir(vapor_dir.path());

        // status succeeded; timeline failed mid-capture.
        let live = LiveCaptures {
            status_json: Some("{\"runState\":\"Running\"}".to_string()),
            diagnostics_json: Some("{\"intents\":[]}".to_string()),
            timeline_json: None,
            capture_errors: vec!["timeline: daemon not responding".to_string()],
        };
        let report = collect_support_bundle(vapor_dir.path(), output.path(), Some(live), 42)
            .expect("bundle collects");

        // Reachability is decided by status, not by the failed endpoint.
        assert!(report.daemon_reachable);
        assert!(report.bundle_dir.join("status.json").is_file());
        assert!(report.bundle_dir.join("diagnostics.json").is_file());
        assert!(!report.bundle_dir.join("timeline.json").exists());

        let manifest =
            fs::read_to_string(report.bundle_dir.join("manifest.json")).expect("manifest");
        let parsed: serde_json::Value = serde_json::from_str(&manifest).expect("manifest json");
        assert_eq!(parsed["daemonReachable"], serde_json::Value::Bool(true));
        assert_eq!(
            parsed["captureErrors"][0],
            serde_json::json!("timeline: daemon not responding")
        );
    }

    #[test]
    fn same_timestamp_bundles_get_distinct_directories() {
        let vapor_dir = TempDir::new().expect("vapor dir");
        let output = TempDir::new().expect("output dir");
        seed_vapor_dir(vapor_dir.path());

        let first = collect_support_bundle(vapor_dir.path(), output.path(), None, 1_000)
            .expect("first bundle");
        let second = collect_support_bundle(vapor_dir.path(), output.path(), None, 1_000)
            .expect("second bundle");

        assert_ne!(
            first.bundle_dir, second.bundle_dir,
            "a repeated timestamp must not merge two bundles into one directory"
        );
        assert!(first.bundle_dir.join("manifest.json").is_file());
        assert!(second.bundle_dir.join("manifest.json").is_file());
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
