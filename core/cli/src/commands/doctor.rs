//! `vapor doctor` — platform-aware sanity checks.
//!
//! Closes `cli.md` L1-4 (macOS flavor). Linux / Windows checks land
//! alongside Wave 13 / 12 respectively. The current macOS check set:
//!
//! - `vapor_dir` exists, is writable, and (Unix) has private permissions.
//! - The IPC socket path fits the Unix socket-address budget, with an
//!   explanation when it was relocated under the OS temp directory.
//! - The daemon binary `vapord` is discoverable via `PATH` or as a
//!   sibling of the running CLI binary.
//! - The LaunchAgent plist is present at the documented path.
//!
//! The report is rendered as a list of `DoctorCheck` rows so the binary
//! can format them either as human-readable text or as `--json`.

use std::fs;
use std::path::{Path, PathBuf};

use vapor_shared::constants;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DoctorCheckStatus {
    Ok,
    Warning,
    Failure,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorCheckStatus,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn worst_status(&self) -> DoctorCheckStatus {
        let mut worst = DoctorCheckStatus::Ok;
        for check in &self.checks {
            if matches!(check.status, DoctorCheckStatus::Failure) {
                return DoctorCheckStatus::Failure;
            }
            if matches!(check.status, DoctorCheckStatus::Warning) {
                worst = DoctorCheckStatus::Warning;
            }
        }
        worst
    }
}

pub fn run() -> DoctorReport {
    let mut checks = Vec::new();
    checks.push(check_vapor_directory(
        &vapor_shared::runtime_paths::vapor_directory(),
    ));
    checks.push(check_ipc_socket_path(
        &vapor_shared::runtime_paths::ipc_socket_location(),
    ));
    checks.push(check_daemon_binary());
    if cfg!(target_os = "macos") {
        checks.push(check_macos_launch_agent_plist());
    }
    DoctorReport { checks }
}

fn check_vapor_directory(path: &Path) -> DoctorCheck {
    let name = "vapor_dir".to_string();
    if !path.exists() {
        return DoctorCheck {
            name,
            status: DoctorCheckStatus::Warning,
            detail: format!(
                "{} does not exist yet — `vapor run` will create it on first start",
                path.display()
            ),
        };
    }

    if !is_writable(path) {
        return DoctorCheck {
            name,
            status: DoctorCheckStatus::Failure,
            detail: format!("{} is not writable by the current user", path.display()),
        };
    }

    if let Some(reason) = private_mode_violation(path) {
        return DoctorCheck {
            name,
            status: DoctorCheckStatus::Warning,
            detail: format!(
                "{} is writable but has loose permissions: {reason}",
                path.display()
            ),
        };
    }

    DoctorCheck {
        name,
        status: DoctorCheckStatus::Ok,
        detail: format!("{} is writable and private", path.display()),
    }
}

/// Returns `Some(reason)` when the directory's mode is more permissive
/// than `PRIVATE_DIRECTORY_MODE` (`0o700`) on Unix. Returns `None` on
/// Windows because NTFS DACLs scope per-user inheritance handles the
/// private-directory invariant, and we have no `0o700` analogue.
#[cfg(unix)]
fn private_mode_violation(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(path).ok()?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode == constants::runtime::PRIVATE_DIRECTORY_MODE {
        return None;
    }
    Some(format!(
        "found mode {:#o}, expected {:#o} (group/other access leaks state to other users)",
        mode,
        constants::runtime::PRIVATE_DIRECTORY_MODE
    ))
}

#[cfg(not(unix))]
fn private_mode_violation(_path: &Path) -> Option<String> {
    None
}

/// Surfaces the socket relocation so it is never a silent surprise: a
/// deep `vapor_dir` used to cost the daemon its IPC endpoint with only
/// a log WARNING while `vapor status` claimed the daemon was not
/// running. Warning (not Failure) — the relocation is functional, but
/// the user should know their socket is not at the canonical path.
fn check_ipc_socket_path(location: &vapor_shared::runtime_paths::IpcSocketLocation) -> DoctorCheck {
    let name = "ipc_socket_path".to_string();
    if location.path.as_os_str().len() > constants::ipc::MAX_SOCKET_PATH_BYTES {
        return DoctorCheck {
            name,
            status: DoctorCheckStatus::Failure,
            detail: format!(
                "{} exceeds the {}-byte Unix socket-address budget even after relocation; the daemon cannot serve IPC",
                location.path.display(),
                constants::ipc::MAX_SOCKET_PATH_BYTES
            ),
        };
    }
    match &location.relocated_from {
        None => DoctorCheck {
            name,
            status: DoctorCheckStatus::Ok,
            detail: format!("canonical at {}", location.path.display()),
        },
        Some(canonical) => DoctorCheck {
            name,
            status: DoctorCheckStatus::Warning,
            detail: format!(
                "{} exceeds the {}-byte Unix socket-address budget; daemon and CLI rendezvous at {} instead",
                canonical.display(),
                constants::ipc::MAX_SOCKET_PATH_BYTES,
                location.path.display()
            ),
        },
    }
}

fn check_daemon_binary() -> DoctorCheck {
    let name = "vapord_binary".to_string();
    if let Some(path) = locate_daemon_binary() {
        DoctorCheck {
            name,
            status: DoctorCheckStatus::Ok,
            detail: format!("found at {}", path.display()),
        }
    } else {
        DoctorCheck {
            name,
            status: DoctorCheckStatus::Failure,
            detail: "vapord not found on PATH or as a sibling of the running CLI binary"
                .to_string(),
        }
    }
}

