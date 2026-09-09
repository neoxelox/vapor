//! What the machine running the suite can do. Scenarios declare needs;
//! the runner skips a scenario whose need the host cannot meet and says
//! which one, so a skip is never silent.

use std::fs;
use std::path::Path;
use std::process::Command;

/// A capability a scenario requires from the host or the run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Need {
    /// A real fs-watch implementation exists for this OS (the daemon
    /// cannot start without one).
    NativeWatcher,
    /// Unix signals, `mkfifo`, POSIX modes.
    Unix,
    /// FIFOs can be created in the sandbox.
    Fifo,
    /// The executable bit survives on the sandbox filesystem.
    PosixMode,
    /// Extended attributes work in the sandbox (op-id tags).
    Xattr,
    /// `launchctl` is usable and no Vapor LaunchAgent exists. Host
    /// mutating: only honored together with [`Need::Full`].
    Launchd,
    /// The run was started with `--full`.
    Full,
    /// The filesystem provider is the run's provider (the scenario
    /// manipulates the cloud root directly).
    Filesystem,
    /// The Google Drive provider is the run's provider.
    Gdrive,
    /// The sandbox sits on a case-insensitive filesystem.
    CaseInsensitiveFs,
    /// The sandbox sits on a case-sensitive filesystem.
    CaseSensitiveFs,
    /// Throwaway disk images can be created and mounted (macOS `hdiutil`).
    DiskImage,
    /// The sandbox filesystem accepts a file name that is not UTF-8
    /// (ext4 and tmpfs do; APFS refuses).
    NonUtf8Names,
}

