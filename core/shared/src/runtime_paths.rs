use std::env;
use std::fs;
use std::path::PathBuf;

use crate::constants;
use crate::paths;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

pub fn vapor_directory() -> PathBuf {
    if let Some(configured) = env::var_os(constants::env::VAPOR_DIR)
        && let Some(path) = normalize_override_path(PathBuf::from(configured))
    {
        return path;
    }

    // Dev/CI default (`./.vapor`) selection. An explicit `VAPOR_ENV`
    // wins over the CI heuristic (so `VAPOR_ENV=prod` uses `~/.vapor`
    // even under CI), and `CI` is honored only when *truthy* — a leaked
    // or explicitly-false `CI` (e.g. `CI=false` in a shell) must not
    // silently redirect `vapor_dir` to a cwd-relative path.
    let vapor_env = env::var(constants::env::VAPOR_ENV)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase());
    let use_dev_directory = match vapor_env.as_deref() {
        Some("dev") => true,
        Some("prod") => false,
        _ => ci_is_truthy(),
    };
    if use_dev_directory && let Ok(current_directory) = env::current_dir() {
        return current_directory.join(constants::runtime::VAPOR_DIRECTORY_NAME);
    }

    if let Some(home) = home_directory() {
        return home.join(constants::runtime::VAPOR_DIRECTORY_NAME);
    }

    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(constants::runtime::VAPOR_DIRECTORY_NAME)
}

/// Whether the `CI` environment variable is set to a truthy value
/// (`true`/`1`, case-insensitive). Mere presence is not enough — many
/// tools set `CI=false`, which must not trigger the dev directory.
fn ci_is_truthy() -> bool {
    env::var("CI")
        .map(|value| {
            let value = value.trim();
            value.eq_ignore_ascii_case("true") || value == "1"
        })
        .unwrap_or(false)
}

/// Runs `body` while holding an exclusive advisory lock on a sidecar
/// (`<config_path>.lock`), serializing `vapor.json` read-modify-write
/// across every surface (CLI, daemon, app) and process. Without it two
/// writers can each read the same document and the last rename silently
/// drops the other's change. The lock releases when `body` returns.
pub fn with_config_lock<T, E>(
    config_path: &std::path::Path,
    body: impl FnOnce() -> Result<T, E>,
) -> Result<T, E>
where
    E: From<std::io::Error>,
{
    let mut lock_path = config_path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    if let Some(parent) = lock_path.parent() {
        ensure_private_directory(parent).map_err(E::from)?;
    }
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(E::from)?;
    // Blocking exclusive advisory lock (flock on Unix, LockFileEx on
    // Windows). Released when `lock_file` drops at the end of this fn.
    lock_file.lock().map_err(E::from)?;
    body()
}

/// A unique temp path next to `target` for an atomic write, so concurrent
/// writers never collide on one shared staging filename.
pub fn unique_temp_path(target: &std::path::Path) -> PathBuf {
    let mut name = target.as_os_str().to_owned();
    name.push(format!(".vapor-tmp-{}", std::process::id()));
    PathBuf::from(name)
}

pub fn logs_directory() -> PathBuf {
    vapor_directory().join(constants::runtime::LOGS_DIRECTORY_NAME)
}

/// Where the daemon binds its IPC socket and the CLI dials it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IpcSocketLocation {
    /// The path both sides use.
    pub path: PathBuf,
    /// Set when the canonical `<vapor_dir>/vapord.sock` exceeded the
    /// socket-address byte budget and the path was relocated. Holds the
    /// canonical path that was passed over, for logs and diagnostics.
    pub relocated_from: Option<PathBuf>,
}

/// Resolve the IPC socket location for the current `vapor_dir`.
///
/// Unix sockets carry a hard address-length cap (`sun_path`: 104 bytes
/// on macOS/BSD, 108 on Linux). The canonical location is
/// `<vapor_dir>/vapord.sock`, but with a deep `vapor_dir` binding it
/// fails outright — pre-fix the daemon degraded to running without its
/// IPC endpoint while `vapor status` reported it as not running. When
/// the canonical path exceeds [`constants::ipc::MAX_SOCKET_PATH_BYTES`],
/// both sides deterministically relocate to
/// `<os-temp>/vapor-<fnv1a64(vapor_dir)>/vapord.sock`: same `vapor_dir`
/// → same socket path in every process, distinct `vapor_dir`s cannot
/// collide, and the daemon's one-socket-per-`vapor_dir` invariant is
/// preserved.
pub fn ipc_socket_location() -> IpcSocketLocation {
    resolve_ipc_socket_location(&vapor_directory(), &env::temp_dir())
}

fn resolve_ipc_socket_location(
    vapor_dir: &std::path::Path,
    temp_dir: &std::path::Path,
) -> IpcSocketLocation {
    let canonical = vapor_dir.join(constants::ipc::SOCKET_FILE_NAME);
    if canonical.as_os_str().len() <= constants::ipc::MAX_SOCKET_PATH_BYTES {
        return IpcSocketLocation {
            path: canonical,
            relocated_from: None,
        };
    }
    let hash = fnv1a64(vapor_dir.as_os_str().as_encoded_bytes());
    IpcSocketLocation {
        path: temp_dir
            .join(format!("vapor-{hash:016x}"))
            .join(constants::ipc::SOCKET_FILE_NAME),
        relocated_from: Some(canonical),
    }
}