fn check_macos_launch_agent_plist() -> DoctorCheck {
    let name = "launch_agent_plist".to_string();
    let Some(home) = std::env::var_os("HOME") else {
        return DoctorCheck {
            name,
            status: DoctorCheckStatus::Warning,
            detail: "HOME is not set; cannot locate LaunchAgent plist".to_string(),
        };
    };
    let plist_path = PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", constants::service::DAEMON_LABEL));
    if plist_path.exists() {
        DoctorCheck {
            name,
            status: DoctorCheckStatus::Ok,
            detail: format!("found at {}", plist_path.display()),
        }
    } else {
        DoctorCheck {
            name,
            status: DoctorCheckStatus::Warning,
            detail: format!(
                "{} not found; run `vapor service install` to create it",
                plist_path.display()
            ),
        }
    }
}

fn is_writable(path: &Path) -> bool {
    let probe = path.join(".vapor-doctor-probe");
    match fs::write(&probe, b"vapor doctor write probe") {
        Ok(_) => {
            let _ = fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

fn locate_daemon_binary() -> Option<PathBuf> {
    if let Ok(current_exe) = std::env::current_exe()
        && let Some(parent) = current_exe.parent()
    {
        let sibling = parent.join("vapord");
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("vapord");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn vapor_dir_check_warns_when_missing() {
        let temp = TempDir::new().expect("temp");
        let missing = temp.path().join("missing");
        let check = check_vapor_directory(&missing);
        assert_eq!(check.status, DoctorCheckStatus::Warning);
    }

    #[test]
    fn vapor_dir_check_succeeds_when_writable_and_private() {
        let temp = TempDir::new().expect("temp");
        // tempfile sets a non-0o700 mode by default; tighten it so the
        // private-mode probe is happy on Unix. On non-Unix the helper
        // skips the probe.
        let private = temp.path().join("private");
        vapor_shared::runtime_paths::ensure_private_directory(&private)
            .expect("ensure private dir");
        let check = check_vapor_directory(&private);
        assert_eq!(check.status, DoctorCheckStatus::Ok);
    }

    #[cfg(unix)]
    #[test]
    fn vapor_dir_check_warns_when_permissions_are_loose() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new().expect("temp");
        let dir = temp.path().join("loose");
        fs::create_dir(&dir).expect("create dir");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("set permissions");
        let check = check_vapor_directory(&dir);
        assert_eq!(check.status, DoctorCheckStatus::Warning);
        assert!(check.detail.contains("loose permissions"));
    }

    #[test]
    fn ipc_socket_check_is_ok_for_a_canonical_location() {
        let location = vapor_shared::runtime_paths::IpcSocketLocation {
            path: PathBuf::from("/Users/alex/.vapor/vapord.sock"),
            relocated_from: None,
        };
        let check = check_ipc_socket_path(&location);
        assert_eq!(check.status, DoctorCheckStatus::Ok);
        assert!(check.detail.contains("canonical"));
    }

    #[test]
    fn ipc_socket_check_warns_and_names_both_paths_when_relocated() {
        let location = vapor_shared::runtime_paths::IpcSocketLocation {
            path: PathBuf::from("/tmp/vapor-0123456789abcdef/vapord.sock"),
            relocated_from: Some(PathBuf::from("/very/deep/vapor/dir/vapord.sock")),
        };
        let check = check_ipc_socket_path(&location);
        assert_eq!(check.status, DoctorCheckStatus::Warning);
        assert!(
            check.detail.contains("/very/deep/vapor/dir/vapord.sock")
                && check
                    .detail
                    .contains("/tmp/vapor-0123456789abcdef/vapord.sock"),
            "detail must explain the rendezvous: {}",
            check.detail
        );
    }

    #[test]
    fn ipc_socket_check_fails_when_even_the_relocated_path_is_too_long() {
        let too_long = format!("/{}/vapord.sock", "t".repeat(120));
        let location = vapor_shared::runtime_paths::IpcSocketLocation {
            path: PathBuf::from(too_long),
            relocated_from: Some(PathBuf::from("/also/too/deep/vapord.sock")),
        };
        let check = check_ipc_socket_path(&location);
        assert_eq!(check.status, DoctorCheckStatus::Failure);
    }

    #[test]
    fn report_worst_status_picks_failure_over_warning() {
        let report = DoctorReport {
            checks: vec![
                DoctorCheck {
                    name: "ok".to_string(),
                    status: DoctorCheckStatus::Ok,
                    detail: String::new(),
                },
                DoctorCheck {
                    name: "warn".to_string(),
                    status: DoctorCheckStatus::Warning,
                    detail: String::new(),
                },
                DoctorCheck {
                    name: "fail".to_string(),
                    status: DoctorCheckStatus::Failure,
                    detail: String::new(),
                },
            ],
        };
        assert_eq!(report.worst_status(), DoctorCheckStatus::Failure);
    }

    #[test]
    fn report_worst_status_warns_when_only_warnings_present() {
        let report = DoctorReport {
            checks: vec![
                DoctorCheck {
                    name: "ok".to_string(),
                    status: DoctorCheckStatus::Ok,
                    detail: String::new(),
                },
                DoctorCheck {
                    name: "warn".to_string(),
                    status: DoctorCheckStatus::Warning,
                    detail: String::new(),
                },
            ],
        };
        assert_eq!(report.worst_status(), DoctorCheckStatus::Warning);
    }

    #[test]
    fn report_worst_status_ok_when_all_pass() {
        let report = DoctorReport {
            checks: vec![DoctorCheck {
                name: "ok".to_string(),
                status: DoctorCheckStatus::Ok,
                detail: String::new(),
            }],
        };
        assert_eq!(report.worst_status(), DoctorCheckStatus::Ok);
    }
}
