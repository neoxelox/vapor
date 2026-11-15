//! `vapor report`: a support report to share when asking for help.
//!
//! Collects everything a maintainer needs to triage a report into one
//! shareable directory: the runtime config, the daemon logs, and, when
//! the daemon is reachable: live status / per-intent diagnostics / the
//! activity timeline, plus a manifest describing what was (and was not)
//! captured.
//!
//! Privacy posture: the config carries no secrets (tokens live only in
//! the platform secret store, §6) and the daemon logs are written
//! through the redacting logger, so the report holds no credential. It
//! does hold the user's folder paths and file names: in `vapor.json`,
//! in every log line, and in the live state. A report attached to a
//! public issue is public, so those names are replaced by stable codes
//! unless the user asks to keep them (`redact.rs`). The command says
//! which of the two it wrote, on the terminal and in the manifest.

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
pub struct SupportReport {
    pub path: PathBuf,
    /// Report-relative paths of everything captured.
    pub artifacts: Vec<String>,
    pub daemon_reachable: bool,
    /// Whether file and folder names were replaced by codes.
    pub paths_redacted: bool,
}

/// What the report does with file and folder names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Paths {
    /// Replace them by stable codes. The default.
    Redact,
    /// Keep them as they are.
    Keep,
}

/// How many reports stay in the default reports directory. Each one
/// can hold every log file; the oldest go when a new one is written.
pub const REPORTS_KEPT: usize = 5;

const REDACTED_NOTE: &str = "no credentials: tokens live in the platform secret store, and the \
     logs are written through the redacting logger. File and folder names are replaced by \
     stable codes (p-xxxxxxxx). A name that stands alone in a sentence can remain when it is \
     one plain lowercase word, or when no path in the report holds it: read the report \
     before you share it";
const KEPT_NOTE: &str = "no credentials: tokens live in the platform secret store, and the logs \
     are written through the redacting logger. This report was written with the paths kept: \
     it holds the sync folders' paths and the name of every file that synced. Do not attach \
     it to a public issue";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportManifest<'a> {
    created_at_ms: u64,
    vapor_version: &'a str,
    os: &'a str,
    arch: &'a str,
    daemon_reachable: bool,
    artifacts: &'a [String],
    /// Per-endpoint live-capture failures (empty when all succeeded or the
    /// daemon was fully unreachable).
    capture_errors: &'a [String],
    /// Whether file and folder names were replaced by codes.
    paths_redacted: bool,
    /// Human-facing note on what the report can and cannot contain.
    redaction: &'a str,
}

