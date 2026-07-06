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
use std::path::{Path, PathBuf};

use serde::Serialize;
use vapor_daemon::conflict::parse_conflict_copy_name;
use vapor_daemon::path_filter::{EventPathFilter, EventPathFilterOptions};
use vapor_daemon::profiles::resolve_profiles;
use vapor_shared::config::VaporConfig;

/// One unresolved keep-both conflict: a canonical path plus the
/// divergent copy preserved next to it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictRecord {
    pub profile_id: String,
    pub canonical_path: PathBuf,
    pub conflict_path: PathBuf,
    /// Device whose divergent version the copy preserves.
    pub device_id: String,
    /// When the divergence was observed (epoch milliseconds).
    pub diverged_at_ms: u64,
    pub copy_size_bytes: u64,
    /// The canonical file can be gone (deleted after the conflict);
    /// the copy still lists so the preserved version is not orphaned
    /// silently.
    pub canonical_exists: bool,
    pub canonical_size_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictListReport {
    pub conflicts: Vec<ConflictRecord>,
    /// Profile local roots that could not be scanned (missing or
    /// unreadable) — listed so an empty result is never silently
    /// incomplete.
    pub skipped_roots: Vec<PathBuf>,
}

/// Scans every enabled profile's local root for conflict copies.
/// Ignored directories are pruned with the same rules the sync
/// pipeline uses, so the scan never descends into `node_modules/`-class
/// subtrees (conflict copies cannot exist there — ignored names never
/// sync).
pub fn list_conflicts(config: &VaporConfig) -> ConflictListReport {
    let filter_options = EventPathFilterOptions::from_environment_and_config(config);
    let mut conflicts = Vec::new();
    let mut skipped_roots = Vec::new();

    for profile in resolve_profiles(config)
        .into_iter()
        .filter(|profile| profile.enabled)
    {
        let Some(root) = profile.scope.local_sync_directory else {
            continue;
        };
        let Ok(root) = root.canonicalize() else {
            skipped_roots.push(root);
            continue;
        };
        let filter = EventPathFilter::for_watch_root(&root, &filter_options);
        scan_root(&root, &filter, &profile.id, &mut conflicts);
    }

    conflicts.sort_by(|a, b| a.conflict_path.cmp(&b.conflict_path));
    ConflictListReport {
        conflicts,
        skipped_roots,
    }
}

fn scan_root(
    root: &Path,
    filter: &EventPathFilter,
    profile_id: &str,
    conflicts: &mut Vec<ConflictRecord>,
) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
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
            let canonical_path = path.with_file_name(&parsed.canonical_file_name);
            let canonical_size_bytes = fs::metadata(&canonical_path)
                .ok()
                .filter(|meta| meta.is_file())
                .map(|meta| meta.len());
            conflicts.push(ConflictRecord {
                profile_id: profile_id.to_string(),
                canonical_path,
                conflict_path: path,
                device_id: parsed.device_id,
                diverged_at_ms: parsed.timestamp_ms,
                copy_size_bytes: metadata.len(),
                canonical_exists: canonical_size_bytes.is_some(),
                canonical_size_bytes,
            });
        }
    }
}

/// Which version survives under the canonical name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeepSide {
    /// Keep the canonical file; delete the conflict copy.
    Canonical,
    /// The copy becomes the canonical content (atomic rename where the
    /// OS allows); the previous canonical version is discarded.
    Copy,
}

impl KeepSide {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "canonical" => Ok(Self::Canonical),
            "copy" => Ok(Self::Copy),
            other => Err(format!(
                "unknown --keep value '{other}'; expected 'canonical' or 'copy'"
            )),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionReport {
    pub canonical_path: PathBuf,
    pub removed_conflict_path: PathBuf,
    pub kept: &'static str,
}

/// Resolves one conflict with plain file operations. Refuses paths
/// whose name does not match the machine-generated conflict grammar so
/// the command can never be talked into deleting an arbitrary file.
pub fn resolve_conflict(conflict_path: &Path, keep: KeepSide) -> Result<ResolutionReport, String> {
    let name = conflict_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no file name", conflict_path.display()))?;
    let parsed = parse_conflict_copy_name(name).ok_or_else(|| {
        format!("{name} is not a Vapor conflict copy (expected the ~conflict-… name pattern)")
    })?;
    if !conflict_path.is_file() {
        return Err(format!(
            "{} does not exist or is not a regular file",
            conflict_path.display()
        ));
    }
    let canonical_path = conflict_path.with_file_name(&parsed.canonical_file_name);

    match keep {
        KeepSide::Canonical => {
            fs::remove_file(conflict_path)
                .map_err(|error| format!("cannot remove {}: {error}", conflict_path.display()))?;
        }
        KeepSide::Copy => {
            // Same-directory rename: atomic replace on Unix. Windows
            // refuses to rename onto an existing file, so fall back to
            // remove-then-rename there — the version being kept (the
            // copy) is never the one at risk in that window.
            if let Err(error) = fs::rename(conflict_path, &canonical_path) {
                if canonical_path.exists() {
                    fs::remove_file(&canonical_path).map_err(|error| {
                        format!("cannot replace {}: {error}", canonical_path.display())
                    })?;
                    fs::rename(conflict_path, &canonical_path).map_err(|error| {
                        format!(
                            "cannot rename {} over {}: {error}",
                            conflict_path.display(),
                            canonical_path.display()
                        )
                    })?;
                } else {
                    return Err(format!(
                        "cannot rename {} over {}: {error}",
                        conflict_path.display(),
                        canonical_path.display()
                    ));
                }
            }
        }
    }

    Ok(ResolutionReport {
        canonical_path,
        removed_conflict_path: conflict_path.to_path_buf(),
        kept: match keep {
            KeepSide::Canonical => "canonical",
            KeepSide::Copy => "copy",
        },
    })
}

pub fn render_list(report: &ConflictListReport) -> String {
    if report.conflicts.is_empty() && report.skipped_roots.is_empty() {
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
                record.canonical_path.display(),
                record.conflict_path.display(),
                record.copy_size_bytes,
                record.device_id,
            ));
        }
        out.push_str(
            "\nResolve with: vapor conflicts resolve <copy-path> --keep <canonical|copy>\n",
        );
    }
    for root in &report.skipped_roots {
        out.push_str(&format!("  (skipped unreadable root {})\n", root.display()));
    }
    out.trim_end().to_string()
}

