//! macOS `TrashBin`: the user's `~/.Trash`, which the Finder shows as
//! the Trash. The move is a rename, so it only works on the home
//! volume; a file on another volume fails with `CrossesDevices` and the
//! caller falls back to the managed trash rather than copying user data
//! across volumes behind the user's back.

use std::io;
use std::path::{Path, PathBuf};

use super::{TrashBin, free_name_in};

#[derive(Debug, Default)]
pub struct NativeTrashBin;

impl NativeTrashBin {
    pub fn for_current_host() -> Self {
        Self
    }

    fn bin(&self) -> io::Result<PathBuf> {
        let home = vapor_shared::runtime_paths::home_directory().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "no home directory for this user",
            )
        })?;
        let bin = home.join(".Trash");
        if !bin.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{} is not a directory", bin.display()),
            ));
        }
        Ok(bin)
    }
}

impl TrashBin for NativeTrashBin {
    fn trash(&self, path: &Path) -> io::Result<PathBuf> {
        let bin = self.bin()?;
        let destination = free_name_in(&bin, path);
        std::fs::rename(path, &destination)?;
        Ok(destination)
    }
}
