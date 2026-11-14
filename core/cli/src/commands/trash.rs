//! `vapor trash list|restore|empty`: the files Vapor removed on this
//! device and kept in the managed trash. Reads and moves files under
//! `vapor_dir/trash/<profile>/`, so it works with or without a running
//! daemon; a restored file lands in the sync root and syncs like any
//! other write. Every trash on disk is in reach: the one of a profile
//! that is disabled, and the one of a profile that left `vapor.json`,
//! hold the user's files as much as the others. Design:
//! `docs/architecture/data-flow.md` §Remote to local.

use std::path::{Path, PathBuf};

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

/// Every profile that can hold a trash: the profiles of `vapor.json`,
/// enabled or not, and every profile directory under `trash_root`,
/// which is what a removed profile leaves.
fn trash_profile_ids(config: &VaporConfig, trash_root: &Path) -> Vec<String> {
    let mut ids: Vec<String> = resolve_profiles(config)
        .into_iter()
        .map(|profile| profile.id)
        .collect();
    let on_disk = std::fs::read_dir(trash_root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| vapor_daemon::profiles::is_valid_profile_id(name));
    for id in on_disk {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// The profile's trash, home location plus the sync root's volume
/// when that is another volume.
fn open(config: &VaporConfig, trash_root: &Path, profile_id: &str) -> LocalTrash {
    let sync_root = resolve_profiles(config)
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .and_then(|profile| profile.scope.local_root);
    LocalTrash::open(profile_id, trash_root.join(profile_id)).with_sync_root(sync_root)
}

/// Every entry of every profile, newest first.
pub fn list_trash(config: &VaporConfig, trash_root: &Path) -> TrashListReport {
    let mut entries: Vec<TrashEntryJson> = trash_profile_ids(config, trash_root)
        .iter()
        .flat_map(|profile_id| open(config, trash_root, profile_id).list())
        .map(to_json)
        .collect();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.discarded_at_ms));
    TrashListReport { entries }
}

/// Puts one entry back. `profile` disambiguates when several profiles
/// hold the same id.
pub fn restore(
    config: &VaporConfig,
    trash_root: &Path,
    id: &str,
    profile: Option<&str>,
) -> Result<RestoreReport, String> {
    let profile_id = pick_profile(config, trash_root, id, profile)?;
    let restored_to = open(config, trash_root, &profile_id)
        .restore(id)
        .map_err(|error| format!("cannot restore trash entry {id}: {error}"))?;
    Ok(RestoreReport {
        profile_id,
        id: id.to_string(),
        restored_to,
    })
}

/// What `empty` would remove: how many entries, and their size.
pub fn empty_preview(
    config: &VaporConfig,
    trash_root: &Path,
    profile: Option<&str>,
) -> (usize, u64) {
    let entries: Vec<TrashEntry> = profiles_to_empty(config, trash_root, profile)
        .iter()
        .flat_map(|profile_id| open(config, trash_root, profile_id).list())
        .collect();
    (
        entries.len(),
        entries.iter().map(|entry| entry.size_bytes).sum(),
    )
}

/// The question `empty` asks before it deletes for good.
pub fn empty_question(count: usize, size_bytes: u64) -> String {
    format!(
        "Delete {count} item(s) ({}) from the trash for good? They cannot be restored after this.",
        human_size(size_bytes)
    )
}

fn profiles_to_empty(
    config: &VaporConfig,
    trash_root: &Path,
    profile: Option<&str>,
) -> Vec<String> {
    match profile {
        Some(profile) => vec![profile.to_string()],
        None => trash_profile_ids(config, trash_root),
    }
}

/// Removes every entry of every profile (or of one), for good. The
/// caller has the user's confirmation.
pub fn empty(config: &VaporConfig, trash_root: &Path, profile: Option<&str>) -> EmptyReport {
    EmptyReport {
        removed: profiles_to_empty(config, trash_root, profile)
            .iter()
            .map(|profile_id| open(config, trash_root, profile_id).empty())
            .sum(),
    }
}

