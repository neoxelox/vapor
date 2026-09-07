//! `vapor doctor`: platform-aware sanity checks.
//!
//! Every row names what it probed and what it found, so the output reads
//! the same in a sandbox (`VAPOR_DIR` under `.vapor/e2e`) and on a real
//! host. Rows that read host-global state carry a `host_` prefix so a
//! sandboxed run is never mistaken for sandbox state. Linux and Windows
//! checks land alongside their platform support. The current set:
//!
//! - `vapor_dir` exists, is writable, and (Unix) has private permissions.
//! - `ipc_socket_path` fits the Unix socket-address budget, with an
//!   explanation when it was relocated under the OS temp directory.
//! - `vapord_binary` is discoverable next to the CLI, inside the app
//!   bundle, or on `PATH` (one resolver shared with `vapor service`).
//! - `secret_store` reports whether provider tokens persist on this OS.
//! - `throttle_inputs` reports where the daemon's throttle signals come
//!   from on this host.
//! - `host_launch_agent_plist` (macOS) is present at the documented path.
//!
//! The report renders as text rows or, with `--json`, as
//! `{"checks": [...], "worst_status": "..."}`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use vapor_shared::constants;

use super::daemon_binary;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCheckStatus {
    Ok,
    Warning,
    Failure,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorCheckStatus,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

/// The `--json` shape: the rows plus the aggregate the exit code is
/// derived from, so scripts do not have to recompute it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReportJson<'a> {
    pub checks: &'a [DoctorCheck],
    pub worst_status: DoctorCheckStatus,
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

    pub fn render_json(&self) -> DoctorReportJson<'_> {
        DoctorReportJson {
            checks: &self.checks,
            worst_status: self.worst_status(),
        }
    }

    pub fn render_text(&self) -> String {
        let mut out = String::new();
        for check in &self.checks {
            let badge = match check.status {
                DoctorCheckStatus::Ok => "OK",
                DoctorCheckStatus::Warning => "WARN",
                DoctorCheckStatus::Failure => "FAIL",
            };
            out.push_str(&format!("[{badge}] {}: {}\n", check.name, check.detail));
        }
        out
    }
}

