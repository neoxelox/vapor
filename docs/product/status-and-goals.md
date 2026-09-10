# Product Status and Goals

## Project status

- Current stage: pre-GA (`v0.2.0-alpha.3`). Runtime Phases M0, M1, M1.5, M2, M2.5 (macOS-centric numbering in legacy task tracking) are complete. Going forward, runtime-level work is tracked in `docs/tasks/core.md`, macOS app work in `docs/tasks/macos.md`, and CLI work in `docs/tasks/cli.md`.
- Two providers ship: the filesystem provider (default; a local directory plays the cloud side) and Google Drive (`provider = "gdrive"`, OAuth with PKCE, tokens in the macOS login keychain). Sync profiles, keep-both conflict copies, loop prevention, one-way sync modes, live configuration reload, and the `vapor` CLI are all in the runtime today.
- Product direction: bidirectional eventual consistency between a user-selected local folder and a user-selected provider folder. macOS is the first shipping surface; the `vapor` CLI follows on every OS; Windows and Linux apps land on the same portable Rust runtime.
- Google Drive is the first external cloud target and is selectable now; it is exercised offline in CI through a scripted HTTP transport, and a live-account end-to-end tier remains future, explicitly gated work.
- Primary constraint: do no harm to user workload, battery, and thermal headroom — on every supported OS.
- Pre-GA compatibility policy: backward compatibility is not guaranteed yet; config/state/schema and local interfaces may change during active development.

## Product goals

- Keep one selected local folder (default `~/Vapor`) bidirectionally synced with one selected provider folder (default `/Vapor` on a cloud provider, `~/cloud/Vapor` when a local folder plays the cloud) with durable intent state.
- Scope sync strictly to that configured folder pair; Vapor is not intended to be full-device backup.
- Stay low-impact during active development and heavy system load.
- Defer expensive work under pressure while maintaining eventual consistency.
- Provide transparent state, diagnostics, and user controls from the macOS app and menubar.
- Honor user-configured resource ceilings (`resourceLimits`) as hard caps, with optional `idleBoost` that raises ceilings only when the device is genuinely idle with measured headroom.
- Support multiple sync profiles so one device can target multiple provider accounts safely with per-profile overrides.
- Let the user choose each folder's sync direction (`syncMode`): `two-way` bidirectional by default, or one-way `pull-only` (cloud → local) / `push-only` (local → cloud) strict-mirror modes for read-only backups and copies. Selectable per profile so one device can keep several read-only mirrors alongside a bidirectional folder. Design: `docs/architecture/sync-modes.md`.
- Default to the `keep both copies` conflict policy with deterministic conflict-suffix paths so no edit is silently overwritten. The `keep both` / never-lose-data guarantee is scoped to `two-way`; the one-way `syncMode` variants are an explicit, opt-in exception with a declared source of truth and an up-front overwrite warning (see `docs/architecture/sync-modes.md §Safety`).

## Runtime model

- Auto-launch at login is ON by default.
- Throttle states govern all heavy work: `IdleDrain`, `Light`, `Throttled`, `Suspended`.
- User resource ceilings (`resourceLimits.cpuPercent`, `memoryPercent`, `bandwidthPercent`) and `idleBoost` (default-on dynamic headroom) layer over the throttle controller without ever relaxing it.
- Eventual consistency is guaranteed by durable intent persistence and retry logic.
- Bidirectional safety includes self-write loop prevention (`self_write_cache` plus op-id tags) and deterministic conflict handling.
- Sync direction is per profile via `syncMode` (`two-way` default; one-way `pull-only` / `push-only` strict mirror). One-way modes have a declared source of truth and drive the subordinate side to match it — opt-in and destructive (permanent overwrite/delete, no recoverable copy), so they carry an up-front data-loss warning. The never-lose-data guarantee applies to `two-way` only. See `docs/architecture/sync-modes.md`.
- Graceful shutdown on SIGTERM/SIGINT (or the platform-native equivalent on Windows/Linux) and a `CrashLoopPaused` state after 5 unclean exits in 10 minutes. macOS specifics: `docs/operations/macos/launchagent-policy.md`.
