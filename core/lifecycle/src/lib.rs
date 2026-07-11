//! Daemon lifecycle orchestration for Vapor.
//!
//! Owns the cross-platform crash-loop guard, the lifecycle manager, and
//! the durable lifecycle state that the macOS / Windows / Linux app
//! surfaces and the `vapor` CLI all consume. The prior duplicate Swift
//! implementation was retired: the macOS app now drives
//! this crate through `vapor service … --json` subprocess calls, so
//! every surface shares one truth — including crash-loop backoff and
//! pause, which persist in `<vapor_dir>/state/lifecycle.json` and
//! survive process restarts.
//!
//! See `docs/plans/core.md §2.3`.

#![forbid(unsafe_code)]

mod auto_launch;
mod crash_loop;
mod durable;
mod manager;

pub use auto_launch::{
    AutoLaunchSettingStore, InMemoryAutoLaunchSettingStore, JsonFileAutoLaunchSettingStore,
    JsonFileError,
};
pub use crash_loop::{CrashLoopDecision, CrashLoopGuard, CrashLoopPolicy};
pub use durable::{
    FixedWallClock, InMemoryLifecycleStateStore, JsonFileLifecycleStateStore,
    LIFECYCLE_STATE_SCHEMA_VERSION, LifecycleStateStore, PersistedLifecycleState, SystemWallClock,
    WallClock,
};
pub use manager::{
    CrashLoopStateSnapshot, DaemonHealthCheckOutcome, DaemonLifecycleActionResult,
    DaemonLifecycleError, DaemonLifecycleManager,
};
