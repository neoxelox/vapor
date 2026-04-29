use std::env;
use std::fs;
use std::path::PathBuf;

use crate::constants;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

pub fn vapor_directory() -> PathBuf {
    if let Some(configured) = env::var_os(constants::env::VAPOR_DIR)
        && let Some(path) = normalize_override_path(PathBuf::from(configured))
    {
        return path;
    }

    if (env::var_os("CI").is_some()
        || env::var(constants::env::VAPOR_ENV)
            .ok()
            .map(|value| value.eq_ignore_ascii_case("dev"))
            .unwrap_or(false))
        && let Ok(current_directory) = env::current_dir()
    {
        return current_directory.join(constants::runtime::VAPOR_DIRECTORY_NAME);
    }

    if let Some(home) = home_directory() {
        return home.join(constants::runtime::VAPOR_DIRECTORY_NAME);
    }

    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(constants::runtime::VAPOR_DIRECTORY_NAME)
}

pub fn logs_directory() -> PathBuf {
    vapor_directory().join(constants::runtime::LOGS_DIRECTORY_NAME)
}

pub fn state_directory() -> PathBuf {
    vapor_directory().join(constants::runtime::STATE_DIRECTORY_NAME)
}

pub fn sqlite_database_path() -> PathBuf {
    state_directory().join(constants::runtime::SQLITE_DATABASE_FILE_NAME)
}

/// Cross-platform home directory resolution.
///
/// Unix honors `HOME`. Windows honors `USERPROFILE` (preferred) and falls
/// back to `HOME` when set (e.g., MSYS2 / Git Bash sessions). The `vapor_dir`
/// runtime override (`VAPOR_DIR` env var) takes precedence and is handled
/// separately in [`vapor_directory`].
pub fn home_directory() -> Option<PathBuf> {
    let home = env::var_os("HOME");
    let user_profile = env::var_os("USERPROFILE");
    resolve_home_directory_from(home.as_deref(), user_profile.as_deref())
}

fn resolve_home_directory_from(
    home: Option<&std::ffi::OsStr>,
    user_profile: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    // Windows prefers `USERPROFILE` because git-bash / MSYS2 expose `HOME`
    // pointing at a POSIX-style mount that may not match real Windows ACLs.
    if cfg!(windows)
        && let Some(profile) = user_profile
        && !profile.is_empty()
    {
        return Some(PathBuf::from(profile));
    }

    if let Some(home) = home
        && !home.is_empty()
    {
        return Some(PathBuf::from(home));
    }

    if let Some(profile) = user_profile
        && !profile.is_empty()
    {
        return Some(PathBuf::from(profile));
    }

    None
}

#[cfg(unix)]
pub fn ensure_private_directory(path: &std::path::Path) -> std::io::Result<()> {
    fs::DirBuilder::new()
        .mode(constants::runtime::PRIVATE_DIRECTORY_MODE)
        .recursive(true)
        .create(path)?;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(constants::runtime::PRIVATE_DIRECTORY_MODE),
    )
}

#[cfg(not(unix))]
pub fn ensure_private_directory(path: &std::path::Path) -> std::io::Result<()> {
    // Windows lacks POSIX `0o700` semantics. Inherited NTFS DACLs already
    // restrict the per-user vapor directory to the current user; the engine
    // does not run elevated, so a child process from the same user is
    // intended access. A future hardening pass can tighten the DACL via
    // `windows-acl` (tracked under `core/platform`).
    fs::create_dir_all(path)
}

#[cfg(unix)]
pub fn ensure_private_file(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_directory(parent)?;
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(constants::runtime::PRIVATE_FILE_MODE)
        .open(path)?;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(constants::runtime::PRIVATE_FILE_MODE),
    )
}

#[cfg(not(unix))]
pub fn ensure_private_file(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_directory(parent)?;
    }
    // Inherited DACLs on Windows scope the file to the current user. See the
    // note on `ensure_private_directory` above for the per-OS hardening plan.
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map(|_| ())
}

fn normalize_override_path(path: PathBuf) -> Option<PathBuf> {
    if path.as_os_str().is_empty() {
        return None;
    }

    let candidate = if path.is_absolute() {
        path
    } else {
        env::current_dir().ok()?.join(path)
    };

    normalize_absolute_path(candidate)
}