/// Writes the report under `output_root/vapor-report-{timestamp}/`.
///
/// Pure filesystem work over an explicit `vapor_dir`, no IPC, no
/// network, so tests drive it hermetically; `main.rs` supplies the
/// live captures.
pub fn write_support_report(
    vapor_dir: &Path,
    output_root: &Path,
    live: Option<LiveCaptures>,
    timestamp_ms: u64,
    paths: Paths,
) -> io::Result<SupportReport> {
    let path = create_unique_report_dir(output_root, timestamp_ms)?;
    let mut artifacts = Vec::new();

    let config_name = vapor_shared::constants::runtime::CONFIGURATION_FILE_NAME;
    let config_path = vapor_dir.join(config_name);
    let mut redactor = (paths == Paths::Redact).then(|| {
        super::redact::Redactor::new(
            &vapor_shared::config::load_from(&config_path).config,
            vapor_shared::runtime_paths::home_directory().as_deref(),
            vapor_shared::device_id::persisted(&config_path).as_deref(),
        )
    });
    let logs_dir = vapor_dir.join(vapor_shared::constants::runtime::LOGS_DIRECTORY_NAME);
    let mut log_files: Vec<PathBuf> = match fs::read_dir(&logs_dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect(),
        Err(_) => Vec::new(),
    };
    log_files.sort();
    // The first pass: the redactor reads everything the report will
    // hold and learns the names, so a name is replaced in a sentence
    // that holds it with no folder in front of it too.
    if let Some(redactor) = redactor.as_mut() {
        let mut learn_json =
            |contents: &str| match serde_json::from_str::<serde_json::Value>(contents) {
                Ok(document) => redactor.learn_json(&document),
                Err(_) => contents
                    .lines()
                    .for_each(|line| redactor.learn_log_line(line)),
            };
        if let Ok(contents) = fs::read_to_string(&config_path) {
            learn_json(&contents);
        }
        if let Some(live) = &live {
            for contents in [
                &live.status_json,
                &live.diagnostics_json,
                &live.timeline_json,
            ]
            .into_iter()
            .flatten()
            {
                learn_json(contents);
            }
        }
        for log in &log_files {
            if let Ok(bytes) = fs::read(log) {
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .for_each(|line| redactor.learn_log_line(line));
            }
        }
    }
    let redactor = redactor;
    // A JSON document goes through the redactor as a document, so a
    // path field is redacted whole. One that does not parse is treated
    // as lines of text.
    let redact_json = |contents: String| match (
        &redactor,
        serde_json::from_str::<serde_json::Value>(&contents),
    ) {
        (None, _) => contents,
        (Some(redactor), Ok(mut document)) => {
            redactor.json(&mut document);
            serde_json::to_string_pretty(&document).unwrap_or_default() + "\n"
        }
        (Some(redactor), Err(_)) => redact_lines(redactor, &contents),
    };

    // Runtime config (no secrets by design; tokens live in the secret
    // store).
    if config_path.is_file() {
        let contents = fs::read_to_string(&config_path)?;
        fs::write(path.join(config_name), redact_json(contents))?;
        artifacts.push(config_name.to_string());
    }

    // Daemon/CLI logs (written through the redacting logger).
    if logs_dir.is_dir() {
        let report_logs = path.join(vapor_shared::constants::runtime::LOGS_DIRECTORY_NAME);
        fs::create_dir_all(&report_logs)?;
        for log in &log_files {
            let Some(name) = log.file_name() else {
                continue;
            };
            match &redactor {
                Some(redactor) => {
                    // Lossy on purpose: a log holds the bytes of file
                    // names, and a name need not be UTF-8.
                    let contents = String::from_utf8_lossy(&fs::read(log)?).into_owned();
                    fs::write(report_logs.join(name), redact_lines(redactor, &contents))?;
                }
                None => {
                    fs::copy(log, report_logs.join(name))?;
                }
            }
            artifacts.push(format!(
                "{}/{}",
                vapor_shared::constants::runtime::LOGS_DIRECTORY_NAME,
                name.to_string_lossy()
            ));
        }
    }

    // Live daemon captures, each independent. "Reachable" is decided by
    // the status call alone: a later endpoint failing must not erase the
    // captures that did land or misreport the daemon as unreachable.
    // An error text can name the socket, which is under the home
    // directory.
    let capture_errors: Vec<String> = live
        .as_ref()
        .map(|live| live.capture_errors.clone())
        .unwrap_or_default()
        .into_iter()
        .map(|error| match &redactor {
            Some(redactor) => redactor.log_line(&error),
            None => error,
        })
        .collect();
    let mut daemon_reachable = false;
    if let Some(live) = live {
        daemon_reachable = live.status_json.is_some();
        for (name, contents) in [
            ("status.json", live.status_json),
            ("diagnostics.json", live.diagnostics_json),
            ("timeline.json", live.timeline_json),
        ] {
            if let Some(contents) = contents {
                fs::write(path.join(name), redact_json(contents))?;
                artifacts.push(name.to_string());
            }
        }
    }

    // The manifest lists itself, so its artifact array matches both the
    // report and the actual report contents.
    artifacts.push("manifest.json".to_string());
    let paths_redacted = redactor.is_some();
    let manifest = ReportManifest {
        created_at_ms: timestamp_ms,
        vapor_version: vapor_daemon::build_info::VERSION,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        daemon_reachable,
        artifacts: &artifacts,
        capture_errors: &capture_errors,
        paths_redacted,
        redaction: if paths_redacted {
            REDACTED_NOTE
        } else {
            KEPT_NOTE
        },
    };
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|error| io::Error::other(format!("cannot serialize manifest: {error}")))?;
    fs::write(path.join("manifest.json"), manifest_json)?;

    Ok(SupportReport {
        path,
        artifacts,
        daemon_reachable,
        paths_redacted,
    })
}

