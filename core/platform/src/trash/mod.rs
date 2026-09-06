//! The user's own trash: where a file goes when the user would expect
//! to find it in the Finder's Trash, the Recycle Bin, or the desktop's
//! trash folder.
//!
//! See `docs/architecture/platform-abstractions.md` §`TrashBin`. The
//! daemon's managed trash (`core/daemon/src/trash.rs`) is the safety
//! net that always works; this trait is the opt-in discoverable
//! alternative. macOS moves into `~/.Trash` (`macos.rs`). Linux and
//! Windows are not shipping surfaces yet; their `NativeTrashBin` refuses
//! with `Unsupported`, and the daemon falls back to the managed trash.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::NativeTrashBin;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::NativeTrashBin;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::NativeTrashBin;

pub trait TrashBin: Send + Sync {
    /// Moves `path` (a file or a directory) into the user's trash and
    /// returns where it landed. Fails with `Unsupported` where no user
    /// trash exists on this host, and with the underlying error when
    /// the move itself fails (a different volume, a permission), so the
    /// caller can fall back to its own trash.
    fn trash(&self, path: &Path) -> io::Result<PathBuf>;
}

/// Picks a free name in `bin` for `path`'s file name, numbering
/// duplicates the way the Finder does (`name 2.txt`, `name 3.txt`).
pub fn free_name_in(bin: &Path, path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "item".to_string());
    let candidate = bin.join(&file_name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, extension) = match file_name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_string(), format!(".{extension}")),
        _ => (file_name.clone(), String::new()),
    };
    (2..)
        .map(|n| bin.join(format!("{stem} {n}{extension}")))
        .find(|candidate| !candidate.exists())
        .expect("an unbounded range always yields a free name")
}

/// Test fake: a trash directory the test owns. Records every move.
#[derive(Debug)]
pub struct InMemoryTrashBin {
    bin: PathBuf,
    moved: Mutex<Vec<(PathBuf, PathBuf)>>,
    refuse: bool,
}

impl InMemoryTrashBin {
    pub fn new(bin: PathBuf) -> Self {
        Self {
            bin,
            moved: Mutex::new(Vec::new()),
            refuse: false,
        }
    }

    /// A bin that behaves like a host without a user trash.
    pub fn unsupported() -> Self {
        Self {
            bin: PathBuf::new(),
            moved: Mutex::new(Vec::new()),
            refuse: true,
        }
    }

    pub fn moved(&self) -> Vec<(PathBuf, PathBuf)> {
        self.moved
            .lock()
            .expect("InMemoryTrashBin mutex poisoned")
            .clone()
    }
}

impl TrashBin for InMemoryTrashBin {
    fn trash(&self, path: &Path) -> io::Result<PathBuf> {
        if self.refuse {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this host has no user trash",
            ));
        }
        std::fs::create_dir_all(&self.bin)?;
        let destination = free_name_in(&self.bin, path);
        std::fs::rename(path, &destination)?;
        self.moved
            .lock()
            .expect("InMemoryTrashBin mutex poisoned")
            .push((path.to_path_buf(), destination.clone()));
        Ok(destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_name_numbers_duplicates_like_the_finder() {
        let temp = tempfile::TempDir::new().expect("temp");
        let bin = temp.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let source = temp.path().join("report.pdf");
        assert_eq!(free_name_in(&bin, &source), bin.join("report.pdf"));
        std::fs::write(bin.join("report.pdf"), b"x").expect("occupy");
        assert_eq!(free_name_in(&bin, &source), bin.join("report 2.pdf"));
        std::fs::write(bin.join("report 2.pdf"), b"x").expect("occupy");
        assert_eq!(free_name_in(&bin, &source), bin.join("report 3.pdf"));
        let dotfile = temp.path().join(".env");
        std::fs::write(bin.join(".env"), b"x").expect("occupy");
        assert_eq!(free_name_in(&bin, &dotfile), bin.join(".env 2"));
    }

    #[test]
    fn fake_bin_moves_files_and_directories_and_can_refuse() {
        let temp = tempfile::TempDir::new().expect("temp");
        let bin = InMemoryTrashBin::new(temp.path().join("bin"));
        let file = temp.path().join("a.txt");
        std::fs::write(&file, b"a").expect("file");
        let dir = temp.path().join("d");
        std::fs::create_dir_all(dir.join("inner")).expect("dir");
        let landed_file = bin.trash(&file).expect("trash file");
        let landed_dir = bin.trash(&dir).expect("trash dir");
        assert!(!file.exists() && landed_file.is_file());
        assert!(!dir.exists() && landed_dir.join("inner").is_dir());
        assert_eq!(bin.moved().len(), 2);

        let refusing = InMemoryTrashBin::unsupported();
        let other = temp.path().join("b.txt");
        std::fs::write(&other, b"b").expect("file");
        let error = refusing.trash(&other).expect_err("refuses");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(other.exists(), "a refusal leaves the file where it was");
    }
}
