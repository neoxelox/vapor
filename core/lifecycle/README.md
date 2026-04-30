# core/lifecycle

Daemon lifecycle orchestration shared by every Vapor surface
(macOS app, `vapor` CLI, future Windows / Linux apps).

Owns the cross-platform crash-loop guard, the autolaunch setting store,
and the `DaemonLifecycleManager` that ties them together over a
[`vapor_platform::ServiceInstaller`](../platform/src/service/mod.rs).

Authoritative reference: `docs/plans/core.md §2.3`.
Tasks: `docs/tasks/core.md` Phase C4.

## What lives here

- `CrashLoopGuard` — pure-logic backoff policy. `baseDelay=2s`,
  `maxDelay=120s`, `maxConsecutiveFailuresBeforePause=5`,
  `failureWindow=600s` by default; matches the Swift implementation
  one-to-one (`apps/macos/Sources/VaporCore/DaemonLifecycle.swift`).
- `AutoLaunchSettingStore` — trait + JSON-file impl that reads / writes
  the `autoLaunch` key in `vapor.json`. Atomic-on-disk via
  temp-file-then-rename so the Swift `VaporConfigurationStore` and the
  Rust CLI can share one file.
- `DaemonLifecycleManager` — orchestrator. Public surface mirrors the
  Swift class (`bootstrap_if_needed`, `set_auto_launch_enabled`,
  `register_unexpected_daemon_exit`, `start_daemon_if_allowed`,
  `stop_daemon_for_termination`, `acknowledge_crash_loop_pause`).

## Why this crate exists

Three surfaces (the macOS Swift app today; the `vapor` CLI in Wave 6;
the Windows / Linux apps if they ship) need identical autolaunch and
crash-loop behavior. Keeping that logic in Rust and consuming it from
Swift via the `vapor` CLI subprocess (per `core.md` C4-5) keeps the
contract single-sourced.

## Wave status

- **Done (Wave 5):** crate + trait + manager + parity tests.
- **Pending (Wave 6):** `vapor service install / start / stop / status`
  CLI commands consume `DaemonLifecycleManager`.
- **Pending (Wave 6 / M2):** macOS Swift app delegates to the CLI
  subprocess; the duplicate Swift `CrashLoopGuard` /
  `DaemonLifecycleManager` retire once parity is verified end-to-end.
