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

/// The marker every keep-both conflict copy carries in its file name.
/// Surfaces that list or resolve conflicts (the `vapor conflicts`
/// command, and the app UIs driving it) recognize copies by this
/// marker via [`parse_conflict_copy_name`].
pub const CONFLICT_MARKER: &str = "~conflict-";

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

    let base = format!("{stem}{CONFLICT_MARKER}{device_id}-{timestamp_ms}");
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

/// A conflict-copy file name decomposed back into its parts — the
/// inverse of [`conflict_copy_path`] for names that machine derived.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConflictCopyName {
    /// The canonical file name the copy diverged from
    /// (`report~conflict-dev-17….md` → `report.md`).
    pub canonical_file_name: String,
    /// The device whose divergent version the copy preserves.
    pub device_id: String,
    /// When the divergence was observed (the conflicting intent's
    /// event time, epoch milliseconds).
    pub timestamp_ms: u64,
    /// Collision sequence (`-2`, `-3`, …); `None` for the first copy.
    pub sequence: Option<u64>,
}

/// Epoch-millisecond timestamps are 12+ digits for any date past 2001.
/// Requiring that length is what disambiguates the timestamp segment
/// from device ids that end in digits (`mbp2`) and from the small
/// collision sequence.
const TIMESTAMP_MIN_DIGITS: usize = 12;

/// Parses a file name produced by [`conflict_copy_path`]. Returns
/// `None` for anything that does not match the machine-generated
/// grammar (`{stem}~conflict-{device_id}-{timestamp_ms}[-{seq}]{ext}`),
/// so user files that merely contain the marker text do not
/// false-positive.
pub fn parse_conflict_copy_name(file_name: &str) -> Option<ConflictCopyName> {
    let marker_index = file_name.find(CONFLICT_MARKER)?;
    let stem = &file_name[..marker_index];
    if stem.is_empty() {
        return None;
    }
    let rest = &file_name[marker_index + CONFLICT_MARKER.len()..];
    // The generator splits the original name at its last dot, so the
    // copy's extension is whatever follows the last dot after the
    // marker (device ids are hostname-derived slugs; they never
    // contain dots).
    let (meta, extension) = match rest.rfind('.') {
        Some(index) => rest.split_at(index),
        None => (rest, ""),
    };

    let segments: Vec<&str> = meta.split('-').collect();
    let timestamp_index = segments
        .iter()
        .rposition(|s| s.len() >= TIMESTAMP_MIN_DIGITS && s.chars().all(|c| c.is_ascii_digit()))?;
    if timestamp_index == 0 {
        return None; // no device id segment before the timestamp
    }
    let timestamp_ms = segments[timestamp_index].parse::<u64>().ok()?;
    let device_id = segments[..timestamp_index].join("-");
    let sequence = match &segments[timestamp_index + 1..] {
        [] => None,
        [seq] => Some(seq.parse::<u64>().ok()?),
        _ => return None,
    };

    Some(ConflictCopyName {
        canonical_file_name: format!("{stem}{extension}"),
        device_id,
        timestamp_ms,
        sequence,
    })
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
    fn parse_round_trips_generated_names() {
        // Realistic epoch-ms timestamps: the parser requires 12+ digits
        // to tell the timestamp apart from digit-suffixed device ids.
        let cases = [
            ("report.md", "alexs-mbp", 1_750_000_000_123_u64),
            ("Makefile", "dev", 1_750_000_000_123),
            (".env", "mbp2", 1_750_000_000_123),
            ("archive.tar.gz", "work-mac-2", 1_750_000_000_123),
        ];
        for (original, device_id, timestamp_ms) in cases {
            let derived = conflict_copy_path(
                &PathBuf::from("/r").join(original),
                device_id,
                timestamp_ms,
                never_exists,
            );
            let name = derived.file_name().unwrap().to_str().unwrap();
            let parsed = parse_conflict_copy_name(name)
                .unwrap_or_else(|| panic!("generated name must parse: {name}"));
            // Multi-dot originals lose their inner stem dots to the
            // canonical reconstruction only when the generator split
            // them — round-trip must restore the full original name.
            assert_eq!(parsed.canonical_file_name, original, "for {name}");
            assert_eq!(parsed.device_id, device_id, "for {name}");
            assert_eq!(parsed.timestamp_ms, timestamp_ms, "for {name}");
            assert_eq!(parsed.sequence, None, "for {name}");
        }
    }

    #[test]
    fn parse_recovers_the_collision_sequence() {
        let parsed = parse_conflict_copy_name("a~conflict-dev-1750000000123-3.txt").expect("parse");
        assert_eq!(parsed.canonical_file_name, "a.txt");
        assert_eq!(parsed.device_id, "dev");
        assert_eq!(parsed.timestamp_ms, 1_750_000_000_123);
        assert_eq!(parsed.sequence, Some(3));
    }

    #[test]
    fn parse_rejects_names_that_only_look_conflicted() {
        // User files containing the marker text but not the generated
        // grammar must not be treated as conflict copies.
        for name in [
            "notes~conflict-resolution.md",         // no timestamp segment
            "~conflict-dev-1750000000123.md",       // empty stem
            "a~conflict-1750000000123.txt",         // no device id segment
            "a~conflict-dev-1750000000123-2-9.txt", // trailing garbage
            "plain.txt",
        ] {
            assert_eq!(parse_conflict_copy_name(name), None, "{name}");
        }
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