/// FNV-1a 64-bit. Implemented here (six lines) instead of relying on
/// `DefaultHasher` because the CLI and daemon are separate binaries that
/// may be built by different compiler versions during upgrade skew, and
/// `DefaultHasher`'s algorithm is explicitly not guaranteed stable. The
/// socket rendezvous only works if every build hashes identically.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub fn state_directory() -> PathBuf {
    vapor_directory().join(constants::runtime::STATE_DIRECTORY_NAME)
}

pub fn sqlite_database_path() -> PathBuf {
    state_directory().join(constants::runtime::SQLITE_DATABASE_FILE_NAME)
}

/// Durable state DB for a named profile. The implicit
/// `default` profile keeps the legacy single-scope path so existing
/// state carries forward without migration; every other profile gets an
/// isolated database under `state/profiles/<id>/`.
pub fn profile_database_path(profile_id: &str) -> PathBuf {
    if profile_id == crate::constants::profile::DEFAULT_PROFILE_ID {
        return sqlite_database_path();
    }
    state_directory()
        .join("profiles")
        .join(profile_id)
        .join(crate::constants::runtime::SQLITE_DATABASE_FILE_NAME)
}

/// Durable daemon-lifecycle side-file (crash-loop bookkeeping +
/// supervision expectations). Lives under `vapor_dir/state/` per the
/// runtime data directory policy (AGENTS.md §8.5).
pub fn lifecycle_state_path() -> PathBuf {
    state_directory().join(constants::runtime::LIFECYCLE_STATE_FILE_NAME)
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

    let path = expand_tilde(path)?;

    let candidate = if path.is_absolute() {
        path
    } else {
        env::current_dir().ok()?.join(path)
    };

    // Resolve through the filesystem so equivalent spellings of the
    // same directory (symlinked vs real, `..` crossing a symlink)
    // converge on one canonical path. This is not cosmetic: the IPC
    // socket relocation hashes the vapor_dir spelling, so a daemon
    // launched with one spelling (say, a launchd plist) and a CLI with
    // another would compute different socket paths and never find each
    // other.
    if let Ok(canonical) = paths::canonicalize(&candidate) {
        return Some(canonical);
    }

    // The directory does not (fully) exist yet: normalize lexically,
    // then canonicalize the deepest existing ancestor so the directory,
    // once created, still lands under the canonical spelling. A `..`
    // crossing a symlinked component inside the not-yet-existing tail
    // resolves lexically here — the one case where a consistent
    // VAPOR_DIR spelling across surfaces still matters.
    let lexical = normalize_absolute_path(candidate)?;
    Some(canonicalize_deepest_existing_ancestor(lexical))
}

/// Canonicalizes the deepest existing ancestor of `path` (which must
/// already be lexically normalized — no `.` / `..` components) and
/// re-appends the missing tail. Falls back to the input when nothing
/// on the path can be canonicalized.
fn canonicalize_deepest_existing_ancestor(path: PathBuf) -> PathBuf {
    let mut missing_tail: Vec<std::ffi::OsString> = Vec::new();
    let mut ancestor = path.as_path();
    loop {
        if let Ok(canonical) = paths::canonicalize(ancestor) {
            let mut resolved = canonical;
            for name in missing_tail.iter().rev() {
                resolved.push(name);
            }
            return resolved;
        }
        match (ancestor.parent(), ancestor.file_name()) {
            (Some(parent), Some(name)) => {
                missing_tail.push(name.to_os_string());
                ancestor = parent;
            }
            _ => return path,
        }
    }
}

