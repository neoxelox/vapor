//! `vapor trash list|restore|empty`: the files Vapor removed on this
//! device and kept in the managed trash. Reads and moves files under
//! `vapor_dir/trash/<profile>/`, so it works with or without a running
//! daemon; a restored file lands in the sync root and syncs like any
//! other write. Design: `docs/architecture/data-flow.md` §Remote to
//! local.

use std::path::PathBuf;

use serde::Serialize;
use vapor_daemon::profiles::resolve_profiles;
use vapor_daemon::trash::{LocalTrash, TrashEntry};
use vapor_shared::config::VaporConfig;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntryJson {
    pub profile_id: String,
    pub id: String,
    pub original_path: PathBuf,
    pub kind: String,
    pub reason: String,
    pub discarded_at_ms: u64,
    pub size_bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashListReport {
    pub entries: Vec<TrashEntryJson>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    pub profile_id: String,
    pub id: String,
    pub restored_to: PathBuf,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptyReport {
    pub removed: usize,
}

fn to_json(entry: TrashEntry) -> TrashEntryJson {
    TrashEntryJson {
        profile_id: entry.profile_id,
        id: entry.id,
        original_path: entry.original_path,
        kind: entry.kind,
        reason: entry.reason,
        discarded_at_ms: entry.discarded_at_ms,
        size_bytes: entry.size_bytes,
    }
}

fn enabled_profile_ids(config: &VaporConfig) -> Vec<String> {
    resolve_profiles(config)
        .into_iter()
        .filter(|profile| profile.enabled)
        .map(|profile| profile.id)
        .collect()
}

fn open(profile_id: &str) -> LocalTrash {
    LocalTrash::open(
        profile_id,
        vapor_shared::runtime_paths::profile_trash_directory(profile_id),
    )
}

/// Every entry of every enabled profile, newest first.
pub fn list_trash(config: &VaporConfig) -> TrashListReport {
    let mut entries: Vec<TrashEntryJson> = enabled_profile_ids(config)
        .iter()
        .flat_map(|profile_id| open(profile_id).list())
        .map(to_json)
        .collect();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.discarded_at_ms));
    TrashListReport { entries }
}

/// Puts one entry back. `profile` disambiguates when several profiles
/// hold the same id.
pub fn restore(
    config: &VaporConfig,
    id: &str,
    profile: Option<&str>,
) -> Result<RestoreReport, String> {
    let profile_id = pick_profile(config, id, profile)?;
    let restored_to = open(&profile_id)
        .restore(id)
        .map_err(|error| format!("cannot restore trash entry {id}: {error}"))?;
    Ok(RestoreReport {
        profile_id,
        id: id.to_string(),
        restored_to,
    })
}

/// Removes every entry of every enabled profile (or of one).
pub fn empty(config: &VaporConfig, profile: Option<&str>) -> EmptyReport {
    let profiles = match profile {
        Some(profile) => vec![profile.to_string()],
        None => enabled_profile_ids(config),
    };
    EmptyReport {
        removed: profiles
            .iter()
            .map(|profile_id| open(profile_id).empty())
            .sum(),
    }
}

fn pick_profile(config: &VaporConfig, id: &str, profile: Option<&str>) -> Result<String, String> {
    if let Some(profile) = profile {
        return Ok(profile.to_string());
    }
    let holders: Vec<String> = enabled_profile_ids(config)
        .into_iter()
        .filter(|profile_id| open(profile_id).entry(id).is_some())
        .collect();
    match holders.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!("no profile holds trash entry {id}")),
        many => Err(format!(
            "trash entry {id} exists in several profiles ({}); pass --profile",
            many.join(", ")
        )),
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn render_list(report: &TrashListReport) -> String {
    if report.entries.is_empty() {
        return "The trash is empty.".to_string();
    }
    let mut lines = Vec::new();
    for entry in &report.entries {
        lines.push(format!(
            "{} [{}] {} {} ({}, {})",
            entry.id,
            entry.profile_id,
            entry.kind,
            entry.original_path.display(),
            entry.reason,
            human_size(entry.size_bytes)
        ));
    }
    lines.push(format!(
        "{} item(s). Restore one with: vapor trash restore <id>",
        report.entries.len()
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape_is_locked() {
        let report = TrashListReport {
            entries: vec![TrashEntryJson {
                profile_id: "default".into(),
                id: "1700000000000-0001".into(),
                original_path: PathBuf::from("/r/notes.txt"),
                kind: "file".into(),
                reason: "deleted-in-cloud".into(),
                discarded_at_ms: 1_700_000_000_000,
                size_bytes: 12,
            }],
        };
        let rendered = serde_json::to_string_pretty(&report).expect("serialize");
        assert_eq!(
            rendered,
            r#"{
  "entries": [
    {
      "profileId": "default",
      "id": "1700000000000-0001",
      "originalPath": "/r/notes.txt",
      "kind": "file",
      "reason": "deleted-in-cloud",
      "discardedAtMs": 1700000000000,
      "sizeBytes": 12
    }
  ]
}"#
        );
    }

    #[test]
    fn text_rendering_names_the_restore_command_and_sizes() {
        let report = TrashListReport {
            entries: vec![TrashEntryJson {
                profile_id: "default".into(),
                id: "1-0001".into(),
                original_path: PathBuf::from("/r/big.bin"),
                kind: "file".into(),
                reason: "mirror-removal".into(),
                discarded_at_ms: 1,
                size_bytes: 3 * 1024 * 1024,
            }],
        };
        let rendered = render_list(&report);
        assert!(rendered.contains("3.0 MiB"));
        assert!(rendered.contains("vapor trash restore <id>"));
        assert_eq!(
            render_list(&TrashListReport { entries: vec![] }),
            "The trash is empty."
        );
    }
}
