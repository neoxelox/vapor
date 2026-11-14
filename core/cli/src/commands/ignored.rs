//! `vapor ignored`: what the ignore rules keep out of sync on this
//! device, each with the rule that does it.
//!
//! The default rules name things that are safe to skip for most people
//! (`node_modules/`, `build/`, `*.log`). A folder of the user's own can
//! have one of those names, and then nothing under it syncs. This list
//! is how the user finds out. The walk reads the local roots only, so
//! it works with or without a running daemon.

use std::path::{Path, PathBuf};

use serde::Serialize;
use vapor_daemon::path_filter::{EventPathFilter, EventPathFilterOptions};
use vapor_daemon::profiles::resolve_profiles;
use vapor_shared::config::VaporConfig;

/// The most entries one answer lists.
pub const LIST_LIMIT: usize = 5_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredEntry {
    pub profile_id: String,
    pub path: PathBuf,
    /// `file` or `folder`. A folder stands for everything under it.
    pub kind: &'static str,
    /// The rule as it is written: `build/`, `*.log`.
    pub rule: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredReport {
    pub entries: Vec<IgnoredEntry>,
    /// True when the list stopped at its limit.
    pub truncated: bool,
}

/// The ignored entries of `profile`, or of every profile, up to
/// `limit`.
pub fn list_ignored(
    config: &VaporConfig,
    profile: Option<&str>,
    limit: usize,
) -> Result<IgnoredReport, String> {
    let profiles = resolve_profiles(config);
    if let Some(wanted) = profile
        && !profiles.iter().any(|candidate| candidate.id == wanted)
    {
        return Err(format!("no profile '{wanted}'"));
    }
    let options = EventPathFilterOptions::from_config(config);
    let mut report = IgnoredReport {
        entries: Vec::new(),
        truncated: false,
    };
    for candidate in profiles
        .iter()
        .filter(|candidate| profile.is_none_or(|wanted| candidate.id == wanted))
    {
        let Some(root) = candidate.scope.local_root.as_deref() else {
            continue;
        };
        if !root.is_dir() {
            continue;
        }
        let filter = EventPathFilter::for_watch_root(root, &options);
        collect(&filter, &candidate.id, root, limit, &mut report);
    }
    report.entries.sort_by(|a, b| {
        a.profile_id
            .cmp(&b.profile_id)
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(report)
}

fn collect(
    filter: &EventPathFilter,
    profile_id: &str,
    root: &Path,
    limit: usize,
    report: &mut IgnoredReport,
) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // A link counts as what it is, never as its target.
            let is_folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
            if let Some(rule) = filter.ignored_by_rule(&path, is_folder) {
                if report.entries.len() >= limit {
                    report.truncated = true;
                    return;
                }
                report.entries.push(IgnoredEntry {
                    profile_id: profile_id.to_string(),
                    path,
                    kind: if is_folder { "folder" } else { "file" },
                    rule: rule.to_string(),
                });
            } else if is_folder && !filter.should_ignore_kind(&path, true) {
                pending.push(path);
            }
        }
    }
}

pub fn render(report: &IgnoredReport) -> String {
    if report.entries.is_empty() {
        return "No file or folder on this device is ignored.".to_string();
    }
    let mut lines: Vec<String> = report
        .entries
        .iter()
        .map(|entry| {
            format!(
                "{}{} [{}] rule: {}",
                entry.path.display(),
                if entry.kind == "folder" { "/" } else { "" },
                entry.profile_id,
                entry.rule
            )
        })
        .collect();
    lines.push(format!(
        "{}{} path(s) are not synced. A folder stands for everything under it. Change the rules \
         in a .vaporignore file or with preIgnoreRules and postIgnoreRules.",
        report.entries.len(),
        if report.truncated { " or more" } else { "" }
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn config(root: &Path) -> VaporConfig {
        VaporConfig {
            local_root: Some(root.display().to_string()),
            cloud_root: Some("/tmp/vapor-test-cloud".to_string()),
            ..VaporConfig::default()
        }
    }

    #[test]
    fn the_list_names_each_hidden_entry_and_its_rule_and_never_looks_inside_one() {
        let temp = TempDir::new().expect("temp");
        let root = vapor_shared::paths::canonicalize(temp.path()).expect("canonical");
        for file in [
            "notes/plan.txt",
            "notes/build",
            "build/huge/a.o",
            "build/huge/b.o",
            "photos/trip.log",
            ".vapor-tmp-upload",
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            std::fs::write(path, b"x").expect("seed");
        }

        let report = list_ignored(&config(&root), None, LIST_LIMIT).expect("list");
        let listed: Vec<(String, &str, &str)> = report
            .entries
            .iter()
            .map(|entry| {
                (
                    entry
                        .path
                        .strip_prefix(&root)
                        .expect("under the root")
                        .display()
                        .to_string(),
                    entry.kind,
                    entry.rule.as_str(),
                )
            })
            .collect();
        assert_eq!(
            listed,
            vec![
                // The folder, one time: not the files under it.
                ("build".to_string(), "folder", "build/"),
                ("photos/trip.log".to_string(), "file", "*.log"),
            ],
            "a file named build syncs, and Vapor's own files are not listed"
        );
        assert!(!report.truncated);
        assert!(render(&report).contains("2 path(s) are not synced"));

        let short = list_ignored(&config(&root), None, 1).expect("list");
        assert_eq!(short.entries.len(), 1);
        assert!(short.truncated);
        assert!(list_ignored(&config(&root), Some("other"), 10).is_err());
    }

    #[test]
    fn the_json_shape_is_stable() {
        let report = IgnoredReport {
            entries: vec![IgnoredEntry {
                profile_id: "default".to_string(),
                path: PathBuf::from("/r/build"),
                kind: "folder",
                rule: "build/".to_string(),
            }],
            truncated: false,
        };
        assert_eq!(
            serde_json::to_string(&report).expect("serialize"),
            r#"{"entries":[{"profileId":"default","path":"/r/build","kind":"folder","rule":"build/"}],"truncated":false}"#
        );
    }
}