pub fn render_resolution(report: &ResolutionReport) -> String {
    format!(
        "Resolved: kept the {} version at {} (removed {}).",
        report.kept,
        report.canonical_path.display(),
        report.removed_conflict_path.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Config pointing the implicit default profile at `root`.
    fn config_for(root: &Path) -> VaporConfig {
        VaporConfig {
            local_sync_directory: root.to_string_lossy().into_owned(),
            ..VaporConfig::default()
        }
    }

    fn seed_conflict(root: &Path, canonical: &str, copy_name: &str) -> PathBuf {
        let canonical_path = root.join(canonical);
        fs::create_dir_all(canonical_path.parent().unwrap()).expect("dirs");
        fs::write(&canonical_path, b"canonical bytes").expect("seed canonical");
        let copy = canonical_path.with_file_name(copy_name);
        fs::write(&copy, b"divergent copy bytes!").expect("seed copy");
        copy
    }

    #[test]
    fn list_finds_conflict_copies_and_pairs_canonicals() {
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
        assert!(record.canonical_path.ends_with("docs/report.md"));
        assert_eq!(record.device_id, "other-mac");
        assert_eq!(record.diverged_at_ms, 1_750_000_000_123);
        assert!(record.canonical_exists);
        assert_eq!(record.canonical_size_bytes, Some(15));
        assert_eq!(record.copy_size_bytes, 21);
        assert!(report.skipped_roots.is_empty());
    }

    #[test]
    fn list_on_a_fresh_config_creates_the_root_and_reports_clean() {
        // Profile resolution creates a missing local root (product
        // policy §1), so a fresh config lists zero conflicts against
        // the just-created empty root — nothing is skipped.
        let temp = TempDir::new().expect("temp");
        let root = temp.path().join("never-created");
        let report = list_conflicts(&config_for(&root));
        assert!(report.conflicts.is_empty());
        assert!(report.skipped_roots.is_empty());
        assert!(root.is_dir(), "resolution must have created the root");
    }

    #[test]
    fn resolve_keep_canonical_deletes_only_the_copy() {
        let temp = TempDir::new().expect("temp");
        let root = temp.path().to_path_buf();
        let copy = seed_conflict(&root, "a.txt", "a~conflict-dev-1750000000123.txt");

        let report = resolve_conflict(&copy, KeepSide::Canonical).expect("resolve");
        assert_eq!(report.kept, "canonical");
        assert!(!copy.exists());
        assert_eq!(
            fs::read(root.join("a.txt")).expect("read"),
            b"canonical bytes"
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

        let error = resolve_conflict(&victim, KeepSide::Canonical).expect_err("must refuse");
        assert!(error.contains("not a Vapor conflict copy"));
        assert!(victim.exists());
    }

    #[test]
    fn json_shape_is_locked() {
        // §9.2 snapshot discipline: the --json contract apps consume.
        let report = ConflictListReport {
            conflicts: vec![ConflictRecord {
                profile_id: "default".to_string(),
                canonical_path: PathBuf::from("/r/a.txt"),
                conflict_path: PathBuf::from("/r/a~conflict-dev-1750000000123.txt"),
                device_id: "dev".to_string(),
                diverged_at_ms: 1_750_000_000_123,
                copy_size_bytes: 21,
                canonical_exists: true,
                canonical_size_bytes: Some(15),
            }],
            skipped_roots: vec![],
        };
        let rendered = serde_json::to_string_pretty(&report).expect("serialize");
        let expected = r#"{
  "conflicts": [
    {
      "profileId": "default",
      "canonicalPath": "/r/a.txt",
      "conflictPath": "/r/a~conflict-dev-1750000000123.txt",
      "deviceId": "dev",
      "divergedAtMs": 1750000000123,
      "copySizeBytes": 21,
      "canonicalExists": true,
      "canonicalSizeBytes": 15
    }
  ],
  "skippedRoots": []
}"#;
        assert_eq!(rendered, expected);
    }
}
