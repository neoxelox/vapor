//! Daemon lifecycle orchestration for Vapor.
//!
//! Owns the cross-platform crash-loop guard and the lifecycle manager
//! that the macOS / Windows / Linux app surfaces and the `vapor` CLI all
//! consume. Replaces the prior Swift implementation in
//! `apps/macos/Sources/VaporCore/DaemonLifecycle.swift` so every surface
//! shares one truth.
//!
//! See `docs/plans/core.md §2.3` and `docs/tasks/core.md` Phase C4.
//!
//! Wave 5 ships the Rust types + parity tests. Wave 6 adds the `vapor
//! service install / start / stop / status` CLI commands that consume
//! [`DaemonLifecycleManager`] directly. Wave 5 / M2 then has the macOS
//! Swift app delegate to the CLI subprocess so the duplicate Swift code
//! retires.

#![forbid(unsafe_code)]

mod auto_launch;
mod crash_loop;
mod manager;

pub use auto_launch::{
    AutoLaunchSettingStore, InMemoryAutoLaunchSettingStore, JsonFileAutoLaunchSettingStore,
    JsonFileError,
};
pub use crash_loop::{CrashLoopDecision, CrashLoopGuard, CrashLoopPolicy};
pub use manager::{DaemonLifecycleActionResult, DaemonLifecycleError, DaemonLifecycleManager};