/// Expands a leading `~` / `~/…` against the home directory so
/// `VAPOR_DIR=~/vapor-dir` behaves identically on every surface (the
/// Swift app expands tildes too). Without this, launchd-style contexts
/// that don't go through a shell would resolve `~/x` to a literal `./~/x`
/// directory. Returns `None` when a tilde is present but no home
/// directory can be resolved.
fn expand_tilde(path: PathBuf) -> Option<PathBuf> {
    let Some(text) = path.to_str() else {
        return Some(path);
    };

    if text == "~" {
        return home_directory();
    }

    if let Some(suffix) = text.strip_prefix("~/") {
        return Some(home_directory()?.join(suffix));
    }

    Some(path)
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
    fn config_lock_serializes_concurrent_read_modify_write() {
        // Each thread runs read-count → increment → write under the lock. If
        // the lock did not actually serialize the cycle, interleaved readers
        // would share a base value and the last writer would clobber the
        // others, leaving a final count below the thread count.
        let dir = TempDir::new().expect("tempdir");
        let path = std::sync::Arc::new(dir.path().join("vapor.json"));
        fs::write(path.as_path(), b"{\"count\":0}\n").expect("seed");

        const THREADS: u64 = 12;
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    with_config_lock::<(), std::io::Error>(&path, || {
                        let text = fs::read_to_string(path.as_path())?;
                        let mut doc: serde_json::Value = serde_json::from_str(&text).unwrap();
                        let current = doc["count"].as_u64().unwrap();
                        doc["count"] = serde_json::json!(current + 1);
                        let tmp = unique_temp_path(&path);
                        fs::write(&tmp, format!("{doc}\n").as_bytes())?;
                        fs::rename(&tmp, path.as_path())?;
                        Ok(())
                    })
                    .expect("locked write");
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("thread joins");
        }

        let final_text = fs::read_to_string(path.as_path()).expect("read back");
        let doc: serde_json::Value = serde_json::from_str(&final_text).expect("parse");
        assert_eq!(doc["count"].as_u64(), Some(THREADS));
    }

    #[test]
    fn unique_temp_path_sits_next_to_the_target() {
        let target = PathBuf::from("/tmp/vapor/vapor.json");
        let temp = unique_temp_path(&target);
        assert_eq!(temp.parent(), target.parent());
        assert!(
            temp.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("vapor.json.vapor-tmp-"),
            "temp name was {temp:?}"
        );
    }

    #[test]
    fn tilde_override_expands_against_home_directory() {
        // `expand_tilde` reads the real HOME/USERPROFILE; assert only the
        // structural property (prefix replaced) so the test is hermetic.
        let Some(home) = home_directory() else {
            return;
        };
        let expanded =
            normalize_override_path(PathBuf::from("~/vapor-tilde-test")).expect("expanded");
        assert_eq!(expanded, home.join("vapor-tilde-test"));

        let bare = normalize_override_path(PathBuf::from("~")).expect("expanded bare tilde");
        assert_eq!(bare, home);
    }

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
    fn symlinked_and_real_spellings_of_the_vapor_dir_converge() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical temp");
        let real = base.join("real-vapor-dir");
        fs::create_dir_all(&real).expect("real dir");
        symlink(&real, base.join("vapor-link")).expect("symlink");

        // Both spellings must resolve to the same path, else the daemon
        // and CLI would hash different socket relocations.
        let via_link = normalize_override_path(base.join("vapor-link")).expect("via link");
        let via_real = normalize_override_path(real.clone()).expect("via real");
        assert_eq!(via_link, via_real);

        // A not-yet-existing directory under a symlinked ancestor also
        // lands under the canonical spelling.
        let nested = normalize_override_path(base.join("vapor-link/nested/.vapor"))
            .expect("nested under link");
        assert_eq!(nested, real.join("nested/.vapor"));
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
    fn ipc_socket_stays_canonical_when_within_the_address_budget() {
        let vapor_dir = PathBuf::from("/Users/alex/.vapor");
        let location = resolve_ipc_socket_location(&vapor_dir, &PathBuf::from("/tmp"));
        assert_eq!(location.path, vapor_dir.join("vapord.sock"));
        assert_eq!(location.relocated_from, None);
    }

    #[test]
    fn ipc_socket_relocation_triggers_exactly_past_the_budget() {
        // Build a vapor_dir whose canonical socket path is exactly at
        // the budget, then one byte past it.
        let file_len = constants::ipc::SOCKET_FILE_NAME.len() + 1; // "/vapord.sock"
        let at_budget = PathBuf::from(format!(
            "/{}",
            "d".repeat(constants::ipc::MAX_SOCKET_PATH_BYTES - file_len - 1)
        ));
        let over_budget = PathBuf::from(format!(
            "/{}",
            "d".repeat(constants::ipc::MAX_SOCKET_PATH_BYTES - file_len)
        ));
        let temp = PathBuf::from("/tmp");

        let at = resolve_ipc_socket_location(&at_budget, &temp);
        assert_eq!(at.relocated_from, None, "at-budget path must not relocate");

        let over = resolve_ipc_socket_location(&over_budget, &temp);
        assert_eq!(
            over.relocated_from,
            Some(over_budget.join("vapord.sock")),
            "over-budget path must relocate and report the canonical path"
        );
        assert!(
            over.path.as_os_str().len() <= constants::ipc::MAX_SOCKET_PATH_BYTES,
            "relocated path must itself fit the budget: {}",
            over.path.display()
        );
        assert!(over.path.starts_with(&temp));
        assert!(over.path.ends_with("vapord.sock"));
    }

    #[test]
    fn ipc_socket_relocation_is_deterministic_and_collision_free() {
        let temp = PathBuf::from("/tmp");
        let deep_a = PathBuf::from(format!("/{}/a", "d".repeat(120)));
        let deep_b = PathBuf::from(format!("/{}/b", "d".repeat(120)));

        let first = resolve_ipc_socket_location(&deep_a, &temp);
        let second = resolve_ipc_socket_location(&deep_a, &temp);
        assert_eq!(
            first.path, second.path,
            "same vapor_dir must resolve to the same socket in every process"
        );

        let other = resolve_ipc_socket_location(&deep_b, &temp);
        assert_ne!(
            first.path, other.path,
            "distinct vapor_dirs must not share a socket"
        );
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
