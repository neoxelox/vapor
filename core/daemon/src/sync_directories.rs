use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};
use vapor_shared::constants;

use crate::logging;

pub fn resolve_from_process_environment() -> Vec<PathBuf> {
    let configured = env::var(constants::env::VAPOR_SYNC_DIRECTORIES).ok();
    let current_directory = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home_directory = home_directory();
    resolve_directories(
        configured.as_deref(),
        &current_directory,
        home_directory.as_deref(),
    )
}

fn resolve_directories(
    configured: Option<&str>,
    current_directory: &Path,
    home_directory: Option<&Path>,
) -> Vec<PathBuf> {
    let raw_directories = match configured {
        Some(raw) => parse_directory_lines(raw),
        None => constants::filtering::DEFAULT_SYNC_DIRECTORIES
            .iter()
            .map(|path| (*path).to_string())
            .collect(),
    };

    let mut seen = HashSet::new();
    let mut resolved = Vec::new();
    for raw_directory in raw_directories {
        let Some(path) = resolve_path(raw_directory.as_str(), current_directory, home_directory)
        else {
            continue;
        };

        if !path.exists() {
            logging::warning(
                "Skipping sync directory because it does not exist",
                &[("path", path.display().to_string())],
            );
            continue;
        }

        if !path.is_dir() {
            logging::warning(
                "Skipping sync directory because path is not a directory",
                &[("path", path.display().to_string())],
            );
            continue;
        }

        let key = path.to_string_lossy().to_string();
        if seen.insert(key) {
            resolved.push(path);
        }
    }

    if resolved.is_empty() {
        logging::warning(
            "No valid sync directories resolved; daemon will remain idle",
            &[],
        );
    }

    resolved
}

fn parse_directory_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn resolve_path(
    raw: &str,
    current_directory: &Path,
    home_directory: Option<&Path>,
) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed == "~" {
        return home_directory.map(Path::to_path_buf);
    }

    if let Some(suffix) = trimmed.strip_prefix("~/") {
        return home_directory.map(|home| home.join(suffix));
    }

    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        return Some(path);
    }

    Some(current_directory.join(path))
}

fn home_directory() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn defaults_to_home_vapor_directory() {
        let home = create_test_directory();
        fs::create_dir_all(home.join("Vapor")).expect("failed to create default Vapor directory");

        let resolved = resolve_directories(None, Path::new("/tmp"), Some(home.as_path()));
        assert_eq!(resolved, vec![home.join("Vapor")]);

        remove_test_directory(&home);
    }

    #[test]
    fn skips_missing_directories() {
        let root = create_test_directory();
        let existing = root.join("existing");
        fs::create_dir_all(&existing).expect("failed to create existing directory");

        let configured = format!("{}\n{}", existing.display(), root.join("missing").display());
        let resolved = resolve_directories(Some(configured.as_str()), Path::new("/tmp"), None);
        assert_eq!(resolved, vec![existing]);

        remove_test_directory(&root);
    }

    #[test]
    fn resolves_relative_paths_against_current_directory() {
        let root = create_test_directory();
        let projects = root.join("projects");
        fs::create_dir_all(&projects).expect("failed to create projects directory");

        let resolved = resolve_directories(Some("projects"), &root, None);
        assert_eq!(resolved, vec![projects]);

        remove_test_directory(&root);
    }

    #[test]
    fn deduplicates_same_directory_entries() {
        let root = create_test_directory();
        let sync = root.join("sync");
        fs::create_dir_all(&sync).expect("failed to create sync directory");

        let configured = format!("{0}\n{0}", sync.display());
        let resolved = resolve_directories(Some(configured.as_str()), Path::new("/tmp"), None);
        assert_eq!(resolved, vec![sync]);

        remove_test_directory(&root);
    }

    fn create_test_directory() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "vapor-daemon-sync-directories-{}-{}",
            std::process::id(),
            timestamp
        ));
        fs::create_dir_all(&root).expect("failed to create test directory");
        root
    }

    fn remove_test_directory(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }
}
