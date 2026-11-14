# Product Status and Goals

## Project status

- Current stage: pre-GA, with no users yet. Runtime work is tracked in `docs/tasks/core.md`, macOS app work in `docs/tasks/macos.md`, and CLI work in `docs/tasks/cli.md`. The version is in the root `VERSION` file.
- Two providers ship: the filesystem provider (default; a local directory plays the cloud side) and Google Drive (`provider = "gdrive"`, OAuth with PKCE, tokens in the macOS login keychain). Sync profiles, keep-both conflict copies, loop prevention, one-way sync modes, live configuration reload, and the `vapor` CLI are all in the runtime today.
- Product direction: bidirectional eventual consistency between a user-selected local folder and a user-selected provider folder. macOS is the first shipping surface; the `vapor` CLI follows on every OS; Windows and Linux apps land on the same portable Rust runtime.
- Google Drive is the first external cloud target and is selectable now; it is exercised offline in CI through a scripted HTTP transport. A run against a live account is a leg of the release gate (`./scripts/e2e.sh --provider gdrive`); its Drive-side operations are open work that waits on a dedicated test account.
- Primary constraint: do no harm to user workload, battery, and thermal headroom, on every supported OS.
- Pre-GA compatibility policy: backward compatibility is not guaranteed yet; config/state/schema and local interfaces may change during active development.

## Product goals

- Keep one selected local folder bidirectionally synced with one selected provider folder, with durable intent state. The user chooses both folders: nothing syncs, and no folder is created, until a profile has its two roots (`vapor profiles add`, or the app's first-run flow). `~/Vapor` and `/Vapor` are the suggestions the setup offers.
- Scope sync strictly to that configured folder pair; Vapor is not intended to be full-device backup.
- Stay low-impact during active development and heavy system load.
- Defer expensive work under pressure while maintaining eventual consistency.
- Provide transparent state, diagnostics, and user controls from the macOS app and menubar.
- Honor user-configured resource ceilings (`resourceLimits`) as measured limits: the daemon reads what it uses and what the link carries, and corrects within seconds. The optional `idleBoost` raises ceilings only when the device is idle with measured headroom.
- Support multiple sync profiles so one device can target multiple provider accounts safely with per-profile overrides.
- Let the user choose each folder's sync direction (`syncMode`): `two-way` bidirectional by default, or one-way `pull-only` (cloud → local) / `push-only` (local → cloud) strict-mirror modes for read-only backups and copies. Selectable per profile so one device can keep several read-only mirrors alongside a bidirectional folder. Design: `docs/architecture/sync-modes.md`.
- Default to the `keep both copies` conflict policy with deterministic conflict-suffix paths so no edit is silently overwritten. The `keep both` / never-lose-data guarantee is scoped to `two-way`; the one-way `syncMode` variants are an explicit, opt-in exception with a declared source of truth and an up-front overwrite warning (see `docs/architecture/sync-modes.md §Safety`).

## Runtime model

- Auto-launch at login is ON by default.
- Throttle states govern all heavy work: `Idle`, `Light`, `Throttled`, `Suspended`. A state makes sync slower. Only `Suspended` stops it, and only critical thermal pressure gives `Suspended`: battery power, a user at the device, and Low Power Mode never stop sync.
- User resource ceilings (`resourceLimits.cpuPercent`, `memoryPercent`, `bandwidthPercent`) and `idleBoost` (default-on dynamic headroom) layer over the throttle controller without ever relaxing it.
- Eventual consistency is guaranteed by durable intent persistence and retry logic.
- Bidirectional safety includes self-write loop prevention (`self_write_cache` plus op-id tags) and deterministic conflict handling.
- Sync direction is per profile via `syncMode` (`two-way` default; one-way `pull-only` / `push-only` strict mirror). One-way modes have a declared source of truth and drive the subordinate side to match it: opt-in and destructive, so the daemon holds a one-way profile until its data-loss warning is acknowledged. The local trash keeps what a mirror removes on this device; an overwrite, and a removal in a cloud without a trash, have no undo. The never-lose-data guarantee applies to `two-way` only. See `docs/architecture/sync-modes.md`.
- Graceful shutdown on SIGTERM/SIGINT (or the platform-native equivalent on Windows/Linux) and a `CrashLoopPaused` state after 5 unclean exits in 10 minutes. macOS specifics: `docs/operations/macos/launchagent-policy.md`.
