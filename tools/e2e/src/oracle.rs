//! The tree oracle: do the local root and the cloud root hold the same
//! files? Compares the file set, sizes, content hashes, and (on Unix)
//! the executable bit, after removing what both sides agree never
//! syncs: ignored names and the provider's internal files. Special
//! files (FIFOs, sockets, symlinks) are skipped and listed, never
//! compared.
//!
//! Directories are not compared on their own. Vapor materializes parents
//! when it applies children, so an empty directory legitimately exists
//! on one side only; a directory with files is covered by those files.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};
use sha2::{Digest, Sha256};
use vapor_shared::constants;

use crate::Failure;

#[derive(Clone, Debug, Default)]
pub struct OracleOptions {
    /// Extra ignore rules on top of the product defaults, in the same
    /// gitignore-style syntax a user would put in `preIgnoreRules`.
    pub extra_ignore_rules: Vec<String>,
    /// Compare the executable bit (Unix only; skipped elsewhere).
    pub compare_mode: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Difference {
    pub relative: String,
    pub detail: String,
}

#[derive(Clone, Debug, Default)]
pub struct OracleReport {
    pub differences: Vec<Difference>,
    /// Special files skipped on either side (`side: relative`).
    pub skipped: Vec<String>,
    pub files_compared: usize,
}

impl OracleReport {
    pub fn is_clean(&self) -> bool {
        self.differences.is_empty()
    }

    pub fn summary(&self, limit: usize) -> String {
        if self.is_clean() {
            return format!("trees match ({} files)", self.files_compared);
        }
        let mut lines = vec![format!(
            "{} difference(s) between local and cloud ({} files compared):",
            self.differences.len(),
            self.files_compared
        )];
        for difference in self.differences.iter().take(limit) {
            lines.push(format!("  {}: {}", difference.relative, difference.detail));
        }
        if self.differences.len() > limit {
            lines.push(format!("  ... {} more", self.differences.len() - limit));
        }
        lines.join("\n")
    }
}

/// What the oracle knows about one regular file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFacts {
    pub size: u64,
    pub executable: bool,
    pub sha256: String,
}

struct IgnoreRule {
    matcher: GlobMatcher,
    /// `true` for rules containing a `/`: matched against the relative
    /// path; otherwise matched against each path component.
    path_scoped: bool,
    directory_only: bool,
}

pub struct TreeOracle {
    rules: Vec<IgnoreRule>,
    compare_mode: bool,
}

