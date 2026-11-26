//! Filesystem operations with per-OS optimal implementations.

use std::io;
use std::path::{Path, PathBuf};

/// Atomically renames `from` onto `to`, failing with
/// [`io::ErrorKind::AlreadyExists`] when `to` already exists: an
/// atomic no-clobber commit. `from` is a file or a folder; a folder
/// goes with everything in it.
///
/// This must be a *rename*, not a hard-link + unlink pair, wherever the
/// OS allows: macOS FSEvents tracks file-level events by node, and a
/// file whose name was created via `link(2)` stays bound to the
/// original (deleted) temp name: every later external edit or
/// deletion of the visible name is then silently missing from any
/// watcher, which broke the filesystem provider's changes feed for
/// uploaded files (observed live: a cloud-side `rm` of an uploaded
/// file produced zero FSEvents deliveries). A real rename keeps the
/// node's event tracking bound to the destination path.
pub fn atomic_noclobber_rename(from: &Path, to: &Path) -> io::Result<()> {
    // A folder cannot go into itself. Refused here, before a fallback
    // that claims the new name could make a folder inside the old one.
    if to != from && to.starts_with(from) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the new path is inside the old one",
        ));
    }
    atomic_noclobber_rename_impl(from, to)
}

/// Atomically exchanges the files at `a` and `b`: after the call each
/// name holds what the other held, and no moment exists at which
/// either name is empty. A writer that replaces a file this way has
/// the file it replaced in its hands, so nothing another writer put
/// there in the meantime is lost.
///
/// Fails with [`io::ErrorKind::Unsupported`] where the host or the
/// volume has no such exchange; the caller then decides what a plain
/// rename costs.
pub fn atomic_swap(a: &Path, b: &Path) -> io::Result<()> {
    atomic_swap_impl(a, b)
}

/// `renamex_np(RENAME_SWAP)`, native on APFS and HFS+.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn atomic_swap_impl(a: &Path, b: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let a_c = CString::new(a.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let b_c = CString::new(b.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    // SAFETY: both pointers reference NUL-terminated buffers that live
    // for the duration of the call; `renamex_np` reads them and has no
    // other memory effects.
    let rc = unsafe { libc::renamex_np(a_c.as_ptr(), b_c.as_ptr(), libc::RENAME_SWAP) };
    if rc == 0 {
        return Ok(());
    }
    Err(swap_error(io::Error::last_os_error()))
}

/// `renameat2(RENAME_EXCHANGE)`.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn atomic_swap_impl(a: &Path, b: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let a_c = CString::new(a.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let b_c = CString::new(b.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    // SAFETY: both pointers reference NUL-terminated buffers that live
    // for the duration of the call; `renameat2` reads them and has no
    // other memory effects.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a_c.as_ptr(),
            libc::AT_FDCWD,
            b_c.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if rc == 0 {
        return Ok(());
    }
    Err(swap_error(io::Error::last_os_error()))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn atomic_swap_impl(_a: &Path, _b: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this host has no atomic exchange of two files",
    ))
}

/// A volume or a kernel with no exchange says so in more than one
/// way; all of them are "not here".
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn swap_error(error: io::Error) -> io::Error {
    match error.raw_os_error() {
        Some(libc::ENOTSUP) | Some(libc::EINVAL) | Some(libc::ENOSYS) => {
            io::Error::new(io::ErrorKind::Unsupported, error)
        }
        _ => error,
    }
}

/// Makes the content of `file` durable: after the call it survives a
/// loss of power. Returns when it is, like [`std::fs::File::sync_all`].
///
/// On macOS that takes two steps, and the second is the costly one:
/// `fsync` moves the file to the drive, and `F_FULLFSYNC` makes the
/// drive write what it holds. The second step is for the whole drive,
/// not for one file. So callers that arrive together share it: each
/// moves its own file to the drive, and one flush of the drive that
/// starts after that covers them all. A burst of small files then pays
/// one flush for a group of files and not one flush for each file,
/// with the same guarantee for every one of them.
pub fn make_durable(file: &std::fs::File) -> io::Result<()> {
    make_durable_impl(file)
}

#[cfg(not(target_os = "macos"))]
fn make_durable_impl(file: &std::fs::File) -> io::Result<()> {
    file.sync_all()
}

#[cfg(target_os = "macos")]
fn make_durable_impl(file: &std::fs::File) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    sync_to_drive(file)?;
    let device = file.metadata()?.dev();
    drive_flushes(device).flushed_after_now(|| file.sync_all())
}

