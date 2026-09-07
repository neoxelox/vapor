//! Linux `TrashBin`: the freedesktop.org trash. The home trash is
//! `$XDG_DATA_HOME/Trash` (default `~/.local/share/Trash`), with the
//! payload under `files/` and a `.trashinfo` under `info/` naming the
//! original path and the deletion time, which is what desktop trash
//! views read. A file on another volume goes to that volume's
//! `.Trash-<uid>` at its mount point instead, since the spec forbids
//! copying across volumes behind the user's back; a volume without one
//! is refused and the caller keeps the item in the managed trash.
#![allow(unsafe_code)]

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{TrashBin, free_name_in};

#[derive(Debug, Default)]
pub struct NativeTrashBin;

impl NativeTrashBin {
    pub fn for_current_host() -> Self {
        Self
    }

    fn home_trash(&self) -> io::Result<PathBuf> {
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
            return Ok(PathBuf::from(data_home).join("Trash"));
        }
        let home = vapor_shared::runtime_paths::home_directory().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "no home directory for this user",
            )
        })?;
        Ok(home.join(".local/share/Trash"))
    }

    /// The trash directory for `path`: the home trash when the file is
    /// on the home volume, else the volume's own `.Trash-<uid>`.
    fn trash_for(&self, path: &Path) -> io::Result<PathBuf> {
        let home_trash = self.home_trash()?;
        let file_device = std::fs::symlink_metadata(path)?.dev();
        // The trash directory may not exist yet; the volume it would
        // land on is the nearest ancestor that does.
        let home_device = home_trash
            .ancestors()
            .find_map(|ancestor| std::fs::metadata(ancestor).ok())
            .map(|metadata| metadata.dev());
        if home_device == Some(file_device) {
            return Ok(home_trash);
        }
        let top = mount_point_of(path, file_device)?;
        // SAFETY: `getuid` has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let volume_trash = top.join(format!(".Trash-{uid}"));
        Ok(volume_trash)
    }
}

/// The highest ancestor of `path` on the same device: the mount point.
fn mount_point_of(path: &Path, device: u64) -> io::Result<PathBuf> {
    let mut current = path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "path has no parent"))?;
    loop {
        let Some(parent) = current.parent() else {
            return Ok(current);
        };
        let parent_device = std::fs::metadata(parent)?.dev();
        if parent_device != device {
            return Ok(current);
        }
        current = parent.to_path_buf();
    }
}

/// `Path=` in a `.trashinfo` is the original path, percent-encoded
/// the way a URL is, byte by byte.
fn encode_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for byte in path.as_os_str().as_bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~');
        if keep {
            out.push(char::from(*byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `DeletionDate=` is local time in `YYYY-MM-DDThh:mm:ss`; the spec
/// wants local time, and without a timezone database at hand UTC is
/// the honest reading every reader accepts.
fn deletion_date(now: SystemTime) -> String {
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let remaining = secs % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        remaining / 3600,
        (remaining % 3600) / 60,
        remaining % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

impl TrashBin for NativeTrashBin {
    fn trash(&self, path: &Path) -> io::Result<PathBuf> {
        let trash = self.trash_for(path)?;
        let files = trash.join("files");
        let info = trash.join("info");
        std::fs::create_dir_all(&files)?;
        std::fs::create_dir_all(&info)?;
        let destination = free_name_in(&files, path);
        let name = destination
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "item".to_string());
        let info_path = info.join(format!("{name}.trashinfo"));
        let original = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let record = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            encode_path(&original),
            deletion_date(SystemTime::now())
        );
        // The info file first, so a crash between the two leaves a
        // record without a payload rather than a payload nothing lists.
        std::fs::write(&info_path, record)?;
        if let Err(error) = std::fs::rename(path, &destination) {
            let _ = std::fs::remove_file(&info_path);
            return Err(error);
        }
        Ok(destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_paths_render_the_way_the_spec_reads_them() {
        assert_eq!(
            deletion_date(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)),
            "2023-11-14T22:13:20"
        );
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(
            encode_path(Path::new("/home/alex/My Docs/a&b.txt")),
            "/home/alex/My%20Docs/a%26b.txt"
        );
    }

    #[test]
    fn the_home_trash_gets_the_payload_and_a_trashinfo() {
        let temp = tempfile::TempDir::new().expect("temp");
        let data_home = temp.path().join("xdg");
        // Point the bin at a throwaway XDG_DATA_HOME on this volume.
        let bin = NativeTrashBin;
        let home_trash = {
            let _guard = EnvGuard::set("XDG_DATA_HOME", &data_home);
            bin.home_trash().expect("home trash")
        };
        assert_eq!(home_trash, data_home.join("Trash"));
        let file = temp.path().join("gone.txt");
        std::fs::write(&file, b"payload").expect("seed");
        let landed = {
            let _guard = EnvGuard::set("XDG_DATA_HOME", &data_home);
            bin.trash(&file).expect("trash")
        };
        assert!(!file.exists());
        assert_eq!(landed, data_home.join("Trash/files/gone.txt"));
        let info = std::fs::read_to_string(data_home.join("Trash/info/gone.txt.trashinfo"))
            .expect("trashinfo");
        assert!(info.starts_with("[Trash Info]\n"));
        assert!(
            info.contains(&format!("Path={}", encode_path(&file))),
            "{info}"
        );
        assert!(info.contains("DeletionDate="));
    }

    struct EnvGuard(&'static str, Option<std::ffi::OsString>);

    impl EnvGuard {
        fn set(name: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(name);
            // SAFETY: tests in this module run one at a time on the
            // variable they set and restore it before returning.
            unsafe { std::env::set_var(name, value) };
            Self(name, previous)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: as above.
            unsafe {
                match &self.1 {
                    Some(value) => std::env::set_var(self.0, value),
                    None => std::env::remove_var(self.0),
                }
            }
        }
    }
}