pub fn run() -> DoctorReport {
    let mut checks = vec![
        check_vapor_directory(&vapor_shared::runtime_paths::vapor_directory()),
        check_ipc_socket_path(&vapor_shared::runtime_paths::ipc_socket_location()),
        check_daemon_binary(daemon_binary::locate()),
        check_secret_store(),
        check_throttle_inputs(std::env::var(constants::env::VAPOR_THROTTLE_INPUTS).ok()),
    ];
    if cfg!(target_os = "macos") {
        checks.push(check_macos_launch_agent_plist());
    }
    if cfg!(target_os = "linux") {
        checks.push(check_linux_systemd_unit());
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

fn check_daemon_binary(found: Option<daemon_binary::DaemonBinary>) -> DoctorCheck {
    let name = "vapord_binary".to_string();
    match found {
        Some(daemon) => DoctorCheck {
            name,
            status: DoctorCheckStatus::Ok,
            detail: format!(
                "found at {} ({})",
                daemon.path.display(),
                daemon.source.label()
            ),
        },
        None => DoctorCheck {
            name,
            status: DoctorCheckStatus::Failure,
            detail: "vapord not found next to this CLI, inside the app bundle, or on PATH"
                .to_string(),
        },
    }
}

/// Whether `vapor auth login` tokens outlive the process on this OS.
/// Constructing the store touches nothing, so this never prompts.
fn check_secret_store() -> DoctorCheck {
    use vapor_platform::SecretStore;
    let name = "secret_store".to_string();
    match vapor_platform::NativeSecretStore::for_current_user() {
        Ok(store) if store.is_persistent() => DoctorCheck {
            name,
            status: DoctorCheckStatus::Ok,
            detail: format!("{}; tokens persist across restarts", store.describe()),
        },
        Ok(_) => DoctorCheck {
            name,
            status: DoctorCheckStatus::Warning,
            detail: "store is not persistent; tokens are lost when the process exits".to_string(),
        },
        Err(error) => DoctorCheck {
            name,
            status: DoctorCheckStatus::Warning,
            detail: format!(
                "no native secret store on this OS ({error}); `vapor auth login` keeps tokens in memory only"
            ),
        },
    }
}

/// Where the daemon's throttle inputs would come from if started here.
fn check_throttle_inputs(override_value: Option<String>) -> DoctorCheck {
    let name = "throttle_inputs".to_string();
    let requested_static = override_value
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| value == constants::engine::THROTTLE_INPUTS_STATIC);
    if requested_static {
        return DoctorCheck {
            name,
            status: DoctorCheckStatus::Ok,
            detail: format!(
                "pinned to neutral inputs by {}; the throttle will not see load, battery or user presence",
                constants::env::VAPOR_THROTTLE_INPUTS
            ),
        };
    }
    let scripted_file = override_value
        .as_deref()
        .map(str::trim)
        .and_then(|value| value.strip_prefix(constants::engine::THROTTLE_INPUTS_FILE_PREFIX))
        .filter(|path| !path.is_empty());
    if let Some(path) = scripted_file {
        return DoctorCheck {
            name,
            status: DoctorCheckStatus::Ok,
            detail: format!(
                "scripted by {}: every sample re-reads {path}; a missing file samples as neutral inputs",
                constants::env::VAPOR_THROTTLE_INPUTS
            ),
        };
    }
    if vapor_platform::NativePlatformMetricsSampler::has_native_sampling() {
        DoctorCheck {
            name,
            status: DoctorCheckStatus::Ok,
            detail: format!(
                "host signals: {}",
                vapor_platform::NativePlatformMetricsSampler::input_sources()
            ),
        }
    } else {
        DoctorCheck {
            name,
            status: DoctorCheckStatus::Warning,
            detail: "static placeholders on this OS; load, battery and thermal pressure will not throttle the daemon"
                .to_string(),
        }
    }
}

fn check_macos_launch_agent_plist() -> DoctorCheck {
    let name = "host_launch_agent_plist".to_string();
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

/// The systemd user unit `vapor service install` writes on Linux.
fn check_linux_systemd_unit() -> DoctorCheck {
    let name = "host_systemd_user_unit".to_string();
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    let Some(config_home) = config_home else {
        return DoctorCheck {
            name,
            status: DoctorCheckStatus::Warning,
            detail: "neither XDG_CONFIG_HOME nor HOME is set; cannot locate the systemd user unit"
                .to_string(),
        };
    };
    let unit_path = config_home
        .join("systemd/user")
        .join(format!("{}.service", constants::service::DAEMON_LABEL));
    if unit_path.exists() {
        DoctorCheck {
            name,
            status: DoctorCheckStatus::Ok,
            detail: format!("found at {}", unit_path.display()),
        }
    } else {
        DoctorCheck {
            name,
            status: DoctorCheckStatus::Warning,
            detail: format!(
                "{} not found; run `vapor service install` to create it",
                unit_path.display()
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
    fn daemon_binary_check_names_the_source_or_fails() {
        let found = check_daemon_binary(Some(daemon_binary::DaemonBinary {
            path: PathBuf::from("/Applications/Vapor.app/Contents/MacOS/vapord"),
            source: daemon_binary::DaemonBinarySource::Bundled,
        }));
        assert_eq!(found.status, DoctorCheckStatus::Ok);
        assert!(found.detail.contains("bundled in the app"));
        let missing = check_daemon_binary(None);
        assert_eq!(missing.status, DoctorCheckStatus::Failure);
    }

    #[test]
    fn throttle_inputs_check_reports_the_static_override() {
        let pinned = check_throttle_inputs(Some(" static ".to_string()));
        assert_eq!(pinned.status, DoctorCheckStatus::Ok);
        assert!(pinned.detail.contains("pinned"));
        let host = check_throttle_inputs(None);
        assert!(!host.detail.contains("pinned"));
        let scripted = check_throttle_inputs(Some("file:/tmp/inputs.json".to_string()));
        assert_eq!(scripted.status, DoctorCheckStatus::Ok);
        assert!(scripted.detail.contains("/tmp/inputs.json"));
    }

    #[test]
    fn json_shape_carries_rows_and_the_aggregate_status() {
        let report = DoctorReport {
            checks: vec![
                DoctorCheck {
                    name: "a".to_string(),
                    status: DoctorCheckStatus::Ok,
                    detail: "fine".to_string(),
                },
                DoctorCheck {
                    name: "b".to_string(),
                    status: DoctorCheckStatus::Warning,
                    detail: "hmm".to_string(),
                },
            ],
        };
        let json = serde_json::to_value(report.render_json()).expect("serialize");
        assert_eq!(json["worst_status"], "warning");
        assert_eq!(json["checks"][0]["name"], "a");
        assert_eq!(json["checks"][0]["status"], "ok");
        assert_eq!(json["checks"][1]["detail"], "hmm");
        assert_eq!(
            json.as_object().expect("object").keys().collect::<Vec<_>>(),
            vec!["checks", "worst_status"]
        );
        assert!(report.render_text().contains("[WARN] b: hmm"));
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
