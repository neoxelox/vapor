//! Throwaway disk images (macOS `hdiutil`). A scenario or a soak phase
//! mounts one to get a filesystem with different rules from the
//! sandbox's own: case-sensitive APFS for collision tests, a tiny
//! volume for disk-full tests, exFAT for no-xattr and coarse-mtime
//! tests. The image is detached and deleted on drop.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Failure;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageFs {
    /// Case-sensitive APFS.
    ApfsCaseSensitive,
    /// Default (case-insensitive) APFS.
    Apfs,
    /// exFAT: no extended attributes, 2 s mtime granularity.
    ExFat,
}

impl ImageFs {
    fn hdiutil_name(self) -> &'static str {
        match self {
            ImageFs::ApfsCaseSensitive => "Case-sensitive APFS",
            ImageFs::Apfs => "APFS",
            ImageFs::ExFat => "ExFAT",
        }
    }
}

#[derive(Debug)]
pub struct DiskImage {
    pub image_path: PathBuf,
    pub mount_point: PathBuf,
    attached: bool,
}

impl DiskImage {
    /// Whether this host can create and attach disk images.
    pub fn available() -> bool {
        cfg!(target_os = "macos")
            && Command::new("hdiutil")
                .arg("info")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
    }

    /// Creates a `size_mb` image under `directory` and mounts it at
    /// `<directory>/<name>-mnt`, hidden from Finder.
    pub fn create(
        directory: &Path,
        name: &str,
        size_mb: u32,
        fs: ImageFs,
    ) -> Result<Self, Failure> {
        let image_path = directory.join(format!("{name}.dmg"));
        let mount_point = directory.join(format!("{name}-mnt"));
        std::fs::create_dir_all(&mount_point)?;
        let create = Command::new("hdiutil")
            .args([
                "create",
                "-quiet",
                "-size",
                &format!("{size_mb}m"),
                "-fs",
                fs.hdiutil_name(),
                "-volname",
                name,
            ])
            .arg(&image_path)
            .output()?;
        if !create.status.success() {
            return Err(Failure::new(format!(
                "hdiutil create failed: {}",
                String::from_utf8_lossy(&create.stderr)
            )));
        }
        let attach = Command::new("hdiutil")
            .args([
                "attach",
                "-quiet",
                "-nobrowse",
                "-noautoopen",
                "-mountpoint",
            ])
            .arg(&mount_point)
            .arg(&image_path)
            .output()?;
        if !attach.status.success() {
            let _ = std::fs::remove_file(&image_path);
            return Err(Failure::new(format!(
                "hdiutil attach failed: {}",
                String::from_utf8_lossy(&attach.stderr)
            )));
        }
        Ok(Self {
            image_path,
            mount_point,
            attached: true,
        })
    }

    pub fn detach(&mut self) {
        if !self.attached {
            return;
        }
        // A daemon may still hold a file open for a moment; retry.
        for _ in 0..10 {
            let ok = Command::new("hdiutil")
                .args(["detach", "-quiet"])
                .arg(&self.mount_point)
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if ok {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let _ = Command::new("hdiutil")
            .args(["detach", "-quiet", "-force"])
            .arg(&self.mount_point)
            .status();
        self.attached = false;
        let _ = std::fs::remove_dir(&self.mount_point);
        let _ = std::fs::remove_file(&self.image_path);
    }
}

impl Drop for DiskImage {
    fn drop(&mut self) {
        self.detach();
    }
}