fn redact_lines(redactor: &super::redact::Redactor, contents: &str) -> String {
    let mut out = String::with_capacity(contents.len());
    for line in contents.split_inclusive('\n') {
        let (text, newline) = match line.strip_suffix('\n') {
            Some(text) => (text, "\n"),
            None => (line, ""),
        };
        out.push_str(&redactor.log_line(text));
        out.push_str(newline);
    }
    out
}

/// Removes the oldest reports under `reports_root` so that at most
/// [`REPORTS_KEPT`] stay. Only directories this command names
/// (`vapor-report-*`) count. Returns how many went.
pub fn prune_old_reports(reports_root: &Path) -> usize {
    let Ok(entries) = fs::read_dir(reports_root) else {
        return 0;
    };
    // A report's name holds the time it was made, and a collision
    // suffix after it. A folder's modification time moves when anything
    // touches the folder, so the name decides the order.
    let mut reports: Vec<((u64, u64), PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let mut parts = name.strip_prefix("vapor-report-")?.splitn(2, '-');
            let made_at: u64 = parts.next()?.parse().ok()?;
            let suffix: u64 = match parts.next() {
                Some(suffix) => suffix.parse().ok()?,
                None => 0,
            };
            Some(((made_at, suffix), entry.path()))
        })
        .collect();
    reports.sort();
    let surplus = reports.len().saturating_sub(REPORTS_KEPT);
    reports
        .into_iter()
        .take(surplus)
        .filter(|(_, report)| fs::remove_dir_all(report).is_ok())
        .count()
}

/// Creates a fresh report directory, disambiguating a name collision
/// (same-millisecond invocations, or a repeated pre-epoch timestamp) with
/// a numeric suffix. `create_dir` fails on an existing directory (unlike
/// `create_dir_all`), so two reports can never silently merge into one
/// and leave the manifest describing contents that are not all there.
fn create_unique_report_dir(output_root: &Path, timestamp_ms: u64) -> io::Result<PathBuf> {
    fs::create_dir_all(output_root)?;
    let base = format!("vapor-report-{timestamp_ms}");
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
        "too many support reports share this timestamp",
    ))
}

