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
  one-to-one.
- `AutoLaunchSettingStore` — trait + JSON-file impl that reads / writes
  the `autoLaunch` key in `vapor.json`. Atomic-on-disk via
  temp-file-then-rename so the Swift `VaporConfigurationStore` and the
  Rust CLI can share one file.
- `DaemonLifecycleManager` — orchestrator. Public surface mirrors the
  Swift class (`bootstrap_if_needed`, `set_auto_launch_enabled`,
  `register_unexpected_daemon_exit`, `start_daemon_if_allowed`,
  `stop_daemon_for_termination`, `acknowledge_crash_loop_pause`).

## Why this crate exists

Three surfaces (the macOS app today; the `vapor` CLI; the Windows and
Linux apps if they ship) need identical autolaunch and crash-loop
behavior. Keeping that logic in Rust and consuming it from Swift through
the `vapor` CLI subprocess keeps the contract single-sourced: the app
never reimplements a lifecycle decision.

## Status

`vapor service install | uninstall | bootstrap | start | stop | restart |
status | check | acknowledge` all drive `DaemonLifecycleManager`, and the
macOS app calls those commands with `--json`; the Swift crash-loop guard
is gone. Durable crash-loop state lives at `<vapor_dir>/state/lifecycle.json`.
