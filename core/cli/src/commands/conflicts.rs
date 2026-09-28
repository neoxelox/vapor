//! `vapor conflicts list|resolve` — keep-both conflict surfacing.
//!
//! The conflict-copy files themselves are the durable registry: they
//! sync to every replica, survive daemon restarts and reinstalls, and
//! disappear exactly when a conflict is resolved — so listing is a
//! filesystem scan of each profile's local root, not a query against
//! the bounded in-memory timeline (whose 1000-event cap makes it a
//! notification stream, never a ledger). Resolution is plain file
//! operations on the local root; the daemon syncs them like any other
//! user edit, so resolving works even while the daemon is down.
//!
//! App surfaces (macOS today, Windows/Linux later) drive this command
//! with `--json` instead of reimplementing the scan — the same shim
//! pattern they use for lifecycle. Full design:
//! `docs/architecture/conflict-resolution.md`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use vapor_daemon::conflict::parse_conflict_copy_name;
use vapor_daemon::path_filter::{EventPathFilter, EventPathFilterOptions};
use vapor_daemon::profiles::resolve_profiles;
use vapor_shared::config::VaporConfig;
use vapor_shared::constants;

/// One unresolved keep-both conflict: the original file plus the
/// divergent copy preserved next to it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictRecord {
    pub profile_id: String,
    pub original_path: PathBuf,
    pub copy_path: PathBuf,
    /// Device whose divergent version the copy preserves.
    pub device_id: String,
    /// When the divergence was observed (epoch milliseconds).
    pub diverged_at_ms: u64,
    pub copy_size_bytes: u64,
    /// The original can be gone (deleted after the conflict);
    /// the copy still lists so the preserved version is not orphaned
    /// silently.
    pub original_exists: bool,
    pub original_size_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictListReport {
    pub conflicts: Vec<ConflictRecord>,
    /// Profile local roots that could not be scanned (missing or
    /// unreadable) — listed so an empty result is never silently
    /// incomplete.
    pub skipped_roots: Vec<PathBuf>,
    /// Subdirectories encountered during the walk that could not be read
    /// (e.g. `chmod 000`). Recorded for the same reason as
    /// `skipped_roots`: a conflict copy under such a directory would
    /// otherwise be invisibly orphaned.
    pub skipped_directories: Vec<PathBuf>,
}

/// Scans every enabled profile's local root for conflict copies.
/// Ignored directories are pruned with the same rules the sync
/// pipeline uses, so the scan never descends into `node_modules/`-class
/// subtrees (conflict copies cannot exist there — ignored names never
/// sync).
pub fn list_conflicts(config: &VaporConfig) -> ConflictListReport {
    let filter_options = EventPathFilterOptions::from_config(config);
    let mut conflicts = Vec::new();
    let mut skipped_roots = Vec::new();
    let mut skipped_directories = Vec::new();

    for profile in resolve_profiles(config)
        .into_iter()
        .filter(|profile| profile.enabled)
    {
        let Some(root) = profile.scope.local_root else {
            continue;
        };
        if !root.exists() {
            // A root the daemon has not created yet (or is waiting for)
            // holds no conflicts; only an unreadable one is skipped.
            continue;
        }
        let Ok(root) = vapor_shared::paths::canonicalize(&root) else {
            skipped_roots.push(root);
            continue;
        };
        let filter = EventPathFilter::for_watch_root(&root, &filter_options);
        scan_root(
            &root,
            &filter,
            &profile.id,
            &mut conflicts,
            &mut skipped_directories,
        );
    }

    conflicts.sort_by(|a, b| a.copy_path.cmp(&b.copy_path));
    skipped_directories.sort();
    ConflictListReport {
        conflicts,
        skipped_roots,
        skipped_directories,
    }
}

fn scan_root(
    root: &Path,
    filter: &EventPathFilter,
    profile_id: &str,
    conflicts: &mut Vec<ConflictRecord>,
    skipped_directories: &mut Vec<PathBuf>,
) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            // An existing-but-unreadable directory (it canonicalized, then
            // read_dir failed) must be surfaced, not dropped: a conflict
            // copy beneath it would otherwise be invisibly orphaned.
            Err(_) => {
                skipped_directories.push(directory);
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if filter.should_ignore(&path) {
                continue;
            }
            let Ok(metadata) = path.symlink_metadata() else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(parsed) = parse_conflict_copy_name(name) else {
                continue;
            };
            let original_path = path.with_file_name(&parsed.canonical_file_name);
            let original_size_bytes = fs::metadata(&original_path)
                .ok()
                .filter(|meta| meta.is_file())
                .map(|meta| meta.len());
            conflicts.push(ConflictRecord {
                profile_id: profile_id.to_string(),
                original_path,
                copy_path: path,
                device_id: parsed.device_id,
                diverged_at_ms: parsed.timestamp_ms,
                copy_size_bytes: metadata.len(),
                original_exists: original_size_bytes.is_some(),
                original_size_bytes,
            });
        }
    }
}

/// Which version survives under the original name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeepSide {
    /// Keep the original; delete the conflict copy.
    Original,
    /// The copy moves over the original (atomic rename where the OS
    /// allows); the previous original is discarded.
    Copy,
}

