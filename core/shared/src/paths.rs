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
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
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
