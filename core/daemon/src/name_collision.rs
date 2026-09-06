//! Name collisions between two remote objects and one local file.
//!
//! A cloud can hold `Readme.md` and `readme.md` side by side; a
//! case-insensitive local filesystem cannot. Applying the second object
//! to its lexical local path would land on the first one's file, so
//! the engine refuses to map a remote name whose exact spelling is
//! absent locally while a differently-cased spelling is present. The
//! object stays untouched in the cloud, the local file stays untouched,
//! and the collision is reported so the user can rename one side.
//!
//! Detection is a property of the directory, not of the OS: the exact
//! name is looked up in the parent's listing, and a differently-cased
//! sibling only counts when the filesystem resolves the target onto it.

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
        if sibling.is_none() && folds_equal(name, wanted) {
            sibling = Some(entry.path());
        }
    }
    let sibling = sibling?;
    // On a case-sensitive volume the target simply does not exist and
    // both spellings can coexist.
    fs::symlink_metadata(target).ok().map(|_| sibling)
}

fn folds_equal(left: &str, right: &str) -> bool {
    left.chars()
        .flat_map(char::to_lowercase)
        .eq(right.chars().flat_map(char::to_lowercase))
}

/// Case-folded form of a name, the key two colliding names share.
pub fn fold(name: &str) -> String {
    name.chars().flat_map(char::to_lowercase).collect()
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