impl KeepSide {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            constants::conflict::CHOICE_ORIGINAL => Ok(Self::Original),
            constants::conflict::CHOICE_COPY => Ok(Self::Copy),
            other => Err(format!(
                "unknown --choose value '{other}'; expected 'original' or 'copy'"
            )),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionReport {
    pub original_path: PathBuf,
    pub copy_path: PathBuf,
    pub kept: &'static str,
}

/// Resolves one conflict with plain file operations. Refuses paths
/// whose name does not match the machine-generated conflict grammar so
/// the command can never be talked into deleting an arbitrary file.
pub fn resolve_conflict(copy_path: &Path, keep: KeepSide) -> Result<ResolutionReport, String> {
    let name = copy_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no file name", copy_path.display()))?;
    let parsed = parse_conflict_copy_name(name).ok_or_else(|| {
        format!("{name} is not a Vapor conflict copy (expected the ~conflict-… name pattern)")
    })?;
    if !copy_path.is_file() {
        return Err(format!(
            "{} does not exist or is not a regular file",
            copy_path.display()
        ));
    }
    let original_path = copy_path.with_file_name(&parsed.canonical_file_name);
    if keep == KeepSide::Copy
        && let Some(existing) = vapor_daemon::name_collision::colliding_local_path(&original_path)
    {
        // The copy stands in for a cloud name this filesystem cannot
        // hold next to `existing`; renaming it over the original name
        // would only swap which cloud object is stranded.
        return Err(format!(
            "{} is a name clash with {}: the two cloud files differ only in spelling this filesystem \
             folds together. Choose the original to drop the cloud's other copy, or rename the \
             files apart in the cloud",
            copy_path.display(),
            existing.display()
        ));
    }

    match keep {
        KeepSide::Original => {
            fs::remove_file(copy_path)
                .map_err(|error| format!("cannot remove {}: {error}", copy_path.display()))?;
        }
        KeepSide::Copy => {
            // Same-directory rename: atomic replace on Unix. Windows
            // refuses to rename onto an existing file, so fall back to
            // remove-then-rename there — the version being kept (the
            // copy) is never the one at risk in that window.
            if let Err(error) = fs::rename(copy_path, &original_path) {
                // Only the Windows destination-exists collision warrants
                // deleting the original first. Any other error
                // (read-only remount, immutable flag, EIO) must NOT remove
                // the original version inside a failing path — the retry
                // would still fail and we'd have destroyed it for nothing.
                if error.kind() == io::ErrorKind::AlreadyExists {
                    fs::remove_file(&original_path).map_err(|error| {
                        format!("cannot replace {}: {error}", original_path.display())
                    })?;
                    fs::rename(copy_path, &original_path).map_err(|error| {
                        format!(
                            "cannot rename {} over {}: {error}",
                            copy_path.display(),
                            original_path.display()
                        )
                    })?;
                } else {
                    return Err(format!(
                        "cannot rename {} over {}: {error}",
                        copy_path.display(),
                        original_path.display()
                    ));
                }
            }
        }
    }

    Ok(ResolutionReport {
        original_path,
        copy_path: copy_path.to_path_buf(),
        kept: match keep {
            KeepSide::Original => constants::conflict::CHOICE_ORIGINAL,
            KeepSide::Copy => constants::conflict::CHOICE_COPY,
        },
    })
}

pub fn render_list(report: &ConflictListReport) -> String {
    if report.conflicts.is_empty()
        && report.skipped_roots.is_empty()
        && report.skipped_directories.is_empty()
    {
        return "No unresolved conflicts.".to_string();
    }
    let mut out = String::new();
    if report.conflicts.is_empty() {
        out.push_str("No unresolved conflicts.\n");
    } else {
        out.push_str(&format!(
            "{} unresolved conflict(s):\n",
            report.conflicts.len()
        ));
        for record in &report.conflicts {
            out.push_str(&format!(
                "  [{}] {}\n      copy: {} ({} bytes, from device {})\n",
                record.profile_id,
                record.original_path.display(),
                record.copy_path.display(),
                record.copy_size_bytes,
                record.device_id,
            ));
        }
        out.push_str(
            "\nResolve with: vapor conflicts resolve <copy-path> --choose <original|copy>\n",
        );
    }
    for root in &report.skipped_roots {
        out.push_str(&format!("  (skipped unreadable root {})\n", root.display()));
    }
    for directory in &report.skipped_directories {
        out.push_str(&format!(
            "  (skipped unreadable directory {}; a conflict copy beneath it would not be listed)\n",
            directory.display()
        ));
    }
    out.trim_end().to_string()
}

pub fn render_resolution(report: &ResolutionReport) -> String {
    format!(
        "Resolved: kept the {} version at {} (removed {}).",
        report.kept,
        report.original_path.display(),
        report.copy_path.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Config pointing the implicit default profile at `root`.
    fn config_for(root: &Path) -> VaporConfig {
        VaporConfig {
            local_root: root.to_string_lossy().into_owned(),
            ..VaporConfig::default()
        }
    }

    fn seed_conflict(root: &Path, original: &str, copy_name: &str) -> PathBuf {
        let original_path = root.join(original);
        fs::create_dir_all(original_path.parent().unwrap()).expect("dirs");
        fs::write(&original_path, b"original bytes").expect("seed original");
        let copy = original_path.with_file_name(copy_name);
        fs::write(&copy, b"divergent copy bytes!").expect("seed copy");
        copy
    }

    #[test]
    fn list_finds_conflict_copies_and_pairs_originals() {
        let temp = TempDir::new().expect("temp");
        let root = temp.path().join("local");
        fs::create_dir_all(&root).expect("root");
        seed_conflict(
            &root,
            "docs/report.md",
            "report~conflict-other-mac-1750000000123.md",
        );
        // Ignored subtree: even a conflict-shaped name inside is
        // invisible (pruned before it is read).
        let ignored = root.join("node_modules");
        fs::create_dir_all(&ignored).expect("dirs");
        fs::write(
            ignored.join("x~conflict-dev-1750000000123.js"),
            b"never listed",
        )
        .expect("seed ignored");

        let report = list_conflicts(&config_for(&root));
        assert_eq!(report.conflicts.len(), 1);
        let record = &report.conflicts[0];
        assert_eq!(record.profile_id, "default");
        assert!(record.original_path.ends_with("docs/report.md"));
        assert_eq!(record.device_id, "other-mac");
        assert_eq!(record.diverged_at_ms, 1_750_000_000_123);
        assert!(record.original_exists);
        assert_eq!(record.original_size_bytes, Some(14));
        assert_eq!(record.copy_size_bytes, 21);
        assert!(report.skipped_roots.is_empty());
    }

    #[test]
    fn list_on_a_fresh_config_reports_clean_without_creating_the_root() {
        // Creating the local root is the daemon's call (it knows
        // whether the profile synced there before); a listing never
        // creates anything, and an absent root holds no conflicts.
        let temp = TempDir::new().expect("temp");
        let root = temp.path().join("never-created");
        let report = list_conflicts(&config_for(&root));
        assert!(report.conflicts.is_empty());
        assert!(report.skipped_roots.is_empty());
        assert!(!root.exists(), "a listing must not create the root");
    }

    #[test]
    fn resolve_keep_original_deletes_only_the_copy() {
        let temp = TempDir::new().expect("temp");
        let root = temp.path().to_path_buf();
        let copy = seed_conflict(&root, "a.txt", "a~conflict-dev-1750000000123.txt");

        let report = resolve_conflict(&copy, KeepSide::Original).expect("resolve");
        assert_eq!(report.kept, "original");
        assert!(!copy.exists());
        assert_eq!(
            fs::read(root.join("a.txt")).expect("read"),
            b"original bytes"
        );
    }

    #[test]
    fn resolve_keep_copy_promotes_the_copy_content() {
        let temp = TempDir::new().expect("temp");
        let root = temp.path().to_path_buf();
        let copy = seed_conflict(&root, "a.txt", "a~conflict-dev-1750000000123.txt");

        let report = resolve_conflict(&copy, KeepSide::Copy).expect("resolve");
        assert_eq!(report.kept, "copy");
        assert!(!copy.exists());
        assert_eq!(
            fs::read(root.join("a.txt")).expect("read"),
            b"divergent copy bytes!"
        );
    }

    #[test]
    fn resolve_refuses_paths_that_are_not_conflict_copies() {
        let temp = TempDir::new().expect("temp");
        let victim = temp.path().join("precious.txt");
        fs::write(&victim, b"do not delete").expect("seed");

        let error = resolve_conflict(&victim, KeepSide::Original).expect_err("must refuse");
        assert!(error.contains("not a Vapor conflict copy"));
        assert!(victim.exists());
    }

    #[test]
    fn json_shape_is_locked() {
        // §9.2 snapshot discipline: the --json contract apps consume.
        let report = ConflictListReport {
            conflicts: vec![ConflictRecord {
                profile_id: "default".to_string(),
                original_path: PathBuf::from("/r/a.txt"),
                copy_path: PathBuf::from("/r/a~conflict-dev-1750000000123.txt"),
                device_id: "dev".to_string(),
                diverged_at_ms: 1_750_000_000_123,
                copy_size_bytes: 21,
                original_exists: true,
                original_size_bytes: Some(15),
            }],
            skipped_roots: vec![],
            skipped_directories: vec![PathBuf::from("/r/locked")],
        };
        let rendered = serde_json::to_string_pretty(&report).expect("serialize");
        let expected = r#"{
  "conflicts": [
    {
      "profileId": "default",
      "originalPath": "/r/a.txt",
      "copyPath": "/r/a~conflict-dev-1750000000123.txt",
      "deviceId": "dev",
      "divergedAtMs": 1750000000123,
      "copySizeBytes": 21,
      "originalExists": true,
      "originalSizeBytes": 15
    }
  ],
  "skippedRoots": [],
  "skippedDirectories": [
    "/r/locked"
  ]
}"#;
        assert_eq!(rendered, expected);
    }
}
