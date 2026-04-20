# LaunchAgent Policy and Crash-Loop Interaction

This document defines the concrete `launchd` plist policy for the `vapord` per-user LaunchAgent, the expected interaction between the LaunchAgent and the daemon's in-process crash-loop protection, and the validation expected at Phase 1.

## Policy

### Label and identity

- `Label`: `sh.arn.vapor.daemon` (fixed; reverse-DNS scoped under `sh.arn.vapor.*` per AGENTS §2.2).
- `ProgramArguments`: absolute path to the bundled sibling daemon resolved at install-time via `Vapor.app/Contents/MacOS/vapord` (never a global install path; see AGENTS §7).

### Execution policy

- `RunAtLoad = true`: the LaunchAgent starts the daemon on user login and after SMAppService registration.
- `KeepAlive = false`: `launchd` does NOT auto-restart the daemon on exit. Restart decisions are owned by (a) the app's lifecycle coordinator (reopen + deliberate restart) and (b) the daemon's in-process crash-loop protection. This avoids collision between `launchd`'s default ~10s restart cadence and the daemon's exponential backoff.
- No `SuccessfulExit` conditional — `KeepAlive = false` is unconditional.

### I/O redirection and environment

- `StandardOutPath` and `StandardErrorPath`: redirected to `<vapor_dir>/logs/vapord.stdout.log` and `<vapor_dir>/logs/vapord.stderr.log` respectively, created with `0o600` if absent.
- `EnvironmentVariables`: pass-through of `VAPOR_DIR` and `VAPOR_ENV` only. All other runtime behavior is code-defined or read from `vapor.json`.
- `ProcessType`: `Background` so the daemon participates in background resource-management policy.

### Limits and scheduling

- `Nice`: unset (default 0). CPU de-prioritization is handled in-process by the throttle controller and user resource ceilings; setting Nice externally would double-dip.
- No `StartInterval`/`StartCalendarInterval`: the daemon is a long-lived service, not a scheduled task.
- No `WatchPaths`: watch paths are owned by the in-process FSEvents watcher, not `launchd`.

## Crash-loop interaction

The daemon owns crash-loop protection. `launchd` is intentionally passive (`KeepAlive = false`):

1. On a clean exit (signal-driven shutdown from app menubar quit, or a fatal-but-expected classified error), `launchd` does nothing and the daemon stays stopped until the next user trigger (app launch, login, explicit restart via menubar).
2. On an unclean exit (panic, SIGSEGV, SIGKILL from external signal, OOM), the app's lifecycle coordinator detects daemon absence on its next health tick, classifies the situation, and applies exponential backoff before attempting restart. Backoff schedule: `2s, 4s, 8s, 16s, 32s, 60s, 120s` with a ceiling at `120s` and reset-on-successful-run-for-600s. The daemon's durable state carries a `last_crash_at_ms` and `consecutive_crashes` counter persisted via the state DB so backoff survives app restarts.
3. After 5 consecutive crashes within a 10-minute window, the coordinator enters a `CrashLoopPaused` state, stops attempting auto-restart, and surfaces a reasoned diagnostic to the menubar ("Vapor paused: repeated crashes, click to inspect logs"). The user must explicitly acknowledge (via a menubar action) before restarts resume.
4. `launchd` is NEVER expected to be the source of a restart. If a contributor finds code or scripts that set `KeepAlive = true`, that is a policy violation and must be reverted.

## Validation

Phase 1 (P1-5) is marked complete as "exponential relaunch delay implemented." Phase 5 / Phase 7 must add the following validation on top:

- **Plist audit**: assert the installed plist at `~/Library/LaunchAgents/sh.arn.vapor.daemon.plist` contains exactly `Label`, `ProgramArguments`, `RunAtLoad=true`, `KeepAlive=false`, `StandardOutPath`, `StandardErrorPath`, `EnvironmentVariables`, `ProcessType=Background`, and no other keys.
- **SIGKILL scenario**: start the daemon, `kill -9` its pid, wait `30s`, assert no automatic restart has occurred (no new pid for `vapord`). Touching the config or issuing a menubar action must then trigger a restart through the app coordinator, not `launchd`.
- **Crash-loop pause scenario**: force 5 crashes within 10 minutes, assert the coordinator reaches `CrashLoopPaused` and stops restart attempts, and that the menubar surfaces the paused state with the documented reason.
- **Clean shutdown scenario**: menubar-quit the app, assert both app and daemon exit cleanly, assert `launchd` does not relaunch the daemon until the next login or explicit app launch.

Test coverage for these scenarios is owned by the Phase 1 follow-up task `P1-6` in `docs/plans/vapor-macos-task-list.md`.
