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
//! - `gdrive-oauth-client` names the Google Drive OAuth client a sign-in
//!   would use: built in, from the environment, or none.
//! - `throttle_inputs` reports where the daemon's throttle signals come
//!   from on this host.
//! - `host_launch_agent_plist` (macOS) is present at the documented path.
//! - `profiles` says when no sync profile exists yet and which command
//!   sets one up, or what in `vapor.json` stops one from running.
//! - `profile:<id>`, one for each profile: its local root can be listed
//!   and written, its cloud root can be listed (filesystem provider),
//!   and it is signed in (a provider with an account). A refused macOS
//!   privacy permission is named with where to allow it.
//!
//! The report renders as text rows or, with `--json`, as
//! `{"checks": [...], "worstStatus": "..."}`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use vapor_shared::constants;

use super::daemon_binary;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DoctorCheckStatus {
    Ok,
    Warning,
    Failure,
}

impl DoctorCheckStatus {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Ok => constants::doctor::STATUS_OK,
            Self::Warning => constants::doctor::STATUS_WARNING,
            Self::Failure => constants::doctor::STATUS_FAILURE,
        }
    }
}

impl Serialize for DoctorCheckStatus {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire_name())
    }
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
#[serde(rename_all = "camelCase")]
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
        check_gdrive_oauth_client(vapor_providers::gdrive::oauth::client_credentials()),
        check_throttle_inputs(std::env::var(constants::env::VAPOR_THROTTLE_INPUTS).ok()),
    ];
    let config = vapor_shared::config::load_default();
    checks.extend(check_profiles(
        &config,
        vapor_platform::open_store(vapor_platform::SecretAccess::Interactive)
            .ok()
            .as_deref(),
    ));
    if cfg!(target_os = "macos") {
        checks.push(check_macos_launch_agent_plist());
    }
    if cfg!(target_os = "linux") {
        checks.push(check_linux_systemd_unit());
    }
    DoctorReport { checks }
}

