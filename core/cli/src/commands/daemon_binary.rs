//! Finds the `vapord` executable the CLI should drive.
//!
//! One resolver for every command that needs the daemon path (`service
//! install`, `doctor`), so the bundled layout can never be recognised by
//! one and missed by another. Search order: sibling of the CLI (build
//! directory), the app bundle (`Contents/Helpers/vapor` next to
//! `Contents/MacOS/vapord`), then `PATH`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use vapor_shared::constants;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonBinarySource {
    /// Next to the running CLI binary.
    Sibling,
    /// `Contents/MacOS/vapord` inside the app bundle that holds the CLI.
    Bundled,
    /// A directory on `PATH`.
    SearchPath,
}

impl DaemonBinarySource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sibling => "sibling of this CLI",
            Self::Bundled => "bundled in the app",
            Self::SearchPath => "on PATH",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonBinary {
    pub path: PathBuf,
    pub source: DaemonBinarySource,
}

pub fn locate() -> Option<DaemonBinary> {
    let current = std::env::current_exe().ok();
    locate_from(current.as_deref(), std::env::var_os("PATH").as_deref())
}

/// Pure resolver over an explicit CLI path and `PATH` value.
pub fn locate_from(cli_path: Option<&Path>, search_path: Option<&OsStr>) -> Option<DaemonBinary> {
    let name = format!(
        "{}{}",
        constants::runtime::DAEMON_BINARY_NAME,
        std::env::consts::EXE_SUFFIX
    );
    if let Some(parent) = cli_path.and_then(Path::parent) {
        let sibling = parent.join(&name);
        if sibling.is_file() {
            return Some(DaemonBinary {
                path: sibling,
                source: DaemonBinarySource::Sibling,
            });
        }
        if let Some(contents) = parent.parent() {
            let bundled = contents.join("MacOS").join(&name);
            if bundled.is_file() {
                return Some(DaemonBinary {
                    path: bundled,
                    source: DaemonBinarySource::Bundled,
                });
            }
        }
    }
    let search_path = search_path?;
    std::env::split_paths(search_path)
        .map(|dir| dir.join(&name))
        .find(|candidate| candidate.is_file())
        .map(|path| DaemonBinary {
            path,
            source: DaemonBinarySource::SearchPath,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, b"").expect("write");
    }

    fn daemon_name() -> String {
        format!(
            "{}{}",
            constants::runtime::DAEMON_BINARY_NAME,
            std::env::consts::EXE_SUFFIX
        )
    }

    #[test]
    fn prefers_the_sibling_over_bundle_and_path() {
        let temp = TempDir::new().expect("temp");
        let cli = temp.path().join("bin").join("vapor");
        touch(&cli);
        let sibling = temp.path().join("bin").join(daemon_name());
        touch(&sibling);
        let on_path = temp.path().join("path").join(daemon_name());
        touch(&on_path);
        let found =
            locate_from(Some(&cli), Some(temp.path().join("path").as_os_str())).expect("found");
        assert_eq!(found.source, DaemonBinarySource::Sibling);
        assert_eq!(found.path, sibling);
    }

    #[test]
    fn finds_the_bundled_daemon_from_the_helpers_directory() {
        let temp = TempDir::new().expect("temp");
        let contents = temp.path().join("Vapor.app").join("Contents");
        let cli = contents.join("Helpers").join("vapor");
        touch(&cli);
        let daemon = contents.join("MacOS").join(daemon_name());
        touch(&daemon);
        let found = locate_from(Some(&cli), None).expect("found");
        assert_eq!(found.source, DaemonBinarySource::Bundled);
        assert_eq!(found.path, daemon);
    }

    #[test]
    fn falls_back_to_path_and_reports_absence() {
        let temp = TempDir::new().expect("temp");
        let cli = temp.path().join("bin").join("vapor");
        touch(&cli);
        let on_path = temp.path().join("path").join(daemon_name());
        touch(&on_path);
        let search = std::env::join_paths([temp.path().join("empty"), temp.path().join("path")])
            .expect("join paths");
        let found = locate_from(Some(&cli), Some(&search)).expect("found");
        assert_eq!(found.source, DaemonBinarySource::SearchPath);
        assert_eq!(found.path, on_path);
        assert!(locate_from(Some(&cli), Some(temp.path().join("empty").as_os_str())).is_none());
        assert!(locate_from(None, None).is_none());
    }
}
