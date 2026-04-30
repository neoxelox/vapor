//! Per-command logic for the `vapor` CLI.
//!
//! Each submodule owns one command. Commands take typed arguments
//! (parsed by `clap` in `main.rs`) and return either a strongly-typed
//! value the binary can render, or an error type the binary translates
//! into a non-zero exit code.

pub mod config;
pub mod doctor;
pub mod run;
pub mod service;
pub mod version;

pub use config::{ConfigCommand, ConfigError};
pub use doctor::{DoctorCheck, DoctorCheckStatus, DoctorReport};
pub use run::{RunError, RunOptions};
pub use service::{ServiceCommand, ServiceCommandError, ServiceStatusReport};
pub use version::version_string;
