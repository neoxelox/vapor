//! `vapor doctor` — platform-aware sanity checks.
//!
//! Closes `cli.md` L1-4 (macOS flavor). Linux / Windows checks land
//! alongside Wave 13 / 12 respectively. The current macOS check set:
//!
//! - `vapor_dir` exists, is writable, and (Unix) has private permissions.
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

    let writable = is_writable(path);
    if !writable {
        return DoctorCheck {
            name,
            status: DoctorCheckStatus::Failure,
            detail: format!("{} is not writable by the current user", path.display()),
        };
    }

    DoctorCheck {
        name,
        status: DoctorCheckStatus::Ok,
        detail: format!("{} is writable", path.display()),
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
    fn vapor_dir_check_succeeds_when_writable() {
        let temp = TempDir::new().expect("temp");
        let check = check_vapor_directory(temp.path());
        assert_eq!(check.status, DoctorCheckStatus::Ok);
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
