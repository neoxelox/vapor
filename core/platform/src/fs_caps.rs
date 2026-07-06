//! Filesystem capabilities trait + per-OS native implementation.
//!
//! See `docs/architecture/platform-abstractions.md` §`FilesystemCapabilities`.
//! Wave 4 shipped the trait + an in-memory fake; the C8 wave adds the
//! metadata-tag API (xattr on macOS/Linux, ADS on Windows once Wave 12
//! lands) that op-id tagging and the self-write cache consume.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaseSensitivity {
    /// Filesystem distinguishes between `Foo.txt` and `foo.txt`.
    Sensitive,
    /// Filesystem treats both as the same path.
    Insensitive,
}

pub trait FilesystemCapabilities: Send + Sync {
    /// Whether the filesystem supports extended attributes (xattr on
    /// macOS / Linux, ADS on Windows). When `false`, callers fall back
    /// to side-files for op-id metadata.
    fn supports_xattr(&self) -> bool;

    /// Filesystem case-sensitivity default. Loop-prevention and
    /// conflict-path derivation must respect this.
    fn case_sensitivity(&self) -> CaseSensitivity;

    /// Reads a metadata tag from `path`. `Ok(None)` when the tag is
    /// absent; `Err` when the filesystem cannot answer (unsupported,
    /// permission, missing file).
    fn read_tag(&self, path: &Path, name: &str) -> io::Result<Option<String>>;

    /// Writes a metadata tag onto `path`. Callers treat
    /// `Unsupported` / `PermissionDenied` / read-only errors as the
    /// signal to fall back to a side-file (`data-flow.md §Loop
    /// prevention`).
    fn write_tag(&self, path: &Path, name: &str, value: &str) -> io::Result<()>;

    /// Removes a metadata tag; removing an absent tag is `Ok`.
    fn remove_tag(&self, path: &Path, name: &str) -> io::Result<()>;
}

/// In-memory `FilesystemCapabilities` for tests. Defaults: xattr
/// supported, case-sensitive — matches the typical Linux ext4 / xfs
/// default. Tests override either toggle to assert per-OS behavior;
/// flipping `supports_xattr` off makes every tag write fail with
/// `Unsupported` so side-file fallback paths are exercisable.
#[derive(Debug)]
pub struct InMemoryFilesystemCapabilities {
    inner: Mutex<InMemoryFilesystemCapabilitiesInner>,
}

#[derive(Debug)]
struct InMemoryFilesystemCapabilitiesInner {
    supports_xattr: bool,
    case_sensitivity: CaseSensitivity,
    tags: BTreeMap<(PathBuf, String), String>,
}

impl Default for InMemoryFilesystemCapabilities {
    fn default() -> Self {
        Self::new(true, CaseSensitivity::Sensitive)
    }
}

impl InMemoryFilesystemCapabilities {
    pub fn new(supports_xattr: bool, case_sensitivity: CaseSensitivity) -> Self {
        Self {
            inner: Mutex::new(InMemoryFilesystemCapabilitiesInner {
                supports_xattr,
                case_sensitivity,
                tags: BTreeMap::new(),
            }),
        }
    }

    pub fn set_supports_xattr(&self, supports_xattr: bool) {
        self.lock().supports_xattr = supports_xattr;
    }

    pub fn set_case_sensitivity(&self, case_sensitivity: CaseSensitivity) {
        self.lock().case_sensitivity = case_sensitivity;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, InMemoryFilesystemCapabilitiesInner> {
        self.inner
            .lock()
            .expect("InMemoryFilesystemCapabilities mutex poisoned")
    }
}

impl FilesystemCapabilities for InMemoryFilesystemCapabilities {
    fn supports_xattr(&self) -> bool {
        self.lock().supports_xattr
    }

    fn case_sensitivity(&self) -> CaseSensitivity {
        self.lock().case_sensitivity
    }

