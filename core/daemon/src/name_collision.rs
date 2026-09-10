//! Name collisions between two remote objects and one local file.
//!
//! A cloud can hold `Readme.md` and `readme.md` side by side, or
//! `café` spelled with a precomposed `é` next to `café` spelled with
//! `e` plus a combining accent; a local filesystem that folds case or
//! normalizes names (APFS does both) cannot. Applying the second object
//! to its lexical local path would land on the first one's file, so
//! the engine refuses to map a remote name whose exact spelling is
//! absent locally while a spelling the filesystem treats as the same
//! name is present. The object stays untouched in the cloud, the local
//! file stays untouched, and the collision is reported so the user can
//! rename one side.
//!
//! Detection is a property of the directory, not of the OS: the exact
//! name is looked up in the parent's listing, and a sibling that folds
//! to the same key only counts when the filesystem resolves the target
//! onto it.

use std::fs;
use std::path::{Path, PathBuf};

/// The existing local file `target` would alias, if any.
pub fn colliding_local_path(target: &Path) -> Option<PathBuf> {
    let parent = target.parent()?;
    let wanted = target.file_name()?.to_str()?;
    let entries = fs::read_dir(parent).ok()?;
    let mut sibling = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == wanted {
            // The exact spelling exists: no collision.
            return None;
        }
        if sibling.is_none() && fold(name) == fold(wanted) {
            sibling = Some(entry.path());
        }
    }
    let sibling = sibling?;
    // On a volume that keeps both spellings apart the target simply
    // does not exist and both can coexist.
    fs::symlink_metadata(target).ok().map(|_| sibling)
}

/// The key two colliding names share: composed (NFC) and case-folded,
/// so `Readme.md` and `readme.md` fold together, and so do the two
/// spellings of `café`.
pub fn fold(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    name.nfc().flat_map(char::to_lowercase).collect()
}

/// Whether the local filesystem of this host treats differently-cased
/// spellings as one name. Answered per host today; a per-volume probe
/// would refine it for a sync root on a case-sensitive volume.
pub fn local_filesystem_folds_case() -> bool {
    use vapor_platform::fs_caps::{
        CaseSensitivity, FilesystemCapabilities, NativeFilesystemCapabilities,
    };
    NativeFilesystemCapabilities::for_current_host().case_sensitivity()
        == CaseSensitivity::Insensitive
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn exact_spelling_present_is_not_a_collision() {
        let dir = TempDir::new().expect("dir");
        fs::write(dir.path().join("Readme.md"), b"x").expect("seed");
        assert_eq!(colliding_local_path(&dir.path().join("Readme.md")), None);
    }

    #[test]
    fn absent_name_with_no_sibling_is_not_a_collision() {
        let dir = TempDir::new().expect("dir");
        fs::write(dir.path().join("other.md"), b"x").expect("seed");
        assert_eq!(colliding_local_path(&dir.path().join("readme.md")), None);
    }

    #[test]
    fn a_differently_normalized_sibling_collides_only_where_the_volume_aliases_it() {
        let dir = TempDir::new().expect("dir");
        let composed = dir.path().join("caf\u{e9}.txt");
        let decomposed = dir.path().join("cafe\u{301}.txt");
        fs::write(&composed, b"x").expect("seed");
        // APFS resolves either spelling onto the same file; ext4 keeps
        // them apart, and then both may exist.
        let expected = if fs::symlink_metadata(&decomposed).is_ok() {
            Some(composed.clone())
        } else {
            None
        };
        assert_eq!(colliding_local_path(&decomposed), expected);
        assert_eq!(fold("Caf\u{e9}.TXT"), fold("cafe\u{301}.txt"));
    }

    #[test]
    fn differently_cased_sibling_collides_only_where_the_volume_aliases_it() {
        let dir = TempDir::new().expect("dir");
        let existing = dir.path().join("Readme.md");
        fs::write(&existing, b"x").expect("seed");
        let target = dir.path().join("readme.md");
        let expected = if fs::symlink_metadata(&target).is_ok() {
            Some(existing)
        } else {
            None
        };
        assert_eq!(colliding_local_path(&target), expected);
    }
}
