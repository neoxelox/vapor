# LaunchAgent Policy and Crash-Loop Interaction (macOS)

This document defines the concrete `launchd` plist policy for the `vapord`
per-user LaunchAgent, the expected interaction between the LaunchAgent and
the daemon's in-process crash-loop protection, and the validation expected
at milestone M1.

Cross-platform lifecycle logic (including `CrashLoopGuard`) lives in
`core/lifecycle`; this document covers only the macOS-native integration
surface produced by `core/platform/service::macos`.

## Policy

### Label and identity

- `Label`: `sh.arn.vapor.daemon` (fixed; reverse-DNS scoped under
  `sh.arn.vapor.*` per `AGENTS.md §2.2`).
- `ProgramArguments`: absolute path to the bundled sibling daemon resolved
  at install-time via `Vapor.app/Contents/MacOS/vapord` (never a global
  install path; see `AGENTS.md §7`).

### Execution policy

- `RunAtLoad = true`: the LaunchAgent starts the daemon on user login and
  after `SMAppService` registration.
- `KeepAlive = false`: `launchd` does NOT auto-restart the daemon on exit.
  Restart decisions are owned by (a) the app's lifecycle coordinator
  (reopen + deliberate restart) and (b) the Rust-backed crash-loop guard
  in `core/lifecycle`. This avoids collision between `launchd`'s default
  ~10s restart cadence and the guard's exponential backoff.
- No `SuccessfulExit` conditional — `KeepAlive = false` is unconditional.

### I/O redirection and environment

- `StandardOutPath` and `StandardErrorPath`: redirected to
  `<vapor_dir>/logs/vapord.stdout.log` and
  `<vapor_dir>/logs/vapord.stderr.log` respectively, created with `0o600`
  if absent.
- `EnvironmentVariables`: pass-through of `VAPOR_DIR` and `VAPOR_ENV` only.
  All other runtime behavior is code-defined or read from `vapor.json`
  (the daemon loads `vapor.json` at startup; `VAPOR_*` variables remain
  per-field overrides).
- Both plist writers — the macOS app's `LaunchAgentController` and the
  Rust `NativeServiceInstaller` driven by `vapor service install` —
  emit this same shape, so either surface may (re)install the agent
  without clobbering the other's configuration.
- `ProcessType`: `Background` so the daemon participates in background
  resource-management policy.

### Limits and scheduling

- `Nice`: unset (default 0). CPU de-prioritization is handled in-process by
  the throttle controller and user resource ceilings; setting Nice
  externally would double-dip.
- No `StartInterval`/`StartCalendarInterval`: the daemon is a long-lived
  service, not a scheduled task.
- No `WatchPaths`: watch paths are owned by the in-process `FsWatcher`, not
  `launchd`.

## Crash-loop interaction

The Rust-backed `CrashLoopGuard` in `core/lifecycle` owns crash-loop
protection. `launchd` is intentionally passive (`KeepAlive = false`):

1. On a clean exit (signal-driven shutdown from app menubar quit, or a
   fatal-but-expected classified error), `launchd` does nothing and the
   daemon stays stopped until the next user trigger (app launch, login,
   explicit restart via menubar).
2. On an unclean exit (panic, SIGSEGV, SIGKILL from external signal, OOM),
   the lifecycle owner registers the crash with the shared
   `CrashLoopGuard` and applies exponential backoff before attempting
   restart. The canonical schedule (locked in by
   `core/lifecycle/tests/crash_loop_parity.rs` and mirrored by the Swift
   tests) with the default policy — `delayStartsAfterFailures = 1`,
   `baseDelay = 2s`, `maxDelay = 120s`,
   `maxConsecutiveFailuresBeforePause = 5` — is:
   `crash 1 → restart immediately, crash 2 → 2s, crash 3 → 4s,
   crash 4 → 8s, paused on the 5th crash`. The longer theoretical
   sequence (`16s, 32s, …, 120s`) is only reachable if a deployment
   raises `maxConsecutiveFailuresBeforePause`; the default never reaches
   it. Crashes age out of the sliding `failureWindow = 600s`, so a run
   that stays healthy for the window length resets the schedule.
   Planned (M-wave, not yet implemented): a periodic app-side health
   tick that *detects* unexpected daemon absence, and durable
   `last_crash_at_ms` / `consecutive_crashes` counters in the state DB so
   backoff survives app restarts — today each surface counts crashes in
   process memory only.
3. After 5 consecutive crashes within the `failureWindow` (default 10
   minutes), the coordinator enters a `CrashLoopPaused` state, stops
   attempting auto-restart, and surfaces a reasoned diagnostic to the
   menubar ("Vapor paused: repeated crashes, click to inspect logs"). The
   user must explicitly acknowledge (via `acknowledgeCrashLoopPause`,
   wired through a menubar action) before restarts resume.
4. `launchd` is NEVER expected to be the source of a restart. If a
   contributor finds code or scripts that set `KeepAlive = true`, that is a
   policy violation and must be reverted.

## Validation

M1-5 is marked complete as "exponential relaunch delay implemented". The
follow-up M1-6 task in `docs/tasks/macos.md` owns the validation scenarios
listed below:

- **Plist audit**: assert the installed plist at
  `~/Library/LaunchAgents/sh.arn.vapor.daemon.plist` contains exactly
  `Label`, `ProgramArguments`, `RunAtLoad=true`, `KeepAlive=false`,
  `StandardOutPath`, `StandardErrorPath`, `EnvironmentVariables`,
  `ProcessType=Background`, and no other keys.
- **SIGKILL scenario**: start the daemon, `kill -9` its pid, wait `30s`,
  assert no automatic restart has occurred (no new pid for `vapord`).
  Touching the config or issuing a menubar action must then trigger a
  restart through the app coordinator, not `launchd`.
- **Crash-loop pause scenario**: force 5 crashes within 10 minutes, assert
  the coordinator reaches `CrashLoopPaused` and stops restart attempts, and
  that the menubar surfaces the paused state with the documented reason.
- **Clean shutdown scenario**: menubar-quit the app, assert both app and
  daemon exit cleanly, assert `launchd` does not relaunch the daemon until
  the next login or explicit app launch.

M1-6 in `docs/tasks/macos.md` is the work item that ships these scenarios
as automated tests.
