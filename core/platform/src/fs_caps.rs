//! Filesystem capabilities trait + per-OS native implementation.
//!
//! See `docs/architecture/platform-abstractions.md` §`FilesystemCapabilities`.
//! Wave 4 ships the trait + an in-memory fake; the native xattr / ADS
//! bridges land alongside the C8 wave when the self-write-cache and
//! op-id tagging consume them.

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
}

/// In-memory `FilesystemCapabilities` for tests. Defaults: xattr
/// supported, case-sensitive — matches the typical Linux ext4 / xfs
/// default. Tests override either toggle to assert per-OS behavior.
#[derive(Debug)]
pub struct InMemoryFilesystemCapabilities {
    inner: Mutex<InMemoryFilesystemCapabilitiesInner>,
}

#[derive(Debug)]
struct InMemoryFilesystemCapabilitiesInner {
    supports_xattr: bool,
    case_sensitivity: CaseSensitivity,
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
            }),
        }
    }

    pub fn set_supports_xattr(&self, supports_xattr: bool) {
        self.inner
            .lock()
            .expect("InMemoryFilesystemCapabilities mutex poisoned")
            .supports_xattr = supports_xattr;
    }

    pub fn set_case_sensitivity(&self, case_sensitivity: CaseSensitivity) {
        self.inner
            .lock()
            .expect("InMemoryFilesystemCapabilities mutex poisoned")
            .case_sensitivity = case_sensitivity;
    }
}

impl FilesystemCapabilities for InMemoryFilesystemCapabilities {
    fn supports_xattr(&self) -> bool {
        self.inner
            .lock()
            .expect("InMemoryFilesystemCapabilities mutex poisoned")
            .supports_xattr
    }

    fn case_sensitivity(&self) -> CaseSensitivity {
        self.inner
            .lock()
            .expect("InMemoryFilesystemCapabilities mutex poisoned")
            .case_sensitivity
    }
}

/// Native `FilesystemCapabilities`. Compile-time default for the
/// current host. Wave 4 wires the trait through; xattr / ADS probes
/// land alongside the C8 self-write-cache work.
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
}

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

    #[cfg(target_os = "macos")]
    #[test]
    fn native_caps_on_macos_default_to_insensitive_with_xattr_support() {
        let caps = NativeFilesystemCapabilities::for_current_host();
        assert!(caps.supports_xattr());
        assert_eq!(caps.case_sensitivity(), CaseSensitivity::Insensitive);
    }
}