    fn read_tag(&self, path: &Path, name: &str) -> io::Result<Option<String>> {
        let inner = self.lock();
        if !inner.supports_xattr {
            return Err(unsupported_tag_error());
        }
        Ok(inner
            .tags
            .get(&(path.to_path_buf(), name.to_string()))
            .cloned())
    }

    fn write_tag(&self, path: &Path, name: &str, value: &str) -> io::Result<()> {
        let mut inner = self.lock();
        if !inner.supports_xattr {
            return Err(unsupported_tag_error());
        }
        inner
            .tags
            .insert((path.to_path_buf(), name.to_string()), value.to_string());
        Ok(())
    }

    fn remove_tag(&self, path: &Path, name: &str) -> io::Result<()> {
        let mut inner = self.lock();
        if !inner.supports_xattr {
            return Err(unsupported_tag_error());
        }
        inner.tags.remove(&(path.to_path_buf(), name.to_string()));
        Ok(())
    }
}

fn unsupported_tag_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "filesystem does not support metadata tags",
    )
}

/// Native `FilesystemCapabilities`. Compile-time default for the
/// current host, with the Unix xattr bridge live since C8; the Windows
/// ADS bridge lands with Wave 12 (C6-6) — until then Windows reports no
/// xattr support and every tag call returns `Unsupported`, which routes
/// callers onto the side-file fallback.
///
/// Per-OS defaults reflect the documented matrix in
/// `docs/architecture/platform-abstractions.md`:
///
/// | OS | xattr | case |
/// |---|---|---|
/// | macOS | yes | insensitive (APFS / HFS+ default) |
/// | Linux | yes (ext4/xfs/btrfs typical) | sensitive |
/// | Windows | no (ADS lands later) | insensitive |
#[derive(Debug)]
pub struct NativeFilesystemCapabilities {
    supports_xattr: bool,
    case_sensitivity: CaseSensitivity,
}

impl Default for NativeFilesystemCapabilities {
    fn default() -> Self {
        Self::for_current_host()
    }
}

impl NativeFilesystemCapabilities {
    #[cfg(target_os = "macos")]
    pub fn for_current_host() -> Self {
        Self {
            supports_xattr: true,
            case_sensitivity: CaseSensitivity::Insensitive,
        }
    }

    #[cfg(target_os = "linux")]
    pub fn for_current_host() -> Self {
        Self {
            supports_xattr: true,
            case_sensitivity: CaseSensitivity::Sensitive,
        }
    }