fn normalize_absolute_path(path: PathBuf) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }

    // On Windows we keep the drive-letter / UNC prefix so absolute paths like
    // `C:\Users\alex\.vapor` survive normalization. `Component::Prefix` is
    // never produced on Unix, so the cross-platform branches are mutually
    // exclusive at runtime.
    let mut normalized = PathBuf::new();
    let mut has_root = false;
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => {
                normalized.push(prefix.as_os_str());
            }
            std::path::Component::RootDir => {
                normalized.push(component.as_os_str());
                has_root = true;
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }

    if !has_root && !cfg!(windows) {
        // On Unix the loop above must have observed a `RootDir` for an
        // absolute path; this is a defensive guard if `is_absolute()` ever
        // accepts something that does not start with `/`.
        return None;
    }

    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn relative_vapor_dir_override_is_normalized_against_current_directory() {
        let current_directory = env::current_dir().expect("current dir");
        let normalized = normalize_override_path(PathBuf::from("./nested/../.vapor-test"))
            .expect("normalized override");

        assert_eq!(normalized, current_directory.join(".vapor-test"));
    }

    #[test]
    fn absolute_override_with_parent_traversal_is_normalized() {
        // Use an OS-appropriate absolute path so the test runs the same on
        // every supported runner (no Unix-only `/` prefix).
        let mut absolute = env::current_dir().expect("current dir");
        absolute.push("a");
        absolute.push("..");
        absolute.push("b");

        let normalized = normalize_absolute_path(absolute).expect("normalized");
        assert_eq!(
            normalized,
            env::current_dir().expect("current dir").join("b")
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_directory_and_file_helpers_apply_restrictive_permissions() {
        let temp_dir = TempDir::new().expect("temp dir");
        let directory = temp_dir.path().join("runtime/state");
        let file = directory.join("vapor.sqlite");

        ensure_private_directory(&directory).expect("ensure private directory");
        ensure_private_file(&file).expect("ensure private file");

        let directory_mode = fs::metadata(&directory)
            .expect("directory metadata")
            .permissions()
            .mode()
            & 0o777;
        let file_mode = fs::metadata(&file)
            .expect("file metadata")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(directory_mode, constants::runtime::PRIVATE_DIRECTORY_MODE);
        assert_eq!(file_mode, constants::runtime::PRIVATE_FILE_MODE);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_directory_tightens_intermediate_components_on_create() {
        let temp_dir = TempDir::new().expect("temp dir");
        let intermediate = temp_dir.path().join("vapor-root");
        let target = intermediate.join("state");

        ensure_private_directory(&target).expect("ensure private directory with intermediates");

        let intermediate_mode = fs::metadata(&intermediate)
            .expect("intermediate directory metadata")
            .permissions()
            .mode()
            & 0o777;
        let target_mode = fs::metadata(&target)
            .expect("target directory metadata")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(
            intermediate_mode,
            constants::runtime::PRIVATE_DIRECTORY_MODE
        );
        assert_eq!(target_mode, constants::runtime::PRIVATE_DIRECTORY_MODE);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_file_creates_with_restrictive_mode_atomically() {
        let temp_dir = TempDir::new().expect("temp dir");
        let file = temp_dir.path().join("vapor-state/new-file.log");

        ensure_private_file(&file).expect("ensure private file from fresh creation");

        let file_mode = fs::metadata(&file)
            .expect("file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, constants::runtime::PRIVATE_FILE_MODE);
    }

    #[cfg(not(unix))]
    #[test]
    fn ensure_private_directory_creates_directories_without_unix_mode_assertions() {
        let temp_dir = TempDir::new().expect("temp dir");
        let target = temp_dir.path().join("runtime/state");

        ensure_private_directory(&target).expect("ensure private directory on non-unix");
        assert!(target.is_dir());
    }

    #[cfg(not(unix))]
    #[test]
    fn ensure_private_file_creates_file_without_unix_mode_assertions() {
        let temp_dir = TempDir::new().expect("temp dir");
        let file = temp_dir.path().join("vapor-state/new-file.log");

        ensure_private_file(&file).expect("ensure private file on non-unix");
        assert!(file.is_file());
    }

    #[test]
    fn home_resolution_returns_home_on_unix_when_only_home_is_set() {
        let resolved = resolve_home_directory_from(Some(std::ffi::OsStr::new("/home/alex")), None);
        assert_eq!(resolved, Some(PathBuf::from("/home/alex")));
    }

    #[test]
    fn home_resolution_returns_none_when_neither_env_var_is_set() {
        assert!(resolve_home_directory_from(None, None).is_none());
    }

    #[test]
    fn home_resolution_skips_empty_values() {
        let resolved = resolve_home_directory_from(
            Some(std::ffi::OsStr::new("")),
            Some(std::ffi::OsStr::new("")),
        );
        assert!(resolved.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn home_resolution_prefers_user_profile_on_windows() {
        let resolved = resolve_home_directory_from(
            Some(std::ffi::OsStr::new("/c/Users/alex")),
            Some(std::ffi::OsStr::new("C:\\Users\\alex")),
        );
        assert_eq!(resolved, Some(PathBuf::from("C:\\Users\\alex")));
    }

    #[cfg(windows)]
    #[test]
    fn home_resolution_falls_back_to_user_profile_when_home_is_empty_on_windows() {
        let resolved =
            resolve_home_directory_from(None, Some(std::ffi::OsStr::new("C:\\Users\\alex")));
        assert_eq!(resolved, Some(PathBuf::from("C:\\Users\\alex")));
    }
}
