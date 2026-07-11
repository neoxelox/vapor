//! `vapor version` / `vapor --version`.
//!
//! Prints the same shape as `vapord --version`:
//! `vapor <semver> (<git-commit-short>)`. The build-info constants come
//! from the daemon crate's `build.rs` so every binary in the workspace
//! agrees on the published version.

pub fn version_string() -> String {
    format!(
        "vapor {} ({})",
        vapor_daemon::build_info::VERSION,
        vapor_daemon::build_info::GIT_COMMIT_SHORT
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_starts_with_program_name_and_contains_version() {
        let s = version_string();
        assert!(s.starts_with("vapor "));
        assert!(s.contains(vapor_daemon::build_info::VERSION));
    }
}