    #[cfg(target_os = "windows")]
    pub fn for_current_host() -> Self {
        Self {
            supports_xattr: false,
            case_sensitivity: CaseSensitivity::Insensitive,
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    pub fn for_current_host() -> Self {
        Self {
            supports_xattr: false,
            case_sensitivity: CaseSensitivity::Sensitive,
        }
    }
}

impl FilesystemCapabilities for NativeFilesystemCapabilities {
    fn supports_xattr(&self) -> bool {
        self.supports_xattr
    }

    fn case_sensitivity(&self) -> CaseSensitivity {
        self.case_sensitivity
    }

    #[cfg(unix)]
    fn read_tag(&self, path: &Path, name: &str) -> io::Result<Option<String>> {
        match xattr::get(path, name) {
            Ok(Some(bytes)) => Ok(Some(String::from_utf8(bytes).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "metadata tag is not UTF-8")
            })?)),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        }
    }

    #[cfg(unix)]
    fn write_tag(&self, path: &Path, name: &str, value: &str) -> io::Result<()> {
        xattr::set(path, name, value.as_bytes())
    }

    #[cfg(unix)]
    fn remove_tag(&self, path: &Path, name: &str) -> io::Result<()> {
        match xattr::remove(path, name) {
            Ok(()) => Ok(()),
            // Removing an absent tag is convergence, not failure.
            Err(error) if error.raw_os_error() == Some(NO_ATTR_ERRNO) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    #[cfg(not(unix))]
    fn read_tag(&self, _path: &Path, _name: &str) -> io::Result<Option<String>> {
        Err(unsupported_tag_error())
    }

    #[cfg(not(unix))]
    fn write_tag(&self, _path: &Path, _name: &str, _value: &str) -> io::Result<()> {
        Err(unsupported_tag_error())
    }

    #[cfg(not(unix))]
    fn remove_tag(&self, _path: &Path, _name: &str) -> io::Result<()> {
        Err(unsupported_tag_error())
    }
}

/// `ENOATTR` on macOS (93); Linux reports absent attributes as `ENODATA`
/// (61). Both mean "tag was not there", which `remove_tag` treats as
/// success.
#[cfg(target_os = "macos")]
const NO_ATTR_ERRNO: i32 = 93;
#[cfg(all(unix, not(target_os = "macos")))]
const NO_ATTR_ERRNO: i32 = 61;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_caps_default_matches_typical_linux_ext4() {
        let caps = InMemoryFilesystemCapabilities::default();
        assert!(caps.supports_xattr());
        assert_eq!(caps.case_sensitivity(), CaseSensitivity::Sensitive);
    }

    #[test]
    fn in_memory_caps_setters_round_trip() {
        let caps = InMemoryFilesystemCapabilities::default();
        caps.set_supports_xattr(false);
        caps.set_case_sensitivity(CaseSensitivity::Insensitive);
        assert!(!caps.supports_xattr());
        assert_eq!(caps.case_sensitivity(), CaseSensitivity::Insensitive);
    }

    #[test]
    fn in_memory_tags_round_trip_and_remove() {
        let caps = InMemoryFilesystemCapabilities::default();
        let path = Path::new("/tmp/file.txt");
        assert_eq!(caps.read_tag(path, "op-id").expect("read"), None);
        caps.write_tag(path, "op-id", "abc123").expect("write");
        assert_eq!(
            caps.read_tag(path, "op-id").expect("read"),
            Some("abc123".to_string())
        );
        caps.remove_tag(path, "op-id").expect("remove");
        assert_eq!(caps.read_tag(path, "op-id").expect("read"), None);
    }

    #[test]
    fn in_memory_tags_fail_as_unsupported_when_xattr_disabled() {
        let caps = InMemoryFilesystemCapabilities::new(false, CaseSensitivity::Sensitive);
        let path = Path::new("/tmp/file.txt");
        let error = caps
            .write_tag(path, "op-id", "abc123")
            .expect_err("must reject");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_caps_on_macos_default_to_insensitive_with_xattr_support() {
        let caps = NativeFilesystemCapabilities::for_current_host();
        assert!(caps.supports_xattr());
        assert_eq!(caps.case_sensitivity(), CaseSensitivity::Insensitive);
    }

    #[cfg(unix)]
    #[test]
    fn native_tags_round_trip_on_real_file() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let file = dir.path().join("tagged.txt");
        std::fs::write(&file, b"payload").expect("seed file");

        let caps = NativeFilesystemCapabilities::for_current_host();
        // Some CI filesystems mount tmp without xattr support; treat an
        // Unsupported write as an acceptable environment, not a failure.
        match caps.write_tag(&file, "sh.arn.vapor.test", "op-42") {
            Ok(()) => {
                assert_eq!(
                    caps.read_tag(&file, "sh.arn.vapor.test").expect("read"),
                    Some("op-42".to_string())
                );
                caps.remove_tag(&file, "sh.arn.vapor.test").expect("remove");
                assert_eq!(
                    caps.read_tag(&file, "sh.arn.vapor.test").expect("read"),
                    None
                );
            }
            Err(error) => {
                assert!(
                    matches!(
                        error.kind(),
                        io::ErrorKind::Unsupported | io::ErrorKind::PermissionDenied
                    ),
                    "unexpected xattr failure: {error}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_remove_of_absent_tag_is_ok() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let file = dir.path().join("untouched.txt");
        std::fs::write(&file, b"payload").expect("seed file");

        let caps = NativeFilesystemCapabilities::for_current_host();
        caps.remove_tag(&file, "sh.arn.vapor.absent")
            .expect("absent tag removal is convergence");
    }
}