fn check_vapor_directory(path: &Path) -> DoctorCheck {
    let name = constants::doctor::CHECK_VAPOR_DIR.to_string();
    if !path.exists() {
        return DoctorCheck {
            name,
            status: DoctorCheckStatus::Warning,
            detail: format!(
                "{} does not exist yet: `vapor run` will create it on first start",
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

/// Surfaces the socket relocation so it is never a silent surprise.
/// Warning, not Failure: the relocation works, but the user should
/// know that the socket is not at the canonical path.
fn check_ipc_socket_path(location: &vapor_shared::runtime_paths::IpcSocketLocation) -> DoctorCheck {
    let name = constants::doctor::CHECK_IPC_SOCKET.to_string();
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
    let name = constants::doctor::CHECK_VAPORD_BINARY.to_string();
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
/// The profile set, then one row for each profile that can run.
fn check_profiles(
    load: &vapor_shared::config::VaporConfigLoadResult,
    secrets: Option<&dyn vapor_platform::SecretStore>,
) -> Vec<DoctorCheck> {
    let set = vapor_daemon::profiles::resolve_profile_set(load);
    let mut checks = Vec::new();
    if set.is_unconfigured() {
        checks.push(DoctorCheck {
            name: constants::doctor::CHECK_PROFILES.to_string(),
            status: DoctorCheckStatus::Warning,
            detail: "no sync profile yet; set Vapor up with `vapor init`".to_string(),
        });
        return checks;
    }
    if !set.problems.is_empty() {
        checks.push(DoctorCheck {
            name: constants::doctor::CHECK_PROFILES.to_string(),
            status: DoctorCheckStatus::Failure,
            detail: set.problems.join("; "),
        });
    }
    for profile in set.profiles.iter().filter(|profile| profile.enabled) {
        checks.push(check_profile(profile, secrets));
    }
    checks
}

fn check_profile(
    profile: &vapor_daemon::profiles::ResolvedProfile,
    secrets: Option<&dyn vapor_platform::SecretStore>,
) -> DoctorCheck {
    let name = format!("{}{}", constants::doctor::CHECK_PROFILE_PREFIX, profile.id);
    let failure = |detail: String| DoctorCheck {
        name: name.clone(),
        status: DoctorCheckStatus::Failure,
        detail,
    };
    if let Some(reason) = &profile.invalid {
        return failure(format!("cannot run as configured: {reason}"));
    }
    let Some(local_root) = profile.scope.local_root.as_deref() else {
        return failure("the local root does not resolve to a folder".to_string());
    };
    let mut notes = Vec::new();
    if local_root.is_dir() {
        if let Err(problem) = folder_can_be_listed(local_root) {
            return failure(format!("local root {}: {problem}", local_root.display()));
        }
        if let Err(problem) = folder_can_be_written(local_root) {
            return failure(format!("local root {}: {problem}", local_root.display()));
        }
        notes.push(format!(
            "local root {} reads and writes",
            local_root.display()
        ));
    } else {
        notes.push(format!(
            "local root {} is not there (created at the first start; a root that synced before is waited for)",
            local_root.display()
        ));
    }
    if profile.provider_kind.trim() == constants::provider::FILESYSTEM {
        let cloud_root = Path::new(profile.scope.cloud_root.as_str());
        if cloud_root.is_dir() {
            if let Err(problem) = folder_can_be_listed(cloud_root) {
                return failure(format!("cloud root {}: {problem}", cloud_root.display()));
            }
            notes.push(format!("cloud root {} reads", cloud_root.display()));
        } else {
            notes.push(format!(
                "cloud root {} is not there yet",
                cloud_root.display()
            ));
        }
    } else if constants::provider::SIGN_IN_REQUIRED.contains(&profile.provider_kind.as_str()) {
        // Names only: a check must not read a secret, which could
        // open a keychain dialog.
        let token_name =
            vapor_daemon::profiles::secret_key(&profile.id, &profile.provider_kind, "token");
        let signed_in = secrets
            .is_some_and(|store| store.list().is_ok_and(|names| names.contains(&token_name)));
        if !signed_in {
            return DoctorCheck {
                name,
                status: DoctorCheckStatus::Warning,
                detail: format!(
                    "not signed in to {}; run `vapor auth login --profile {}`",
                    profile.provider_kind, profile.id
                ),
            };
        }
        notes.push(format!("signed in to {}", profile.provider_kind));
    }
    DoctorCheck {
        name,
        status: DoctorCheckStatus::Ok,
        detail: notes.join("; "),
    }
}

/// A permission error with what to do about it on macOS, where a
/// background process needs the user's consent for some folders.
fn describe_access_error(error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        format!("permission denied; {}", constants::FOLDER_ACCESS_HINT)
    } else {
        error.to_string()
    }
}

fn folder_can_be_listed(folder: &Path) -> Result<(), String> {
    fs::read_dir(folder)
        .map(|_| ())
        .map_err(|error| format!("cannot be listed: {}", describe_access_error(&error)))
}

/// Creates and removes one file under Vapor's own temp prefix, which
/// the watcher and the walk ignore.
fn folder_can_be_written(folder: &Path) -> Result<(), String> {
    let probe = folder.join(format!(
        "{}doctor-{}",
        constants::provider::TEMP_FILE_PREFIX,
        std::process::id()
    ));
    let written = fs::write(&probe, b"")
        .map_err(|error| format!("cannot be written: {}", describe_access_error(&error)));
    let _ = fs::remove_file(&probe);
    written
}

fn check_secret_store() -> DoctorCheck {
    let name = constants::doctor::CHECK_SECRET_STORE.to_string();
    match vapor_platform::open_store(vapor_platform::SecretAccess::Interactive) {
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

/// Which OAuth client a Google Drive sign-in would present. Names the
/// client id (public by design) so a build can be checked against the
/// console; never the secret.
fn check_gdrive_oauth_client(
    client: Option<vapor_providers::gdrive::oauth::ClientCredentials>,
) -> DoctorCheck {
    use vapor_providers::gdrive::oauth::ClientSource;
    let name = constants::doctor::CHECK_GDRIVE_OAUTH_CLIENT.to_string();
    match client {
        Some(client) => {
            let source = match client.source {
                ClientSource::BuiltIn => "built into this build".to_string(),
                ClientSource::Environment => format!(
                    "from {} (overrides any built-in client)",
                    constants::env::VAPOR_GDRIVE_CLIENT_ID
                ),
            };
            DoctorCheck {
                name,
                status: DoctorCheckStatus::Ok,
                detail: format!(
                    "{source}: {}{}",
                    client.client_id,
                    if client.client_secret.is_some() {
                        " (with a client secret)"
                    } else {
                        ""
                    }
                ),
            }
        }
        None => DoctorCheck {
            name,
            status: DoctorCheckStatus::Warning,
            detail: format!(
                "none: this build has no Google Drive OAuth client and {} is not set, so Google Drive sign-in is unavailable",
                constants::env::VAPOR_GDRIVE_CLIENT_ID
            ),
        },
    }
}

/// Where the daemon's throttle inputs would come from if started here.
fn check_throttle_inputs(override_value: Option<String>) -> DoctorCheck {
    let name = constants::doctor::CHECK_THROTTLE_INPUTS.to_string();
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
    let name = constants::doctor::CHECK_LAUNCH_AGENT.to_string();
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
    let name = constants::doctor::CHECK_SYSTEMD_UNIT.to_string();
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

    fn load(contents: serde_json::Value) -> (TempDir, vapor_shared::config::VaporConfigLoadResult) {
        let temp = TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        fs::write(&path, contents.to_string()).expect("seed config");
        let load = vapor_shared::config::load_from(&path);
        (temp, load)
    }

    #[test]
    fn an_install_with_no_profile_is_told_which_command_sets_one_up() {
        let (_temp, load) = load(serde_json::json!({}));
        let checks = check_profiles(&load, None);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "profiles");
        assert_eq!(checks[0].status, DoctorCheckStatus::Warning);
        assert!(checks[0].detail.contains("vapor init"));
    }

    #[test]
    fn each_profile_gets_a_check_of_its_roots_and_its_sign_in() {
        let temp = TempDir::new().expect("temp");
        let local = temp.path().join("local");
        let cloud = temp.path().join("cloud");
        fs::create_dir_all(&local).expect("local");
        fs::create_dir_all(&cloud).expect("cloud");
        let (_config, load) = load(serde_json::json!({
            "profiles": [
                { "id": "files", "localRoot": local, "cloudRoot": cloud },
                { "id": "drive", "provider": "gdrive", "localRoot": temp.path().join("d"),
                  "cloudRoot": "/Vapor" },
                { "id": "typo", "localRoot": local, "cloudRoot": cloud, "syncMode": "mirror" },
            ]
        }));
        let store = vapor_platform::InMemorySecretStore::new();
        let checks = check_profiles(&load, Some(&store));
        let by_name = |name: &str| {
            checks
                .iter()
                .find(|check| check.name == name)
                .unwrap_or_else(|| panic!("{name} in {checks:?}"))
        };
        let files = by_name("profile:files");
        assert_eq!(files.status, DoctorCheckStatus::Ok, "{}", files.detail);
        assert!(files.detail.contains("reads and writes"));
        assert!(
            fs::read_dir(&local).expect("list").next().is_none(),
            "the write probe leaves nothing behind"
        );
        let drive = by_name("profile:drive");
        assert_eq!(drive.status, DoctorCheckStatus::Warning);
        assert!(drive.detail.contains("vapor auth login --profile drive"));
        let typo = by_name("profile:typo");
        assert_eq!(typo.status, DoctorCheckStatus::Failure);
        assert!(typo.detail.contains("\"mirror\""), "{}", typo.detail);
    }

    #[cfg(unix)]
    #[test]
    fn a_root_that_cannot_be_read_names_where_to_allow_it() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new().expect("temp");
        let local = temp.path().join("local");
        let cloud = temp.path().join("cloud");
        fs::create_dir_all(&local).expect("local");
        fs::create_dir_all(&cloud).expect("cloud");
        let (_config, load) = load(serde_json::json!({ "localRoot": local, "cloudRoot": cloud }));
        fs::set_permissions(&local, fs::Permissions::from_mode(0o000)).expect("lock");
        let readable_anyway = fs::read_dir(&local).is_ok();
        let checks = check_profiles(&load, None);
        fs::set_permissions(&local, fs::Permissions::from_mode(0o755)).expect("unlock");
        if readable_anyway {
            return;
        }
        let check = &checks[0];
        assert_eq!(check.name, "profile:default");
        assert_eq!(check.status, DoctorCheckStatus::Failure);
        assert!(
            check.detail.contains("permission denied")
                && check.detail.contains("Privacy & Security"),
            "{}",
            check.detail
        );
    }

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
    fn gdrive_client_check_names_the_source_and_never_the_secret() {
        use vapor_providers::gdrive::oauth::{ClientCredentials, ClientSource};
        let built_in = check_gdrive_oauth_client(Some(ClientCredentials {
            client_id: "123.apps.googleusercontent.com".to_string(),
            client_secret: Some("GOCSPX-hidden".to_string()),
            source: ClientSource::BuiltIn,
        }));
        assert_eq!(built_in.status, DoctorCheckStatus::Ok);
        assert!(built_in.detail.contains("built into this build"));
        assert!(built_in.detail.contains("123.apps.googleusercontent.com"));
        assert!(!built_in.detail.contains("GOCSPX-hidden"));

        let missing = check_gdrive_oauth_client(None);
        assert_eq!(missing.status, DoctorCheckStatus::Warning);
        assert!(
            missing
                .detail
                .contains(constants::env::VAPOR_GDRIVE_CLIENT_ID)
        );
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
        assert_eq!(json["worstStatus"], "warning");
        assert_eq!(json["checks"][0]["name"], "a");
        assert_eq!(json["checks"][0]["status"], "ok");
        assert_eq!(json["checks"][1]["detail"], "hmm");
        assert_eq!(
            json.as_object().expect("object").keys().collect::<Vec<_>>(),
            vec!["checks", "worstStatus"]
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