impl Need {
    pub fn label(self) -> &'static str {
        match self {
            Need::NativeWatcher => "native-watcher",
            Need::Unix => "unix",
            Need::Fifo => "fifo",
            Need::PosixMode => "posix-mode",
            Need::Xattr => "xattr",
            Need::Launchd => "launchd",
            Need::Full => "full",
            Need::Filesystem => "filesystem-provider",
            Need::Gdrive => "gdrive-provider",
            Need::CaseInsensitiveFs => "case-insensitive-fs",
            Need::CaseSensitiveFs => "case-sensitive-fs",
            Need::DiskImage => "disk-image",
            Need::NonUtf8Names => "non-utf8-names",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provider {
    Filesystem,
    Gdrive,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Filesystem => "filesystem",
            Provider::Gdrive => "gdrive",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Host {
    pub os: &'static str,
    pub arch: &'static str,
    pub native_watcher: bool,
    pub unix: bool,
    pub fifo: bool,
    pub posix_mode: bool,
    pub xattr: bool,
    pub launchd: bool,
    pub launchd_blocker: Option<String>,
    pub case_insensitive_fs: bool,
    pub disk_image: bool,
    pub non_utf8_names: bool,
    pub full: bool,
    pub provider: Provider,
}

impl Host {
    /// Probes the host against `sandbox_root` (capabilities of the
    /// filesystem the sandbox lives on) with the run's flags.
    pub fn detect(sandbox_root: &Path, full: bool, provider: Provider) -> Self {
        let unix = cfg!(unix);
        let (launchd, launchd_blocker) = detect_launchd(full);
        Self {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            // FSEvents on macOS, inotify on Linux; the Windows stub
            // refuses to start until its implementation lands.
            native_watcher: cfg!(any(target_os = "macos", target_os = "linux")),
            unix,
            fifo: unix && probe_fifo(sandbox_root),
            posix_mode: unix && probe_posix_mode(sandbox_root),
            xattr: probe_xattr(sandbox_root),
            launchd,
            launchd_blocker,
            case_insensitive_fs: probe_case_insensitive(sandbox_root),
            disk_image: crate::diskimage::DiskImage::available(),
            non_utf8_names: probe_non_utf8_names(sandbox_root),
            full,
            provider,
        }
    }

    /// `Err(reason)` when the host cannot meet `need`.
    pub fn check(&self, need: Need) -> Result<(), String> {
        let ok = match need {
            Need::NativeWatcher => self.native_watcher,
            Need::Unix => self.unix,
            Need::Fifo => self.fifo,
            Need::PosixMode => self.posix_mode,
            Need::Xattr => self.xattr,
            Need::Launchd => self.launchd,
            Need::Full => self.full,
            Need::Filesystem => self.provider == Provider::Filesystem,
            Need::Gdrive => self.provider == Provider::Gdrive,
            Need::CaseInsensitiveFs => self.case_insensitive_fs,
            Need::CaseSensitiveFs => !self.case_insensitive_fs,
            Need::DiskImage => self.disk_image,
            Need::NonUtf8Names => self.non_utf8_names,
        };
        if ok {
            Ok(())
        } else {
            let detail = match need {
                Need::Launchd => self
                    .launchd_blocker
                    .clone()
                    .unwrap_or_else(|| "launchd unavailable".to_string()),
                Need::Full => "run with --full to include it".to_string(),
                Need::NativeWatcher => format!("no native fs-watch on {}", self.os),
                _ => format!("host lacks {}", need.label()),
            };
            Err(format!("needs {}: {detail}", need.label()))
        }
    }
}

fn probe_fifo(root: &Path) -> bool {
    #[cfg(unix)]
    {
        let path = root.join(".probe.fifo");
        let _ = fs::remove_file(&path);
        let ok = Command::new("mkfifo")
            .arg(&path)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        let _ = fs::remove_file(&path);
        ok
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        false
    }
}

fn probe_posix_mode(root: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = root.join(".probe.mode");
        if fs::write(&path, b"x").is_err() {
            return false;
        }
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
        let mode = fs::metadata(&path)
            .map(|metadata| metadata.permissions().mode() & 0o777)
            .unwrap_or(0);
        let _ = fs::remove_file(&path);
        mode == 0o755
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        false
    }
}

fn probe_xattr(root: &Path) -> bool {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let path = root.join(".probe.xattr");
        if fs::write(&path, b"x").is_err() {
            return false;
        }
        let mut command = if cfg!(target_os = "macos") {
            let mut command = Command::new("xattr");
            command.args(["-w", "sh.arn.vapor.probe", "1"]);
            command
        } else {
            let mut command = Command::new("setfattr");
            command.args(["-n", "user.sh.arn.vapor.probe", "-v", "1"]);
            command
        };
        let ok = command
            .arg(&path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        let _ = fs::remove_file(&path);
        ok
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = root;
        false
    }
}

fn probe_non_utf8_names(root: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = root.join(std::ffi::OsStr::from_bytes(b".probe-\xff"));
        let accepted = fs::write(&path, b"x").is_ok();
        let _ = fs::remove_file(&path);
        accepted
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        false
    }
}

fn probe_case_insensitive(root: &Path) -> bool {
    let lower = root.join(".probe-case");
    let upper = root.join(".PROBE-CASE");
    let _ = fs::remove_file(&lower);
    let _ = fs::remove_file(&upper);
    if fs::write(&lower, b"x").is_err() {
        return false;
    }
    let insensitive = upper.exists();
    let _ = fs::remove_file(&lower);
    let _ = fs::remove_file(&upper);
    insensitive
}

fn detect_launchd(full: bool) -> (bool, Option<String>) {
    if !full {
        return (false, Some("run with --full to include it".to_string()));
    }
    #[cfg(target_os = "macos")]
    {
        let plist = launch_agent_plist_path();
        if plist.exists() {
            return (
                false,
                Some(format!(
                    "{} already exists (a real Vapor install?); the round-trip would uninstall it",
                    plist.display()
                )),
            );
        }
        let domain = launchd_domain_target();
        let available = Command::new("launchctl")
            .args(["print", &domain])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !available {
            return (
                false,
                Some(format!(
                    "launchctl domain {domain} is unavailable in this session"
                )),
            );
        }
        (true, None)
    }
    #[cfg(not(target_os = "macos"))]
    {
        (
            false,
            Some("the service round-trip drives launchd; this host has none".to_string()),
        )
    }
}

/// `~/Library/LaunchAgents/sh.arn.vapor.daemon.plist`.
pub fn launch_agent_plist_path() -> std::path::PathBuf {
    launch_agent_plist_for(vapor_shared::constants::service::DAEMON_LABEL)
}

pub fn supervisor_plist_path() -> std::path::PathBuf {
    launch_agent_plist_for(vapor_shared::constants::service::SUPERVISOR_LABEL)
}

fn launch_agent_plist_for(label: &str) -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist"))
}

/// `gui/<uid>`.
pub fn launchd_domain_target() -> String {
    #[cfg(unix)]
    {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        format!("gui/{uid}")
    }
    #[cfg(not(unix))]
    {
        "gui/0".to_string()
    }
}