/// `fsync(2)`: the file's data and attributes go to the drive. The
/// drive may still hold them in its cache.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn sync_to_drive(file: &std::fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: the descriptor is open for the life of `file`, and
    // `fsync` has no memory effects.
    let rc = unsafe { libc::fsync(file.as_raw_fd()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The flush group of one drive, by the device number of its volume.
#[cfg(target_os = "macos")]
fn drive_flushes(device: u64) -> std::sync::Arc<FlushGroup> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static GROUPS: OnceLock<Mutex<HashMap<u64, Arc<FlushGroup>>>> = OnceLock::new();
    GROUPS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(device)
        .or_default()
        .clone()
}

/// Callers that each need one flush that starts after they asked, and
/// that share the flushes: one caller at a time runs a flush, and every
/// caller that asked before it started is done when it ends.
#[derive(Default)]
struct FlushGroup {
    state: std::sync::Mutex<FlushState>,
    ended: std::sync::Condvar,
}

#[derive(Default)]
struct FlushState {
    /// How many flushes have started.
    started: u64,
    /// The number of the last flush that ended well.
    done: u64,
    running: bool,
    /// Callers that wait for a flush another caller runs.
    waiting: usize,
}

impl FlushGroup {
    /// Returns when a flush that started after this call has ended
    /// well. `flush` runs it when no other caller does; a flush that
    /// fails is this caller's error, and the callers that waited on it
    /// run their own.
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    fn flushed_after_now(&self, flush: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A flush that runs now started before this call: the next one
        // is the first that counts.
        let needed = state.started + 1;
        loop {
            if state.done >= needed {
                return Ok(());
            }
            if !state.running {
                state.running = true;
                state.started += 1;
                let number = state.started;
                drop(state);
                let result = flush();
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.running = false;
                if result.is_ok() {
                    state.done = number;
                }
                drop(state);
                self.ended.notify_all();
                return result;
            }
            state.waiting += 1;
            state = self
                .ended
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.waiting -= 1;
        }
    }
}

/// Runs `action` while the owner may write `path`, and puts the mode
/// back as it was. A file that is read-only for its owner refuses an
/// extended attribute; the caller that has to tag such a file does it
/// in here. A path that is not a regular file, a file the owner can
/// write already, and a host with no POSIX modes run `action` as is.
pub fn with_owner_write<T>(path: &Path, action: impl FnOnce() -> T) -> T {
    with_owner_write_impl(path, action)
}

#[cfg(unix)]
fn with_owner_write_impl<T>(path: &Path, action: impl FnOnce() -> T) -> T {
    use std::os::unix::fs::PermissionsExt;

    const OWNER_WRITE: u32 = 0o200;
    let mode = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata.permissions().mode(),
        _ => return action(),
    };
    if mode & OWNER_WRITE != 0
        || std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | OWNER_WRITE))
            .is_err()
    {
        return action();
    }
    let result = action();
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    result
}

