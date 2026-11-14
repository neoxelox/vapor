//! Filesystem path helpers shared by every crate that canonicalizes a
//! path a user or another component can see.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf, Prefix};

/// `fs::canonicalize` that returns the plain Win32 spelling on Windows.
///
/// `fs::canonicalize` on Windows yields verbatim paths (`\\?\C:\…`,
/// `\\?\UNC\server\share\…`). Those spellings reach logs, status output,
/// the IPC socket location hash, and path comparisons between components,
/// and several Win32 consumers reject them. Every canonicalization of a
/// visible path goes through here so the whole runtime agrees on one
/// spelling. Identical to `fs::canonicalize` on every other OS.
#[allow(clippy::disallowed_methods)] // the one sanctioned call site
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).map(strip_verbatim_prefix)
}

/// Rewrites a Windows verbatim path (`\\?\C:\…`, `\\?\UNC\server\share\…`)
/// into its plain form. No-op for every other path and on every other OS.
pub fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path;
    };
    let mut simplified = match prefix.kind() {
        Prefix::VerbatimDisk(letter) => {
            let letter = letter as char;
            PathBuf::from(format!("{letter}:\\"))
        }
        Prefix::VerbatimUNC(server, share) => {
            let mut root = OsString::from(r"\\");
            root.push(server);
            root.push(r"\");
            root.push(share);
            root.push(r"\");
            PathBuf::from(root)
        }
        _ => return path,
    };
    for component in components {
        if !matches!(component, Component::RootDir) {
            simplified.push(component.as_os_str());
        }
    }
    simplified
}

/// How a filesystem spells the entry that a path resolves to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoredSpelling {
    /// Nothing is there under any spelling.
    Absent,
    /// The entry is there, spelled exactly as the path spells it.
    Exact,
    /// The filesystem resolves the path onto an entry it stores under
    /// another spelling (a different case, or a different Unicode
    /// normalization): the path as stored.
    Other(PathBuf),
}

/// Asks the filesystem for the stored spelling of `path`. A name is
/// one entry on a volume that folds it, so `report.txt` answers for
/// `Report.txt` there. Sync state is keyed by the exact spelling and
/// has to tell the two apart. A symlink at `path` counts as exact: it
/// is outside the sync contract and is never resolved.
pub fn stored_spelling(path: &Path) -> StoredSpelling {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return StoredSpelling::Absent;
    };
    if metadata.file_type().is_symlink() {
        return StoredSpelling::Exact;
    }
    let Ok(stored) = canonicalize(path) else {
        return StoredSpelling::Exact;
    };
    // Compare from the last component upward for as long as the two
    // paths name the same thing. A parent that differs in more than
    // spelling is a symlink the canonical form resolved (`/tmp`), and
    // says nothing about the entry.
    let mut differs = false;
    for (wanted, found) in path.components().rev().zip(stored.components().rev()) {
        if wanted == found {
            continue;
        }
        match (wanted.as_os_str().to_str(), found.as_os_str().to_str()) {
            (Some(wanted), Some(found)) if fold_name(wanted) == fold_name(found) => differs = true,
            _ => break,
        }
    }
    if differs {
        StoredSpelling::Other(stored)
    } else {
        StoredSpelling::Exact
    }
}

/// The key that two spellings of one name share: composed (NFC) and
/// case-folded, so `Readme.md` and `readme.md` fold together, and so
/// do the precomposed and the decomposed spelling of `café`.
pub fn fold_name(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    name.nfc().flat_map(char::to_lowercase).collect()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefixes_are_simplified_to_plain_win32_paths() {
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(r"\\?\C:\Users\vapor\.vapor")),
            PathBuf::from(r"C:\Users\vapor\.vapor")
        );
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(r"\\?\UNC\server\share\vapor")),
            PathBuf::from(r"\\server\share\vapor")
        );
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(r"C:\plain")),
            PathBuf::from(r"C:\plain")
        );
    }

    #[test]
    fn canonicalize_never_returns_a_verbatim_path() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let canonical = canonicalize(temp.path()).expect("canonicalize");
        assert!(canonical.is_absolute());
        assert!(
            !canonical.to_string_lossy().starts_with(r"\\?\"),
            "{canonical:?}"
        );
    }
}
