//! Keep-both conflict-copy path derivation (C8-14).
//!
//! Template per `docs/architecture/data-flow.md §Conflict handling`:
//! `{stem}~conflict-{device_id}-{timestamp_ms}{ext}`, where the split
//! happens at the last `.` of the basename. When the derived path
//! already exists, `-{seq}` (starting at 2) appends before the
//! extension until a free path is found. Every input is derivable from
//! durable state (device id from `vapor.json`, timestamp from the
//! intent's event time), so the same conflict replayed on the same
//! device lands on the same conflict path across restarts.

use std::path::{Path, PathBuf};

/// Derives the keep-both conflict-copy path for `original`.
/// `path_exists` abstracts the existence probe so derivation stays
/// pure and testable.
pub fn conflict_copy_path(
    original: &Path,
    device_id: &str,
    timestamp_ms: u64,
    path_exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    let file_name = original
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, extension) = match file_name.rfind('.') {
        // A leading dot (hidden file) is part of the stem, not an
        // extension separator.
        Some(0) | None => (file_name.as_str(), ""),
        Some(index) => file_name.split_at(index),
    };

    let base = format!("{stem}~conflict-{device_id}-{timestamp_ms}");
    let candidate = original.with_file_name(format!("{base}{extension}"));
    if !path_exists(&candidate) {
        return candidate;
    }
    let mut sequence: u64 = 2;
    loop {
        let candidate = original.with_file_name(format!("{base}-{sequence}{extension}"));
        if !path_exists(&candidate) {
            return candidate;
        }
        sequence += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never_exists(_: &Path) -> bool {
        false
    }

    #[test]
    fn suffix_lands_between_stem_and_extension() {
        let derived = conflict_copy_path(
            Path::new("/root/docs/report.md"),
            "alexs-mbp",
            1_750_000_000_123,
            never_exists,
        );
        assert_eq!(
            derived,
            PathBuf::from("/root/docs/report~conflict-alexs-mbp-1750000000123.md")
        );
    }

    #[test]
    fn extensionless_and_hidden_files_append_the_suffix() {
        assert_eq!(
            conflict_copy_path(Path::new("/r/Makefile"), "dev", 5, never_exists),
            PathBuf::from("/r/Makefile~conflict-dev-5")
        );
        assert_eq!(
            conflict_copy_path(Path::new("/r/.env"), "dev", 5, never_exists),
            PathBuf::from("/r/.env~conflict-dev-5")
        );
    }

    #[test]
    fn multi_dot_names_split_at_the_last_dot() {
        assert_eq!(
            conflict_copy_path(Path::new("/r/archive.tar.gz"), "dev", 7, never_exists),
            PathBuf::from("/r/archive.tar~conflict-dev-7.gz")
        );
    }

    #[test]
    fn collisions_walk_the_sequence_from_two() {
        let taken = [
            PathBuf::from("/r/a~conflict-dev-9.txt"),
            PathBuf::from("/r/a~conflict-dev-9-2.txt"),
        ];
        let derived = conflict_copy_path(Path::new("/r/a.txt"), "dev", 9, |candidate| {
            taken.iter().any(|t| t == candidate)
        });
        assert_eq!(derived, PathBuf::from("/r/a~conflict-dev-9-3.txt"));
    }

    #[test]
    fn derivation_is_deterministic_for_identical_inputs() {
        // The determinism contract: same device + same event timestamp
        // → same conflict path across daemon restarts.
        let a = conflict_copy_path(Path::new("/r/f.rs"), "dev-1", 42, never_exists);
        let b = conflict_copy_path(Path::new("/r/f.rs"), "dev-1", 42, never_exists);
        assert_eq!(a, b);
        let c = conflict_copy_path(Path::new("/r/f.rs"), "dev-2", 42, never_exists);
        assert_ne!(a, c, "different devices must not collide");
    }
}