#[cfg(not(unix))]
fn with_owner_write_impl<T>(_path: &Path, action: impl FnOnce() -> T) -> T {
    action()
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
/// the module docs): acceptable only where no atomic-rename API is
/// available.
#[cfg(not(target_os = "macos"))]
fn link_based_noclobber(from: &Path, to: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(from)?.is_dir() {
        return folder_noclobber(from, to);
    }
    std::fs::hard_link(from, to)?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

/// A folder has no hard link. The name is claimed with an empty
/// folder, which fails when the name is taken, and the rename then
/// takes the place of that empty folder. A rename that fails gives the
/// claim up.
#[cfg(not(any(target_os = "macos", windows)))]
fn folder_noclobber(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir(to)?;
    std::fs::rename(from, to).inspect_err(|_| {
        let _ = std::fs::remove_dir(to);
    })
}

/// On Windows a rename does not take the place of a folder. The claim
/// shows that the name is free and is given up just before the rename.
/// Nothing at the name is this function's after that, so a rename that
/// fails leaves the name as it is: what is there belongs to whoever
/// took the name in that instant.
#[cfg(windows)]
fn folder_noclobber(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir(to)?;
    std::fs::remove_dir(to)?;
    std::fs::rename(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_renamed_with_what_it_holds_and_never_onto_a_taken_name() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let from = dir.path().join("photos");
        std::fs::create_dir_all(from.join("2026")).expect("folder");
        std::fs::write(from.join("2026/a.jpg"), b"payload").expect("file");

        // A file and a folder each hold the name: nothing is replaced.
        for taken in ["a-file", "a-folder"] {
            let to = dir.path().join(taken);
            if taken == "a-file" {
                std::fs::write(&to, b"mine").expect("taken by a file");
            } else {
                std::fs::create_dir(&to).expect("taken by a folder");
                std::fs::write(to.join("kept.txt"), b"mine").expect("content");
            }
            atomic_noclobber_rename(&from, &to).expect_err("the name is taken");
            assert!(from.join("2026/a.jpg").exists(), "the folder stays");
        }
        assert_eq!(
            std::fs::read(dir.path().join("a-file")).expect("file"),
            b"mine"
        );
        assert!(dir.path().join("a-folder/kept.txt").exists());

        let to = dir.path().join("pictures");
        atomic_noclobber_rename(&from, &to).expect("rename");
        assert_eq!(
            std::fs::read(to.join("2026/a.jpg")).expect("moved file"),
            b"payload"
        );
        assert!(!from.exists(), "the old name is gone");
    }

    #[test]
    fn a_folder_does_not_go_into_itself_and_no_folder_is_made_for_the_try() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let from = dir.path().join("photos");
        std::fs::create_dir_all(&from).expect("folder");
        std::fs::write(from.join("a.jpg"), b"payload").expect("file");

        let error = atomic_noclobber_rename(&from, &from.join("inside/photos"))
            .expect_err("a folder cannot hold itself");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read_dir(&from)
                .expect("list")
                .flatten()
                .map(|entry| entry.file_name())
                .collect::<Vec<_>>(),
            ["a.jpg"],
            "nothing was made under the folder"
        );
    }

    /// The path a host takes when its atomic call is not there: the
    /// volume has no `RENAME_NOREPLACE` on Linux, and every folder on
    /// Windows.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_fallback_for_a_folder_moves_it_whole_and_refuses_a_taken_name() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let from = dir.path().join("photos");
        std::fs::create_dir_all(from.join("2026")).expect("folder");
        std::fs::write(from.join("2026/a.jpg"), b"payload").expect("file");

        for taken in ["a-file", "a-folder"] {
            let to = dir.path().join(taken);
            if taken == "a-file" {
                std::fs::write(&to, b"mine").expect("taken by a file");
            } else {
                std::fs::create_dir(&to).expect("taken by a folder");
                std::fs::write(to.join("kept.txt"), b"mine").expect("content");
            }
            folder_noclobber(&from, &to).expect_err("the name is taken");
            assert!(from.join("2026/a.jpg").exists(), "the folder stays");
        }
        assert_eq!(
            std::fs::read(dir.path().join("a-file")).expect("file"),
            b"mine"
        );
        assert!(dir.path().join("a-folder/kept.txt").exists());

        let to = dir.path().join("pictures");
        folder_noclobber(&from, &to).expect("rename");
        assert_eq!(
            std::fs::read(to.join("2026/a.jpg")).expect("moved file"),
            b"payload"
        );
        assert!(!from.exists(), "the old name is gone");
    }

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

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_swap_gives_each_name_the_file_of_the_other() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let staged = dir.path().join("staged");
        let target = dir.path().join("final");
        std::fs::write(&staged, b"new").expect("stage");
        std::fs::write(&target, b"old").expect("target");

        atomic_swap(&staged, &target).expect("swap");

        assert_eq!(std::fs::read(&target).expect("target"), b"new");
        assert_eq!(std::fs::read(&staged).expect("staged"), b"old");

        // Both names must exist: there is nothing to exchange with.
        std::fs::remove_file(&staged).expect("remove");
        let error = atomic_swap(&staged, &target).expect_err("one name is gone");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(std::fs::read(&target).expect("target intact"), b"new");
    }

    #[test]
    fn callers_that_arrive_during_a_flush_share_the_next_one() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier, mpsc};

        let group = Arc::new(FlushGroup::default());
        let flushes = Arc::new(AtomicUsize::new(0));
        let (first_started_tx, first_started_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel::<()>();

        // The first caller runs a flush and stays in it.
        let first = {
            let group = group.clone();
            let flushes = flushes.clone();
            std::thread::spawn(move || {
                group.flushed_after_now(|| {
                    flushes.fetch_add(1, Ordering::SeqCst);
                    first_started_tx.send(()).expect("signal");
                    release_first_rx.recv().expect("release");
                    Ok(())
                })
            })
        };
        first_started_rx.recv().expect("the first flush runs");

        // Eight more arrive while it runs. It started before they
        // asked, so it does not count for them: they need the next.
        const LATER: usize = 8;
        let ready = Arc::new(Barrier::new(LATER + 1));
        let later: Vec<_> = (0..LATER)
            .map(|_| {
                let group = group.clone();
                let flushes = flushes.clone();
                let ready = ready.clone();
                std::thread::spawn(move || {
                    ready.wait();
                    group.flushed_after_now(|| {
                        flushes.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                })
            })
            .collect();
        ready.wait();
        while group.state.lock().expect("state").waiting < LATER {
            std::thread::yield_now();
        }
        release_first_tx.send(()).expect("release the first");
        first.join().expect("first").expect("first flush");
        for caller in later {
            caller.join().expect("caller").expect("flushed");
        }

        assert_eq!(
            flushes.load(Ordering::SeqCst),
            2,
            "the flush that ran when they asked does not count for them, and one more covers all eight"
        );
    }

    #[test]
    fn a_flush_that_fails_is_the_error_of_its_caller_only() {
        let group = FlushGroup::default();
        let failed = group.flushed_after_now(|| Err(io::Error::other("the drive said no")));
        assert!(failed.is_err());
        // The failure counts for nobody: the next caller runs its own.
        let mut ran = false;
        group
            .flushed_after_now(|| {
                ran = true;
                Ok(())
            })
            .expect("flush");
        assert!(ran);
    }

    #[test]
    fn a_file_made_durable_holds_what_was_written() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("payload");
        let mut file = std::fs::File::create(&path).expect("create");
        std::io::Write::write_all(&mut file, b"payload").expect("write");
        make_durable(&file).expect("durable");
        assert_eq!(std::fs::read(&path).expect("read"), b"payload");
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
