# LaunchAgent Policy and Crash-Loop Interaction (macOS)

This document defines the concrete `launchd` plist policy for the `vapord`
per-user LaunchAgent, the expected interaction between the LaunchAgent and
the Rust-backed crash-loop protection in `core/lifecycle`, and the
validation scenarios.

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
- The plist has a single writer: the Rust `NativeServiceInstaller` in
  `core/platform`, driven through `core/lifecycle` by `vapor service
  install`. Every surface goes through it — the macOS app shells out
  to the bundled `vapor` CLI instead of writing the plist itself — so
  the definition cannot diverge between surfaces.
- Uninstall semantics: removing the agent (`vapor service uninstall`,
  the app's autolaunch-off toggle) boots the job out of launchd, and
  launchd terminates the running daemon as part of bootout. This is
  long-standing macOS behavior (the retired Swift controller did the
  same); `--keep-running` therefore only suppresses the *explicit*
  stop signal and is meaningful on service managers that keep a
  disabled unit running (e.g. systemd on Linux).
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
   Detection is app-side but policy-free: the macOS app runs a
   30-second timer (`DaemonHealthMonitor`,
   `VaporConstants.Daemon.healthTickIntervalSeconds`) whose every tick
   invokes `vapor service check` — one supervision tick that detects an
   unexpected daemon exit, registers the crash, and restarts or defers
   per the schedule above. Expected stops are never counted, and the
   `awaiting_restart` marker prevents the same exit from being counted
   twice. Crash-loop bookkeeping (`consecutive_crashes`,
   `last_crash_at_ms`, pause, `awaiting_restart`) persists durably in
   `<vapor_dir>/state/lifecycle.json` (owned by `core/lifecycle`), so
   backoff and pause survive process restarts and are shared across
   surfaces.
3. After 5 consecutive crashes within the `failureWindow` (default 10
   minutes), the guard enters a durable `CrashLoopPaused` state, stops
   attempting auto-restart (`vapor service status` reports
   `crash_loop_paused`), and the app surfaces a reasoned diagnostic to
   the menubar ("Vapor paused: repeated crashes, click to inspect
   logs"). The user must explicitly acknowledge (via `vapor service
   acknowledge`, which the menubar action invokes) before restarts
   resume.
4. `launchd` is NEVER expected to be the source of a daemon restart. If
   a contributor finds code or scripts that set `KeepAlive = true` on the
   daemon's job, that is a policy violation and must be reverted.

## The headless supervisor

Without the app, nobody runs `vapor service check`, so a CLI-only
install would never restart a crashed daemon. `vapor service install
--supervise` registers a second LaunchAgent, `sh.arn.vapor.supervisor`,
whose program is the `vapor` binary itself running `service check
--loop`: the app's health tick as a process, one tick every 30 seconds
(`--interval` to change it), printing an outcome only when it changes.
This job is the one job launchd keeps alive (`KeepAlive = true`): it is
the supervisor, not the daemon, so the crash-loop guard still owns
every daemon restart and its budget. Its output lands in
`<vapor_dir>/logs/vapor-supervisor.log`. `vapor service status` reports
`supervisor_installed`, and `vapor service uninstall` removes both jobs.
The `--full` e2e run installs it, kills the daemon, and asserts the
supervisor alone brings it back.

## Validation

The exponential relaunch delay is implemented in `core/lifecycle`. The
scenarios below are the acceptance checks for the policy; the plist
audit and the crash-loop sequence run automatically in the `--full`
phase of `./scripts/e2e.sh`, the rest are checked by hand on a release
candidate:

- **Plist audit**: assert the installed plist at
  `~/Library/LaunchAgents/sh.arn.vapor.daemon.plist` contains exactly
  `Label`, `ProgramArguments`, `RunAtLoad=true`, `KeepAlive=false`,
  `StandardOutPath`, `StandardErrorPath`, `EnvironmentVariables`,
  `ProcessType=Background`, and no other keys; the supervisor's plist
  is the same shape with `KeepAlive=true`.
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

The service lifecycle path (install → start → status → crash-loop
supervision through backoff and pause → acknowledge → stop → uninstall)
is exercised against real `launchd` in CI by the `--full` phase of
`./scripts/e2e.sh` (`test.yml`'s macOS job runs the e2e step with
`--full`); that phase installs a real LaunchAgent, so it is opt-in and
runs only on disposable CI runners.