impl TreeOracle {
    pub fn new(options: &OracleOptions) -> Result<Self, Failure> {
        let mut rules = Vec::new();
        let defaults = constants::filtering::DEFAULT_PRE_IGNORE_RULES
            .iter()
            .map(|rule| (*rule).to_string());
        for rule in defaults.chain(options.extra_ignore_rules.iter().cloned()) {
            let trimmed = rule.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
                continue;
            }
            let directory_only = trimmed.ends_with('/');
            let body = trimmed.trim_end_matches('/').trim_start_matches('/');
            let path_scoped = body.contains('/');
            let glob = GlobBuilder::new(body)
                .literal_separator(path_scoped)
                .build()
                .map_err(|error| Failure::new(format!("bad ignore rule {rule:?}: {error}")))?;
            rules.push(IgnoreRule {
                matcher: glob.compile_matcher(),
                path_scoped,
                directory_only,
            });
        }
        Ok(Self {
            rules,
            compare_mode: options.compare_mode,
        })
    }

    fn ignored(&self, relative: &str, is_dir: bool) -> bool {
        let name = relative.rsplit('/').next().unwrap_or(relative);
        if is_internal_name(name) {
            return true;
        }
        for rule in &self.rules {
            if rule.directory_only && !is_dir {
                continue;
            }
            let hit = if rule.path_scoped {
                rule.matcher.is_match(relative)
            } else {
                rule.matcher.is_match(name)
            };
            if hit {
                return true;
            }
        }
        false
    }

    /// Walks `root`, returning every regular file keyed by its
    /// slash-separated relative path, plus the special files skipped.
    pub fn snapshot(
        &self,
        root: &Path,
    ) -> Result<(BTreeMap<String, FileFacts>, Vec<String>), Failure> {
        self.collect(root)
    }

    fn collect(&self, root: &Path) -> Result<(BTreeMap<String, FileFacts>, Vec<String>), Failure> {
        let mut files = BTreeMap::new();
        let mut skipped = Vec::new();
        if !root.exists() {
            return Ok((files, skipped));
        }
        let mut pending: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
        while let Some((directory, prefix)) = pending.pop() {
            let entries = fs::read_dir(&directory).map_err(|error| {
                Failure::new(format!(
                    "oracle cannot read {}: {error}",
                    directory.display()
                ))
            })?;
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let relative = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                let metadata = fs::symlink_metadata(entry.path())?;
                let file_type = metadata.file_type();
                if file_type.is_dir() {
                    if self.ignored(&relative, true) {
                        continue;
                    }
                    pending.push((entry.path(), relative));
                } else if file_type.is_file() {
                    if self.ignored(&relative, false) {
                        continue;
                    }
                    files.insert(relative, file_facts(&entry.path(), &metadata)?);
                } else {
                    skipped.push(relative);
                }
            }
        }
        Ok((files, skipped))
    }

    pub fn compare(&self, local: &Path, cloud: &Path) -> Result<OracleReport, Failure> {
        let (local_files, local_skipped) = self.collect(local)?;
        let (cloud_files, cloud_skipped) = self.collect(cloud)?;
        let mut report = OracleReport::default();
        report.skipped.extend(
            local_skipped
                .into_iter()
                .map(|path| format!("local: {path}")),
        );
        report.skipped.extend(
            cloud_skipped
                .into_iter()
                .map(|path| format!("cloud: {path}")),
        );

        for (relative, local_facts) in &local_files {
            match cloud_files.get(relative) {
                None => report.differences.push(Difference {
                    relative: relative.clone(),
                    detail: "present locally, missing in the cloud root".to_string(),
                }),
                Some(cloud_facts) => {
                    report.files_compared += 1;
                    if local_facts.size != cloud_facts.size {
                        report.differences.push(Difference {
                            relative: relative.clone(),
                            detail: format!(
                                "size differs: local {} bytes, cloud {} bytes",
                                local_facts.size, cloud_facts.size
                            ),
                        });
                    } else if local_facts.sha256 != cloud_facts.sha256 {
                        report.differences.push(Difference {
                            relative: relative.clone(),
                            detail: format!(
                                "content differs: local sha256 {}…, cloud sha256 {}…",
                                &local_facts.sha256[..12],
                                &cloud_facts.sha256[..12]
                            ),
                        });
                    }
                    if self.compare_mode && local_facts.executable != cloud_facts.executable {
                        report.differences.push(Difference {
                            relative: relative.clone(),
                            detail: format!(
                                "executable bit differs: local {}, cloud {}",
                                local_facts.executable, cloud_facts.executable
                            ),
                        });
                    }
                }
            }
        }
        for relative in cloud_files.keys() {
            if !local_files.contains_key(relative) {
                report.differences.push(Difference {
                    relative: relative.clone(),
                    detail: "present in the cloud root, missing locally".to_string(),
                });
            }
        }
        Ok(report)
    }
}

