//! `vapor` CLI — headless-first control plane that consumes the
//! portable Rust runtime.
//!
//! The library half exposes the per-command logic so unit tests can
//! exercise it without spawning the binary. `src/main.rs` wires `clap`
//! to these entry points and exits with the right status code.
//!
//! See `docs/plans/cli.md` and `docs/tasks/cli.md`.

#![forbid(unsafe_code)]

use std::path::PathBuf;

pub mod commands;

pub use commands::{
    ConfigCommand, DoctorReport, RunOptions, ServiceCommand, ServiceCommandError, config, doctor,
    run, service, version, version_string,
};

/// Resolves the `vapor_dir` consumed by every CLI command. Mirrors the
/// daemon-side resolution rules (see
/// `core/shared/src/runtime_paths.rs`) but returns the path explicitly
/// so commands can log it.
pub fn resolve_vapor_directory() -> PathBuf {
    vapor_shared::runtime_paths::vapor_directory()
}

/// Resolves the canonical `vapor.json` path under the current
/// `vapor_dir`.
pub fn resolve_configuration_path() -> PathBuf {
    let vapor_dir = resolve_vapor_directory();
    vapor_dir.join(vapor_shared::constants::runtime::CONFIGURATION_FILE_NAME)
}
