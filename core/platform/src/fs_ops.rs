//! Filesystem operations with per-OS optimal implementations.

use std::io;
use std::path::{Path, PathBuf};

/// Atomically renames `from` onto `to`, failing with
/// [`io::ErrorKind::AlreadyExists`] when `to` already exists — an
/// atomic no-clobber commit.
///
/// This must be a *rename*, not a hard-link + unlink pair, wherever the
/// OS allows: macOS FSEvents tracks file-level events by node, and a
/// file whose name was created via `link(2)` stays bound to the
/// original (deleted) temp name — every later external edit or
/// deletion of the visible name is then silently missing from any
/// watcher, which broke the filesystem provider's changes feed for
/// uploaded files (observed live: a cloud-side `rm` of an uploaded
/// file produced zero FSEvents deliveries). A real rename keeps the
/// node's event tracking bound to the destination path.
pub fn atomic_noclobber_rename(from: &Path, to: &Path) -> io::Result<()> {
    atomic_noclobber_rename_impl(from, to)
}

/// `renamex_np(RENAME_EXCL)`: atomic no-clobber rename, native since
/// macOS 10.12 on APFS/HFS+.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn atomic_noclobber_rename_impl(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from_c = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let to_c = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    // SAFETY: both pointers reference NUL-terminated buffers that live
    // for the duration of the call; `renamex_np` reads them and has no
    // other memory effects.
    let rc = unsafe { libc::renamex_np(from_c.as_ptr(), to_c.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `renameat2(RENAME_NOREPLACE)`: atomic no-clobber rename on Linux.
/// Filesystems / kernels without support report `EINVAL` / `ENOSYS`,
/// where the link-based fallback still provides the no-clobber
/// guarantee (inotify does not share FSEvents' node-bound tracking, so
/// the fallback is watch-safe there).
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn atomic_noclobber_rename_impl(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from_c = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let to_c = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    // SAFETY: both pointers reference NUL-terminated buffers that live
    // for the duration of the call; `renameat2` reads them and has no
    // other memory effects.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from_c.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EINVAL) | Some(libc::ENOSYS) => link_based_noclobber(from, to),
        _ => Err(error),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn atomic_noclobber_rename_impl(from: &Path, to: &Path) -> io::Result<()> {
    link_based_noclobber(from, to)
}

/// Portable fallback: `link` + `unlink` is atomic no-clobber everywhere
/// hard links exist, at the cost of node-bound watcher tracking (see
/// the module docs) — acceptable only where no atomic-rename API is
/// available.
#[cfg(not(target_os = "macos"))]
fn link_based_noclobber(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::hard_link(from, to)?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_atomically_when_the_target_is_absent() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let from = dir.path().join("staged");
        let to = dir.path().join("final");
        std::fs::write(&from, b"payload").expect("stage");

        atomic_noclobber_rename(&from, &to).expect("rename");

        assert_eq!(std::fs::read(&to).expect("target"), b"payload");
        assert!(!from.exists(), "the staged name must be consumed");
    }

    #[test]
    fn refuses_to_clobber_an_existing_target() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let from = dir.path().join("staged");
        let to = dir.path().join("final");
        std::fs::write(&from, b"new").expect("stage");
        std::fs::write(&to, b"existing").expect("target");

        let error = atomic_noclobber_rename(&from, &to).expect_err("must not clobber");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&to).expect("target intact"),
            b"existing",
            "the existing target must survive untouched"
        );
        assert!(
            from.exists(),
            "the staged file stays for the caller to clean up"
        );
    }
}

/// The volume `path` lives on, as an identity two paths can be compared
/// by: the device number on Unix, the drive or share prefix on Windows.
/// A path that does not exist yet is judged by its nearest existing
/// ancestor, which is the volume it would be created on.
pub fn volume_of(path: &Path) -> io::Result<VolumeId> {
    let existing = path
        .ancestors()
        .find(|ancestor| std::fs::symlink_metadata(ancestor).is_ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no ancestor of {} exists", path.display()),
            )
        })?;
    volume_of_existing(existing)
}

/// Whether `a` and `b` are on the same volume, so a rename between them
/// is a metadata operation rather than a copy.
pub fn same_volume(a: &Path, b: &Path) -> io::Result<bool> {
    Ok(volume_of(a)? == volume_of(b)?)
}

/// The top of the volume `path` lives on: the mount point on Unix, the
/// drive or share root on Windows.
pub fn volume_root_of(path: &Path) -> io::Result<PathBuf> {
    let volume = volume_of(path)?;
    let mut current = path
        .ancestors()
        .find(|ancestor| std::fs::symlink_metadata(ancestor).is_ok())
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "path has no existing ancestor"))?;
    loop {
        let Some(parent) = current.parent() else {
            return Ok(current);
        };
        if volume_of_existing(parent)? != volume {
            return Ok(current);
        }
        current = parent.to_path_buf();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeId(VolumeIdInner);

#[derive(Clone, Debug, PartialEq, Eq)]
enum VolumeIdInner {
    #[cfg(unix)]
    Device(u64),
    #[cfg(windows)]
    Prefix(std::ffi::OsString),
}

#[cfg(unix)]
fn volume_of_existing(path: &Path) -> io::Result<VolumeId> {
    use std::os::unix::fs::MetadataExt;
    Ok(VolumeId(VolumeIdInner::Device(
        std::fs::symlink_metadata(path)?.dev(),
    )))
}

#[cfg(windows)]
fn volume_of_existing(path: &Path) -> io::Result<VolumeId> {
    use std::path::Component;
    let canonical = vapor_shared::paths::canonicalize(path)?;
    let prefix = match canonical.components().next() {
        Some(Component::Prefix(prefix)) => prefix.as_os_str().to_os_string(),
        _ => std::ffi::OsString::new(),
    };
    Ok(VolumeId(VolumeIdInner::Prefix(prefix)))
}

#[cfg(test)]
mod volume_tests {
    use super::*;

    #[test]
    fn a_path_and_its_parent_share_a_volume_and_a_missing_child_follows_its_parent() {
        let temp = tempfile::TempDir::new().expect("temp");
        let file = temp.path().join("a.txt");
        std::fs::write(&file, b"a").expect("write");
        assert!(same_volume(&file, temp.path()).expect("same"));
        let missing = temp.path().join("not/yet/here");
        assert_eq!(
            volume_of(&missing).expect("judged by the ancestor"),
            volume_of(temp.path()).expect("volume")
        );
        let root = volume_root_of(&file).expect("root");
        assert!(temp.path().starts_with(&root), "{root:?}");
        assert_eq!(volume_root_of(&root).expect("idempotent"), root);
    }
}