/// Render the report for the human (non-`--json`) path.
pub fn render_report(report: &SupportReport) -> String {
    let mut out = format!("support report written to {}\n", report.path.display());
    out.push_str(&format!(
        "daemon: {}\n",
        if report.daemon_reachable {
            "reachable (live status/diagnostics/timeline captured)"
        } else {
            "not reachable (the report holds on-disk files only)"
        }
    ));
    for artifact in &report.artifacts {
        out.push_str(&format!("  + {artifact}\n"));
    }
    out.push_str(if report.paths_redacted {
        "File and folder names are replaced by codes. A name that stands alone in a \
         sentence can remain: read the report before you share it.\n"
    } else {
        "This report holds your folder paths and the name of every file that synced. Do not \
         attach it to a public issue.\n"
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn seed_vapor_dir(root: &Path) {
        fs::create_dir_all(root.join("logs")).expect("logs dir");
        fs::write(root.join("vapor.json"), "{\"language\":\"en\"}").expect("config");
        fs::write(root.join("logs/vapord.log"), "line-1\n").expect("log file");
        fs::write(root.join("logs/vapor.log"), "line-2\n").expect("log file");
    }

    #[test]
    fn report_collects_config_logs_and_manifest_without_a_daemon() {
        let vapor_dir = TempDir::new().expect("vapor dir");
        let output = TempDir::new().expect("output dir");
        seed_vapor_dir(vapor_dir.path());

        let report =
            write_support_report(vapor_dir.path(), output.path(), None, 1_234, Paths::Keep)
                .expect("report collects");

        assert!(!report.daemon_reachable);
        assert!(report.path.ends_with("vapor-report-1234"));
        assert!(report.path.join("vapor.json").is_file());
        assert!(report.path.join("logs/vapord.log").is_file());
        assert!(report.path.join("logs/vapor.log").is_file());
        assert!(report.path.join("manifest.json").is_file());
        assert!(report.artifacts.contains(&"vapor.json".to_string()));
        assert!(report.artifacts.contains(&"manifest.json".to_string()));

        let manifest = fs::read_to_string(report.path.join("manifest.json")).expect("manifest");
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
            status_json: Some("{\"run\":{\"state\":\"running\"}}".to_string()),
            diagnostics_json: Some("{\"items\":[]}".to_string()),
            timeline_json: Some("{\"entries\":[]}".to_string()),
            capture_errors: Vec::new(),
        };
        let report =
            write_support_report(vapor_dir.path(), output.path(), Some(live), 99, Paths::Keep)
                .expect("report collects");

        assert!(report.daemon_reachable);
        for name in ["status.json", "diagnostics.json", "timeline.json"] {
            assert!(report.path.join(name).is_file(), "missing {name}");
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
            status_json: Some("{\"run\":{\"state\":\"running\"}}".to_string()),
            diagnostics_json: Some("{\"items\":[]}".to_string()),
            timeline_json: None,
            capture_errors: vec!["timeline: daemon not responding".to_string()],
        };
        let report =
            write_support_report(vapor_dir.path(), output.path(), Some(live), 42, Paths::Keep)
                .expect("report collects");

        // Reachability is decided by status, not by the failed endpoint.
        assert!(report.daemon_reachable);
        assert!(report.path.join("status.json").is_file());
        assert!(report.path.join("diagnostics.json").is_file());
        assert!(!report.path.join("timeline.json").exists());

        let manifest = fs::read_to_string(report.path.join("manifest.json")).expect("manifest");
        let parsed: serde_json::Value = serde_json::from_str(&manifest).expect("manifest json");
        assert_eq!(parsed["daemonReachable"], serde_json::Value::Bool(true));
        assert_eq!(
            parsed["captureErrors"][0],
            serde_json::json!("timeline: daemon not responding")
        );
    }

    #[test]
    fn same_timestamp_reports_get_distinct_directories() {
        let vapor_dir = TempDir::new().expect("vapor dir");
        let output = TempDir::new().expect("output dir");
        seed_vapor_dir(vapor_dir.path());

        let first = write_support_report(vapor_dir.path(), output.path(), None, 1_000, Paths::Keep)
            .expect("first report");
        let second =
            write_support_report(vapor_dir.path(), output.path(), None, 1_000, Paths::Keep)
                .expect("second report");

        assert_ne!(
            first.path, second.path,
            "a repeated timestamp must not merge two reports into one directory"
        );
        assert!(first.path.join("manifest.json").is_file());
        assert!(second.path.join("manifest.json").is_file());
    }

    #[test]
    fn missing_config_and_logs_are_tolerated() {
        let vapor_dir = TempDir::new().expect("vapor dir");
        let output = TempDir::new().expect("output dir");

        let report = write_support_report(vapor_dir.path(), output.path(), None, 7, Paths::Keep)
            .expect("report collects from an empty runtime dir");

        // Only the manifest is guaranteed.
        assert_eq!(report.artifacts, vec!["manifest.json".to_string()]);
        assert!(report.path.join("manifest.json").is_file());
    }

    #[test]
    fn report_json_shape_is_camel_case_for_the_json_flag() {
        let report = SupportReport {
            path: PathBuf::from("/tmp/vapor-report-1"),
            artifacts: vec!["manifest.json".to_string()],
            daemon_reachable: false,
            paths_redacted: true,
        };
        assert_eq!(
            serde_json::to_string(&report).expect("serializes"),
            r#"{"path":"/tmp/vapor-report-1","artifacts":["manifest.json"],"daemonReachable":false,"pathsRedacted":true}"#
        );
    }

    // The fixtures are Unix paths. The redaction of drive-letter and
    // backslash paths is open work for the Windows surface.
    #[cfg(unix)]
    #[test]
    fn a_report_holds_no_folder_or_file_name_unless_the_user_keeps_them() {
        let vapor_dir = TempDir::new().expect("vapor dir");
        let output = TempDir::new().expect("output dir");
        fs::create_dir_all(vapor_dir.path().join("logs")).expect("logs dir");
        fs::write(
            vapor_dir.path().join("vapor.json"),
            r#"{ "deviceId": "sams-mac-k3x9",
                 "localRoot": "/Users/sam/Family Photos",
                 "cloudRoot": "/Users/sam/cloud/Photos" }"#,
        )
        .expect("config");
        fs::write(
            vapor_dir.path().join("logs/vapord.log"),
            "1 [INFO] (vapord): Removed from the cloud. \
             path=/Users/sam/Family Photos/summer 2025/beach day.jpg remote_path=summer 2025/beach day.jpg\n",
        )
        .expect("log");
        let live = || LiveCaptures {
            status_json: Some(
                r#"{"run":{"state":"running","reason":"watching /Users/sam/Family Photos"}}"#
                    .to_string(),
            ),
            diagnostics_json: Some(
                r#"{"items":[{"path":"/Users/sam/Family Photos/summer 2025/beach day.jpg"}]}"#
                    .to_string(),
            ),
            // A question names the file with no folder in front of it,
            // and an error names the socket under the home directory.
            timeline_json: Some(
                r#"{"entries":[{"message":"Keep only beach day.jpg? (beach day.jpg | other)"}]}"#
                    .to_string(),
            ),
            capture_errors: vec![
                "diagnostics: no answer on /Users/sam/.vapor/vapord.sock".to_string(),
            ],
        };
        let everything = |report: &SupportReport| {
            [
                "vapor.json",
                "logs/vapord.log",
                "status.json",
                "diagnostics.json",
                "timeline.json",
                "manifest.json",
            ]
            .iter()
            .map(|name| fs::read_to_string(report.path.join(name)).expect(name))
            .collect::<Vec<_>>()
            .join("\n")
        };

        let redacted = write_support_report(
            vapor_dir.path(),
            output.path(),
            Some(live()),
            1,
            Paths::Redact,
        )
        .expect("report");
        assert!(redacted.paths_redacted);
        let text = everything(&redacted);
        for name in ["sam", "Family", "Photos", "summer", "beach", "2025"] {
            assert!(
                !text.contains(name),
                "{name} is still in the report:\n{text}"
            );
        }
        assert!(
            text.contains("Removed from the cloud."),
            "the messages stay"
        );
        assert!(
            text.contains("<local-root:default>") && text.contains(".jpg"),
            "{text}"
        );
        let manifest = fs::read_to_string(redacted.path.join("manifest.json")).expect("manifest");
        assert!(manifest.contains("\"pathsRedacted\": true"), "{manifest}");
        assert!(
            manifest.contains("/.vapor/vapord.sock"),
            "the layout of the runtime directory stays: {manifest}"
        );
        assert!(render_report(&redacted).contains("read the report before you share it"));

        let kept = write_support_report(
            vapor_dir.path(),
            output.path(),
            Some(live()),
            2,
            Paths::Keep,
        )
        .expect("report");
        assert!(!kept.paths_redacted);
        assert!(everything(&kept).contains("/Users/sam/Family Photos/summer 2025/beach day.jpg"));
        assert!(render_report(&kept).contains("Do not"));
    }

    #[test]
    fn the_oldest_reports_go_when_more_than_the_kept_number_are_there() {
        let reports = TempDir::new().expect("reports");
        let vapor_dir = TempDir::new().expect("vapor dir");
        seed_vapor_dir(vapor_dir.path());
        let mut written = Vec::new();
        for index in 0..(REPORTS_KEPT as u64 + 2) {
            let report =
                write_support_report(vapor_dir.path(), reports.path(), None, index, Paths::Redact)
                    .expect("report");
            written.push(report.path);
        }
        fs::create_dir_all(reports.path().join("my-notes")).expect("another folder");

        assert_eq!(prune_old_reports(reports.path()), 2);
        assert!(
            !written[0].exists() && !written[1].exists(),
            "the two oldest went"
        );
        assert!(written[2..].iter().all(|report| report.exists()));
        assert!(
            reports.path().join("my-notes").exists(),
            "only reports are counted"
        );
        assert_eq!(prune_old_reports(reports.path()), 0);
    }
}