fn pick_profile(
    config: &VaporConfig,
    trash_root: &Path,
    id: &str,
    profile: Option<&str>,
) -> Result<String, String> {
    if let Some(profile) = profile {
        return Ok(profile.to_string());
    }
    let holders: Vec<String> = trash_profile_ids(config, trash_root)
        .into_iter()
        .filter(|profile_id| open(config, trash_root, profile_id).entry(id).is_some())
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
    fn the_trash_of_a_disabled_and_of_a_removed_profile_is_in_reach() {
        use vapor_shared::config::ProfileConfig;
        let temp = tempfile::TempDir::new().expect("temp");
        let trash_root = temp.path().join("trash");
        let root = temp.path().join("root");
        std::fs::create_dir_all(&root).expect("root");
        let profile = |id: &str, enabled: bool| ProfileConfig {
            id: id.to_string(),
            enabled: Some(enabled),
            local_root: Some(root.join(id).display().to_string()),
            cloud_root: Some(format!("/{id}")),
            ..ProfileConfig::default()
        };
        let config = VaporConfig {
            profiles: vec![profile("work", true), profile("paused", false)],
            ..VaporConfig::default()
        };
        // One discarded file for a running profile, for a disabled one,
        // and for one that is no longer in vapor.json.
        let mut ids = Vec::new();
        for (index, id) in ["work", "paused", "gone"].iter().enumerate() {
            let file = root.join(format!("{id}.txt"));
            std::fs::write(&file, b"discarded").expect("seed");
            let trash = LocalTrash::open(id, trash_root.join(id));
            let disposition = trash
                .discard(
                    &file,
                    "cloud-delete",
                    std::time::SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_secs(1_700_000_000 + index as u64),
                )
                .expect("discard");
            let vapor_daemon::trash::Disposition::Managed(entry) = disposition else {
                panic!("managed trash expected");
            };
            ids.push(entry);
        }

        let listed = list_trash(&config, &trash_root);
        let mut profiles: Vec<&str> = listed
            .entries
            .iter()
            .map(|entry| entry.profile_id.as_str())
            .collect();
        profiles.sort();
        assert_eq!(profiles, vec!["gone", "paused", "work"]);

        // Restore finds the entry of the removed profile with no flag.
        let restored = restore(&config, &trash_root, &ids[2], None).expect("restore");
        assert_eq!(restored.profile_id, "gone");
        assert!(root.join("gone.txt").is_file());

        let (count, size) = empty_preview(&config, &trash_root, None);
        assert_eq!((count, size), (2, 18));
        assert!(empty_question(count, size).contains("2 item(s)"));
        assert_eq!(empty(&config, &trash_root, None).removed, 2);
        assert!(list_trash(&config, &trash_root).entries.is_empty());
    }

    #[test]
    fn restore_and_empty_json_shapes_are_locked() {
        let restored = RestoreReport {
            profile_id: "default".into(),
            id: "1700000000000-0001".into(),
            restored_to: PathBuf::from("/r/notes.txt"),
        };
        assert_eq!(
            serde_json::to_string(&restored).expect("serialize"),
            r#"{"profileId":"default","id":"1700000000000-0001","restoredTo":"/r/notes.txt"}"#
        );
        assert_eq!(
            serde_json::to_string(&EmptyReport { removed: 3 }).expect("serialize"),
            r#"{"removed":3}"#
        );
    }

    #[test]
    fn json_shape_is_locked() {
        let report = TrashListReport {
            entries: vec![TrashEntryJson {
                profile_id: "default".into(),
                id: "1700000000000-0001".into(),
                original_path: PathBuf::from("/r/notes.txt"),
                kind: "file".into(),
                reason: "cloud-delete".into(),
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
      "reason": "cloud-delete",
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
                reason: "mirror-delete".into(),
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
