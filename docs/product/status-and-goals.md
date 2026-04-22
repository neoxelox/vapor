# Product Status and Goals

## Project status

- Current stage: pre-GA (`v0.2.0-alpha.3`). Runtime Phases M0, M1, M1.5, M2, M2.5 (macOS-centric numbering in legacy task tracking) are complete. Going forward, runtime-level work is tracked in `docs/tasks/core.md`, macOS app work in `docs/tasks/macos.md`, and CLI work in `docs/tasks/cli.md`.
- Default provider is currently the inert `FilesystemStubProvider`; the real `provider_filesystem` ships in the Phase C8 bidirectional runtime shell and `GoogleDriveProvider` becomes selectable later in C8.
- Product direction: bidirectional eventual consistency between a user-selected local folder and a user-selected provider folder. macOS is the first shipping surface; the `vapor` CLI follows on every OS; Windows and Linux apps land on the same portable Rust runtime.
- Google Drive remains the first external cloud target but is intentionally deferred until the runtime, abstractions, and acceptance criteria are stable.
- Primary constraint: do no harm to user workload, battery, and thermal headroom — on every supported OS.
- Pre-GA compatibility policy: backward compatibility is not guaranteed yet; config/state/schema and local interfaces may change during active development.

## Product goals

- Keep one selected local folder (default `~/Vapor`) bidirectionally synced with one selected provider folder (default `/Vapor`) with durable intent state.
- Scope sync strictly to that configured folder pair; Vapor is not intended to be full-device backup.
- Stay low-impact during active development and heavy system load.
- Defer expensive work under pressure while maintaining eventual consistency.
- Provide transparent state, diagnostics, and user controls from the macOS app and menubar.
- Honor user-configured resource ceilings (`resourceLimits`) as hard caps, with optional `idleBoost` that raises ceilings only when the device is genuinely idle with measured headroom.
- Support multiple sync profiles (planned in Phase 5) so one device can target multiple provider accounts safely with per-profile overrides.
- Default to the `keep both copies` conflict policy with deterministic conflict-suffix paths (planned in Phase 4) so no edit is silently overwritten.

## Runtime model

- Auto-launch at login is ON by default.
- Throttle states govern all heavy work: `IdleDrain`, `Light`, `Throttled`, `Suspended`.
- User resource ceilings (`resourceLimits.cpuPercent`, `memoryPercent`, `bandwidthPercent`) and `idleBoost` (default-on dynamic headroom) layer over the throttle controller without ever relaxing it.
- Eventual consistency is guaranteed by durable intent persistence and retry logic.
- Bidirectional safety includes self-write loop prevention (`self_write_cache`, planned in Phase 3) and deterministic conflict handling (planned in Phase 4).
- Graceful shutdown on SIGTERM/SIGINT (or the platform-native equivalent on Windows/Linux) and a `CrashLoopPaused` state after 5 unclean exits in 10 minutes. macOS specifics: `docs/operations/macos/launchagent-policy.md`.
