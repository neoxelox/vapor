//! Per-command logic for the `vapor` CLI.
//!
//! Each submodule owns one command. Commands take typed arguments
//! (parsed by `clap` in `main.rs`) and return either a strongly-typed
//! value the binary can render, or an error type the binary translates
//! into a non-zero exit code.

pub mod auth;
pub mod config;
pub mod conflicts;
pub mod daemon_binary;
pub mod decisions;
pub mod doctor;
pub mod ipc;
pub mod run;
pub mod service;
pub mod support;
pub mod version;

pub use auth::{AuthCommand, AuthError, AuthStatusEntry};
pub use config::{ConfigCommand, ConfigError};
pub use conflicts::{ConflictListReport, ConflictRecord, KeepSide, ResolutionReport};
pub use decisions::{DecisionJson, DecisionListReport};
pub use doctor::{DoctorCheck, DoctorCheckStatus, DoctorReport};
pub use ipc::IpcCliError;
pub use run::{RunError, RunOptions};
pub use service::{ServiceCommand, ServiceCommandError, ServiceStatusReport};
pub use support::{LiveCaptures, SupportBundleReport};
pub use version::version_string;
