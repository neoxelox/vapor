//! Platform abstractions for the Vapor runtime.
//!
//! Each module under this crate owns one trait that the engine
//! (`core/daemon`, `core/lifecycle`, `core/cli`) consumes instead of
//! reaching for OS APIs directly. Per-OS native implementations live in
//! sibling modules (e.g., `fs_watch::macos`); cross-OS in-memory fakes
//! live alongside (`fs_watch::fake`).
//!
//! Selection is compile-time via `#[cfg(target_os = "...")]` so there is
//! zero runtime dispatch cost. Per the design rules in
//! `docs/architecture/platform-abstractions.md`, every trait must:
//!
//! 1. Have an in-memory fake usable on any host.
//! 2. Have a native implementation on every shipping OS (macOS today).
//! 3. Stay byte-for-byte semantically identical across native impls
//!    (parity tests guarantee this once they land).
//!
//! Every trait below ships with a macOS-native implementation seam.
//! Windows / Linux native impls are intentionally `unimplemented!()`
//! until those platforms become shipping surfaces.
// Allow tightly-scoped `unsafe` for OS FFI calls (e.g. `libc::getuid()`).
// Each `unsafe` block must explain why it is sound. Engine code in
// `core/daemon`, `core/shared`, etc. continues to `forbid(unsafe_code)`.
#![deny(unsafe_code)]

pub mod fs_caps;
pub mod fs_ops;
pub mod fs_watch;
pub mod idle;
pub mod metrics;
pub mod process;
pub mod secrets;
pub mod service;

pub use fs_caps::{
    CaseSensitivity, FilesystemCapabilities, InMemoryFilesystemCapabilities,
    NativeFilesystemCapabilities,
};
pub use fs_watch::{
    FsWatcher, FsWatcherError, InMemoryFsWatcher, NativeFsWatcher, WatchEvent, WatchEventKind,
};
pub use idle::{
    AlwaysIdleNotifier, IdleNotifier, ManualIdleNotifier, NativeIdleNotifier, UserActivity,
};
pub use metrics::{
    InMemoryPlatformMetricsSampler, NativePlatformMetricsSampler, PlatformMetricsSampler,
    StaticPlatformMetricsSampler, ThrottleInputs,
};
pub use process::{NativeProcessSupervisor, NoopProcessSupervisor, ProcessSupervisor};
pub use secrets::{InMemorySecretStore, NativeSecretStore, SecretStore, SecretStoreError};
pub use service::{
    InMemoryServiceInstaller, NativeServiceInstaller, ServiceDescriptor, ServiceInstallError,
    ServiceInstaller, ServiceStatus,
};