pub fn is_internal_name(name: &str) -> bool {
    constants::filtering::INTERNAL_IGNORE_FILE_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || constants::filtering::INTERNAL_IGNORE_FILE_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn file_facts(path: &Path, metadata: &fs::Metadata) -> Result<FileFacts, Failure> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let sha256 = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(FileFacts {
        size: metadata.len(),
        executable: is_executable(metadata),
        sha256,
    })
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

/// SHA-256 of a file, hex. Shared with scenarios that pin content.
pub fn sha256_of(path: &Path) -> Result<String, Failure> {
    let metadata = fs::metadata(path)?;
    Ok(file_facts(path, &metadata)?.sha256)
}

/// Convenience for the CLI: compare two roots with the default rules.
pub fn verify_trees(
    local: &Path,
    cloud: &Path,
    options: &OracleOptions,
) -> Result<OracleReport, Failure> {
    TreeOracle::new(options)?.compare(local, cloud)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn oracle() -> TreeOracle {
        TreeOracle::new(&OracleOptions {
            extra_ignore_rules: vec!["*.skipme".to_string(), "junk/".to_string()],
            compare_mode: true,
        })
        .expect("oracle")
    }

    #[test]
    fn matching_trees_are_clean_and_ignored_names_do_not_count() {
        let temp = TempDir::new().expect("temp");
        let local = temp.path().join("local");
        let cloud = temp.path().join("cloud");
        fs::create_dir_all(local.join("sub")).unwrap();
        fs::create_dir_all(cloud.join("sub")).unwrap();
        fs::write(local.join("a.txt"), b"same").unwrap();
        fs::write(cloud.join("a.txt"), b"same").unwrap();
        fs::write(local.join("sub/b.txt"), b"nested").unwrap();
        fs::write(cloud.join("sub/b.txt"), b"nested").unwrap();
        // Ignored on both sides with divergent content.
        fs::write(local.join(".DS_Store"), b"l").unwrap();
        fs::write(cloud.join(".DS_Store"), b"c").unwrap();
        fs::write(local.join("x.skipme"), b"only local").unwrap();
        fs::create_dir_all(cloud.join("junk")).unwrap();
        fs::write(cloud.join("junk/inside.txt"), b"only cloud").unwrap();
        // Provider internals.
        fs::write(
            cloud.join(format!("{}stage", constants::provider::TEMP_FILE_PREFIX)),
            b"tmp",
        )
        .unwrap();
        let report = oracle().compare(&local, &cloud).expect("compare");
        assert!(report.is_clean(), "{}", report.summary(10));
        assert_eq!(report.files_compared, 2);
    }

    #[test]
    fn every_kind_of_divergence_is_named() {
        let temp = TempDir::new().expect("temp");
        let local = temp.path().join("local");
        let cloud = temp.path().join("cloud");
        fs::create_dir_all(&local).unwrap();
        fs::create_dir_all(&cloud).unwrap();
        fs::write(local.join("only-local.txt"), b"l").unwrap();
        fs::write(cloud.join("only-cloud.txt"), b"c").unwrap();
        fs::write(local.join("size.txt"), b"1234").unwrap();
        fs::write(cloud.join("size.txt"), b"12").unwrap();
        fs::write(local.join("content.txt"), b"aaaa").unwrap();
        fs::write(cloud.join("content.txt"), b"bbbb").unwrap();
        let report = oracle().compare(&local, &cloud).expect("compare");
        let details: Vec<String> = report
            .differences
            .iter()
            .map(|d| format!("{}: {}", d.relative, d.detail))
            .collect();
        assert_eq!(report.differences.len(), 4, "{details:?}");
        assert!(
            details
                .iter()
                .any(|d| d.starts_with("only-local.txt: present locally"))
        );
        assert!(
            details
                .iter()
                .any(|d| d.starts_with("only-cloud.txt: present in the cloud"))
        );
        assert!(
            details
                .iter()
                .any(|d| d.starts_with("size.txt: size differs"))
        );
        assert!(
            details
                .iter()
                .any(|d| d.starts_with("content.txt: content differs"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn executable_bit_is_compared_when_asked() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new().expect("temp");
        let local = temp.path().join("local");
        let cloud = temp.path().join("cloud");
        fs::create_dir_all(&local).unwrap();
        fs::create_dir_all(&cloud).unwrap();
        fs::write(local.join("run.sh"), b"#!/bin/sh\n").unwrap();
        fs::write(cloud.join("run.sh"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(local.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        let report = oracle().compare(&local, &cloud).expect("compare");
        assert_eq!(report.differences.len(), 1);
        assert!(report.differences[0].detail.contains("executable bit"));
    }
}
