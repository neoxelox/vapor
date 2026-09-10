//! Windows `TrashBin` stub: refuses with `Unsupported` so the daemon keeps
//! what it removes in its managed trash. The Recycle Bin lands
//! with the Windows surface.

use std::io;
use std::path::{Path, PathBuf};

use super::TrashBin;

#[derive(Debug, Default)]
pub struct NativeTrashBin;

impl NativeTrashBin {
    pub fn for_current_host() -> Self {
        Self
    }
}

impl TrashBin for NativeTrashBin {
    fn trash(&self, _path: &Path) -> io::Result<PathBuf> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the user trash is not wired on this OS yet",
        ))
    }
}
