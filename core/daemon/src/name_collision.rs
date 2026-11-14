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

use std::path::{Path, PathBuf};

pub use vapor_shared::paths::StoredSpelling as LocalSpelling;

/// How the local filesystem spells the entry that `path` resolves to.
pub fn local_spelling(path: &Path) -> LocalSpelling {
    vapor_shared::paths::stored_spelling(path)
}

/// Whether `path` is on this device under exactly this spelling.
pub fn exactly_present(path: &Path) -> bool {
    local_spelling(path) == LocalSpelling::Exact
}

/// The existing local file `target` would alias, if any: the entry is
/// there under another spelling of its own name. A parent folder that
/// is spelled differently is not an alias of the file.
pub fn colliding_local_path(target: &Path) -> Option<PathBuf> {
    match local_spelling(target) {
        LocalSpelling::Other(stored) if stored.file_name() != target.file_name() => {
            Some(target.with_file_name(stored.file_name()?))
        }
        _ => None,
    }
}

/// The key two colliding names share.
pub fn fold(name: &str) -> String {
    vapor_shared::paths::fold_name(name)
}

/// Whether the volume that holds `directory` treats differently-cased
/// spellings as one name. The volume answers where the OS can say (a
/// case-sensitive volume on macOS holds `Readme.md` and `readme.md`
/// side by side); the host default answers otherwise, and for a
/// directory that is not there yet.
pub fn folds_case_at(directory: &Path) -> bool {
    use vapor_platform::fs_caps::{
        CaseSensitivity, FilesystemCapabilities, NativeFilesystemCapabilities,
    };
    NativeFilesystemCapabilities::for_current_host().case_sensitivity_at(directory)
        == CaseSensitivity::Insensitive
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn the_stored_spelling_is_reported_where_the_volume_folds_names() {
        let dir = TempDir::new().expect("dir");
        let root = vapor_shared::paths::canonicalize(dir.path()).expect("canonical");
        fs::create_dir(root.join("Photos")).expect("dir");
        let stored = root.join("Photos/Report.txt");
        fs::write(&stored, b"x").expect("seed");

        assert_eq!(local_spelling(&stored), LocalSpelling::Exact);
        assert_eq!(
            local_spelling(&root.join("Photos/missing.txt")),
            LocalSpelling::Absent
        );
        for other in [
            "Photos/report.txt",
            "photos/Report.txt",
            "PHOTOS/REPORT.TXT",
        ] {
            let other = root.join(other);
            // A volume that keeps the spellings apart has no such entry.
            let expected = if fs::symlink_metadata(&other).is_ok() {
                LocalSpelling::Other(stored.clone())
            } else {
                LocalSpelling::Absent
            };
            assert_eq!(local_spelling(&other), expected, "{}", other.display());
        }
    }

    #[test]
    fn a_folder_spelled_differently_is_not_an_alias_of_the_file() {
        let dir = TempDir::new().expect("dir");
        let root = vapor_shared::paths::canonicalize(dir.path()).expect("canonical");
        fs::create_dir(root.join("Photos")).expect("dir");
        fs::write(root.join("Photos/a.jpg"), b"x").expect("seed");
        assert_eq!(colliding_local_path(&root.join("photos/a.jpg")), None);
    }

    #[test]
    fn a_symlinked_parent_does_not_read_as_another_spelling() {
        let dir = TempDir::new().expect("dir");
        let root = vapor_shared::paths::canonicalize(dir.path()).expect("canonical");
        fs::create_dir(root.join("real")).expect("dir");
        fs::write(root.join("real/a.txt"), b"x").expect("seed");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("real"), root.join("link")).expect("symlink");
            assert_eq!(
                local_spelling(&root.join("link/a.txt")),
                LocalSpelling::Exact
            );
        }
    }

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
        let root = vapor_shared::paths::canonicalize(dir.path()).expect("canonical");
        let composed = root.join("caf\u{e9}.txt");
        let decomposed = root.join("cafe\u{301}.txt");
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
        let root = vapor_shared::paths::canonicalize(dir.path()).expect("canonical");
        let existing = root.join("Readme.md");
        fs::write(&existing, b"x").expect("seed");
        let target = root.join("readme.md");
        let expected = if fs::symlink_metadata(&target).is_ok() {
            Some(existing)
        } else {
            None
        };
        assert_eq!(colliding_local_path(&target), expected);
    }
}
