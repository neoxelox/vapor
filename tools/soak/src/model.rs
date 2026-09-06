//! The model of what must survive, and the oracle that checks the two
//! real trees against it.
//!
//! The generator tells the model about every operation it performed
//! and the hash of what it wrote. Phases are separated by quiescence,
//! and inside a phase a path has one writer, so the model's "last
//! write per path" is exactly what both trees must hold once the
//! daemon has converged. The conflict phase is the deliberate
//! exception: it edits the same paths on both sides and the model
//! only demands that both payloads survive somewhere.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use vapor_e2e::oracle::{FileFacts, OracleOptions, TreeOracle};

use crate::Failure;
use crate::workload::{Op, Side};

/// The sync mode the daemon under test runs in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncMode {
    TwoWay,
    PullOnly,
    PushOnly,
}

impl SyncMode {
    pub fn label(self) -> &'static str {
        match self {
            SyncMode::TwoWay => "two-way",
            SyncMode::PullOnly => "pull-only",
            SyncMode::PushOnly => "push-only",
        }
    }

    /// The side whose writes define the truth, if the mode has one.
    pub fn authority(self) -> Option<Side> {
        match self {
            SyncMode::TwoWay => None,
            SyncMode::PullOnly => Some(Side::Cloud),
            SyncMode::PushOnly => Some(Side::Local),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Expected {
    pub version: u64,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
    /// Which side wrote this version.
    pub writer: Side,
}

/// A path edited on both sides inside one conflict phase.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Contested {
    pub local: Expected,
    pub cloud: Expected,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Model {
    pub seed: u64,
    pub mode: Option<SyncMode>,
    pub next_version: u64,
    /// Last write per path across both sides (single-writer phases).
    pub files: BTreeMap<String, Expected>,
    /// Paths edited on both sides in the current conflict phase.
    pub contested: BTreeMap<String, Contested>,
    /// Every content hash the generator ever produced, so a file with
    /// unknown content can be recognised as an invention.
    pub known_hashes: BTreeSet<String>,
    /// Writes made on the non-authoritative side of a one-way mode;
    /// the daemon must undo them.
    pub to_be_reverted: BTreeMap<String, Expected>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Violation {
    pub kind: String,
    pub path: String,
    pub detail: String,
}

impl Model {
    pub fn new(seed: u64, mode: SyncMode) -> Self {
        Self {
            seed,
            mode: Some(mode),
            next_version: 1,
            ..Self::default()
        }
    }

    pub fn allocate_version(&mut self) -> u64 {
        let version = self.next_version;
        self.next_version += 1;
        version
    }

    fn mode(&self) -> SyncMode {
        self.mode.unwrap_or(SyncMode::TwoWay)
    }

    /// Whether writes on `side` are the truth in this mode.
    fn writes_count(&self, side: Side) -> bool {
        match self.mode().authority() {
            None => true,
            Some(authority) => authority == side,
        }
    }

    /// Records an operation the generator applied on `side`. `written`
    /// carries the facts of the file the operation produced, when it
    /// produced one.
    pub fn record(&mut self, side: Side, op: &Op, written: Option<Expected>) {
        let counts = self.writes_count(side);
        match op {
            Op::Create { path, .. }
            | Op::Overwrite { path, .. }
            | Op::SameSizeEdit { path }
            | Op::Append { path, .. }
            | Op::Chmod { path, .. } => {
                if let Some(expected) = written {
                    self.known_hashes.insert(expected.sha256.clone());
                    if counts {
                        self.files.insert(path.clone(), expected);
                    } else {
                        self.to_be_reverted.insert(path.clone(), expected);
                    }
                }
            }
            Op::Rename { from, to } | Op::Move { from, to } => {
                if counts {
                    if let Some(mut expected) = self.files.remove(from) {
                        expected.writer = side;
                        self.files.insert(to.clone(), expected);
                    }
                } else if let Some(expected) = self
                    .files
                    .get(from)
                    .cloned()
                    .or_else(|| self.to_be_reverted.remove(from))
                {
                    // The authoritative copy stays at `from`; the moved
                    // copy at `to` must disappear again.
                    self.to_be_reverted.insert(to.clone(), expected);
                }
            }
            Op::Delete { path } => {
                if counts {
                    self.files.remove(path);
                } else {
                    self.to_be_reverted.remove(path);
                }
            }
            Op::DeleteTree { dir } => {
                let prefix = format!("{dir}/");
                if counts {
                    self.files.retain(|path, _| !path.starts_with(&prefix));
                } else {
                    self.to_be_reverted
                        .retain(|path, _| !path.starts_with(&prefix));
                }
            }
            Op::Mkdir { .. } => {}
        }
    }

    /// Records a deliberate same-path edit on both sides.
    pub fn record_contested(&mut self, path: &str, local: Expected, cloud: Expected) {
        self.known_hashes.insert(local.sha256.clone());
        self.known_hashes.insert(cloud.sha256.clone());
        self.files.remove(path);
        self.contested
            .insert(path.to_string(), Contested { local, cloud });
    }

    /// Paths the generator currently owns on `side` (files it may
    /// edit). In two-way both sides share the set; in a one-way mode
    /// the non-authoritative side edits and expects reverts.
    pub fn owned_paths(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    pub fn directories(&self) -> Vec<String> {
        let mut dirs = BTreeSet::new();
        for path in self.files.keys() {
            let mut current = path.as_str();
            while let Some(index) = current.rfind('/') {
                current = &current[..index];
                dirs.insert(current.to_string());
            }
        }
        dirs.into_iter().collect()
    }

    pub fn live_files(&self) -> usize {
        self.files.len()
    }

    /// After the daemon has converged and every conflict copy has
    /// replicated, promotes the contested paths back into the plain
    /// model: whichever version holds the canonical name is the model's
    /// version from now on; the conflict copies are ordinary files.
    pub fn absorb_conflicts(&mut self, local_tree: &BTreeMap<String, FileFacts>) {
        let contested = std::mem::take(&mut self.contested);
        for (path, versions) in contested {
            let Some(actual) = local_tree.get(&path) else {
                continue;
            };
            let winner = if actual.sha256 == versions.local.sha256 {
                versions.local
            } else {
                versions.cloud
            };
            self.files.insert(path, winner);
        }
        // Conflict copies now exist as files the generator did not
        // write; adopt them so later phases can edit or delete them.
        for (path, facts) in local_tree {
            if path.contains("~conflict-")
                && !self.files.contains_key(path)
                && self.known_hashes.contains(&facts.sha256)
            {
                self.files.insert(
                    path.clone(),
                    Expected {
                        version: 0,
                        size: facts.size,
                        sha256: facts.sha256.clone(),
                        executable: facts.executable,
                        writer: Side::Local,
                    },
                );
            }
        }
    }

    /// Adopts files the daemon created that the model does not know by
    /// path but does know by content: a restored file at a moved path,
    /// a conflict copy. Called after convergence so the generator's
    /// owned set stays truthful.
    pub fn adopt_known_content(&mut self, local_tree: &BTreeMap<String, FileFacts>) {
        for (path, facts) in local_tree {
            if !self.files.contains_key(path) && self.known_hashes.contains(&facts.sha256) {
                self.files.insert(
                    path.clone(),
                    Expected {
                        version: 0,
                        size: facts.size,
                        sha256: facts.sha256.clone(),
                        executable: facts.executable,
                        writer: Side::Local,
                    },
                );
            }
        }
    }

    /// The oracle. Compares both real trees against the model.
    pub fn check(&self, local_root: &Path, cloud_root: &Path) -> Result<Vec<Violation>, Failure> {
        // Modes carry over on transfers but a permissions-only change
        // does not propagate (`data-flow.md`), so the executable bit is
        // not part of the convergence contract here.
        let oracle = TreeOracle::new(&OracleOptions {
            extra_ignore_rules: Vec::new(),
            compare_mode: false,
        })?;
        let (local, local_skipped) = oracle.snapshot(local_root)?;
        let (cloud, cloud_skipped) = oracle.snapshot(cloud_root)?;
        let mut violations = Vec::new();
        for skipped in local_skipped.into_iter().chain(cloud_skipped) {
            violations.push(Violation {
                kind: "special-file".to_string(),
                path: skipped,
                detail:
                    "a special file appeared in a tree the workload only writes regular files to"
                        .to_string(),
            });
        }

        // 1. Convergence: both trees hold the same files.
        let comparison = oracle.compare(local_root, cloud_root)?;
        for difference in comparison.differences {
            violations.push(Violation {
                kind: "divergence".to_string(),
                path: difference.relative,
                detail: difference.detail,
            });
        }

        // 2. Every expected file exists with the expected content on
        //    both sides; deleted paths are gone from both.
        for (path, expected) in &self.files {
            for (side, tree) in [("local", &local), ("cloud", &cloud)] {
                match tree.get(path) {
                    None => violations.push(Violation {
                        kind: "loss".to_string(),
                        path: path.clone(),
                        detail: format!(
                            "missing on the {side} side; version {} ({} bytes) written by the {} side",
                            expected.version,
                            expected.size,
                            expected.writer.label()
                        ),
                    }),
                    Some(facts) if facts.sha256 != expected.sha256 => violations.push(Violation {
                        kind: "wrong-content".to_string(),
                        path: path.clone(),
                        detail: format!(
                            "{side} side holds {} bytes (sha256 {}…) but version {} written by the {} side has {} bytes (sha256 {}…)",
                            facts.size,
                            &facts.sha256[..12],
                            expected.version,
                            expected.writer.label(),
                            expected.size,
                            &expected.sha256[..12]
                        ),
                    }),
                    Some(_) => {}
                }
            }
        }

        // 3. Contested paths: both payloads survive somewhere; the
        //    canonical is one of them.
        let all_hashes: BTreeSet<&str> = local
            .values()
            .chain(cloud.values())
            .map(|facts| facts.sha256.as_str())
            .collect();
        for (path, versions) in &self.contested {
            for (label, expected) in [
                ("local edit", &versions.local),
                ("cloud edit", &versions.cloud),
            ] {
                if !all_hashes.contains(expected.sha256.as_str()) {
                    violations.push(Violation {
                        kind: "loss".to_string(),
                        path: path.clone(),
                        detail: format!(
                            "the {label} (version {}, {} bytes) survives nowhere, neither canonical nor as a conflict copy",
                            expected.version, expected.size
                        ),
                    });
                }
            }
            match local.get(path) {
                Some(facts)
                    if facts.sha256 == versions.local.sha256
                        || facts.sha256 == versions.cloud.sha256 => {}
                Some(facts) => violations.push(Violation {
                    kind: "wrong-content".to_string(),
                    path: path.clone(),
                    detail: format!(
                        "the canonical holds neither contested version (sha256 {}…)",
                        &facts.sha256[..12]
                    ),
                }),
                None => violations.push(Violation {
                    kind: "loss".to_string(),
                    path: path.clone(),
                    detail: "the contested path has no canonical file locally".to_string(),
                }),
            }
        }

        // 4. Reverts: writes on the non-authoritative side are undone.
        for (path, expected) in &self.to_be_reverted {
            for (side, tree) in [("local", &local), ("cloud", &cloud)] {
                if let Some(facts) = tree.get(path)
                    && facts.sha256 == expected.sha256
                    && self
                        .files
                        .get(path)
                        .is_none_or(|truth| truth.sha256 != expected.sha256)
                {
                    violations.push(Violation {
                        kind: "not-reverted".to_string(),
                        path: path.clone(),
                        detail: format!(
                            "the {side} side still holds a write the {} mode must undo",
                            self.mode().label()
                        ),
                    });
                }
            }
        }

        // 5. No invention: every file's content was written by the
        //    workload at some point (conflict copies included).
        for (side, tree) in [("local", &local), ("cloud", &cloud)] {
            for (path, facts) in tree {
                if !self.known_hashes.contains(&facts.sha256) {
                    violations.push(Violation {
                        kind: "invention".to_string(),
                        path: path.clone(),
                        detail: format!(
                            "{side} side holds {} bytes (sha256 {}…) the workload never wrote: a truncated or corrupted payload",
                            facts.size,
                            &facts.sha256[..12]
                        ),
                    });
                }
            }
        }
        Ok(violations)
    }

    pub fn save(&self, path: &Path) -> Result<(), Failure> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, Failure> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::{SizeClass, content_for, sha256_hex};
    use std::fs;
    use tempfile::TempDir;

    fn expected(path: &str, version: u64, size: u64, writer: Side) -> (Vec<u8>, Expected) {
        let bytes = content_for(1, path, version, size);
        let facts = Expected {
            version,
            size,
            sha256: sha256_hex(&bytes),
            executable: false,
            writer,
        };
        (bytes, facts)
    }

    fn write(root: &Path, path: &str, bytes: &[u8]) {
        let full = root.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, bytes).unwrap();
    }

    #[test]
    fn converged_trees_matching_the_model_are_clean() {
        let temp = TempDir::new().unwrap();
        let (local, cloud) = (temp.path().join("l"), temp.path().join("c"));
        let mut model = Model::new(1, SyncMode::TwoWay);
        let (bytes, facts) = expected("a/one.txt", 1, 200, Side::Local);
        write(&local, "a/one.txt", &bytes);
        write(&cloud, "a/one.txt", &bytes);
        model.record(
            Side::Local,
            &Op::Create {
                path: "a/one.txt".into(),
                size: SizeClass::Small,
            },
            Some(facts),
        );
        assert!(model.check(&local, &cloud).unwrap().is_empty());
    }

    #[test]
    fn every_kind_of_violation_is_named() {
        let temp = TempDir::new().unwrap();
        let (local, cloud) = (temp.path().join("l"), temp.path().join("c"));
        let mut model = Model::new(1, SyncMode::TwoWay);
        // Lost on the cloud side.
        let (bytes, facts) = expected("lost.txt", 1, 100, Side::Local);
        write(&local, "lost.txt", &bytes);
        model.record(
            Side::Local,
            &Op::Create {
                path: "lost.txt".into(),
                size: SizeClass::Small,
            },
            Some(facts),
        );
        // Wrong content on the local side (an older version).
        let (old, old_facts) = expected("edited.txt", 1, 100, Side::Local);
        let (new, new_facts) = expected("edited.txt", 2, 100, Side::Local);
        model.record(
            Side::Local,
            &Op::Create {
                path: "edited.txt".into(),
                size: SizeClass::Small,
            },
            Some(old_facts),
        );
        model.record(
            Side::Local,
            &Op::Overwrite {
                path: "edited.txt".into(),
                size: SizeClass::Small,
            },
            Some(new_facts),
        );
        write(&local, "edited.txt", &old);
        write(&cloud, "edited.txt", &new);
        // Invented content.
        write(&cloud, "ghost.txt", b"never written by the workload");
        let violations = model.check(&local, &cloud).unwrap();
        let kinds: Vec<(&str, &str)> = violations
            .iter()
            .map(|v| (v.kind.as_str(), v.path.as_str()))
            .collect();
        assert!(kinds.contains(&("loss", "lost.txt")), "{kinds:?}");
        assert!(
            kinds.contains(&("wrong-content", "edited.txt")),
            "{kinds:?}"
        );
        assert!(kinds.contains(&("invention", "ghost.txt")), "{kinds:?}");
        assert!(kinds.contains(&("divergence", "lost.txt")), "{kinds:?}");
    }

    #[test]
    fn contested_paths_need_both_payloads_somewhere() {
        let temp = TempDir::new().unwrap();
        let (local, cloud) = (temp.path().join("l"), temp.path().join("c"));
        let mut model = Model::new(1, SyncMode::TwoWay);
        let (local_bytes, local_facts) = expected("doc.txt", 5, 100, Side::Local);
        let (cloud_bytes, cloud_facts) = expected("doc.txt", 6, 100, Side::Cloud);
        model.record_contested("doc.txt", local_facts, cloud_facts);
        // Keep-both outcome: canonical carries the cloud edit, the local
        // edit survives as a replicated conflict copy.
        for root in [&local, &cloud] {
            write(root, "doc.txt", &cloud_bytes);
            write(root, "doc~conflict-dev-1.txt", &local_bytes);
        }
        assert!(model.check(&local, &cloud).unwrap().is_empty());
        // Losing the conflict copy on both sides is a loss.
        fs::remove_file(local.join("doc~conflict-dev-1.txt")).unwrap();
        fs::remove_file(cloud.join("doc~conflict-dev-1.txt")).unwrap();
        let violations = model.check(&local, &cloud).unwrap();
        assert!(violations.iter().any(|v| v.kind == "loss"));
    }

    #[test]
    fn one_way_mode_expects_the_non_authoritative_write_to_be_undone() {
        let temp = TempDir::new().unwrap();
        let (local, cloud) = (temp.path().join("l"), temp.path().join("c"));
        let mut model = Model::new(1, SyncMode::PullOnly);
        let (truth, truth_facts) = expected("doc.txt", 1, 100, Side::Cloud);
        model.record(
            Side::Cloud,
            &Op::Create {
                path: "doc.txt".into(),
                size: SizeClass::Small,
            },
            Some(truth_facts),
        );
        let (edit, edit_facts) = expected("doc.txt", 2, 100, Side::Local);
        model.record(
            Side::Local,
            &Op::Overwrite {
                path: "doc.txt".into(),
                size: SizeClass::Small,
            },
            Some(edit_facts),
        );
        write(&cloud, "doc.txt", &truth);
        write(&local, "doc.txt", &edit);
        let violations = model.check(&local, &cloud).unwrap();
        assert!(
            violations.iter().any(|v| v.kind == "not-reverted"),
            "{violations:?}"
        );
        write(&local, "doc.txt", &truth);
        assert!(model.check(&local, &cloud).unwrap().is_empty());
    }
}
