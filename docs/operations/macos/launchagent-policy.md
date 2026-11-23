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
- No `SuccessfulExit` conditional: `KeepAlive = false` is unconditional.

### I/O redirection and environment

- `StandardOutPath` and `StandardErrorPath`: redirected to
  `<vapor_dir>/logs/vapord.stdout.log` and
  `<vapor_dir>/logs/vapord.stderr.log` respectively, created with `0o600`
  if absent.
- `EnvironmentVariables`: pass-through of `VAPOR_DIR`, plus `VAPOR_ENV`
  and `VAPOR_THROTTLE_INPUTS` when the installing shell has them set
  (the latter is how the e2e harness keeps a launchd-managed daemon on
  neutral throttle inputs; an ordinary install never carries it). All
  other runtime behavior is code-defined or read from `vapor.json`
  (the daemon loads it at startup and reloads it live). No environment
  variable overrides a `vapor.json` key.
- The plist has a single writer: the Rust `NativeServiceInstaller` in
  `core/platform`, driven through `core/lifecycle` by `vapor service
  install`. Every surface goes through it: the macOS app shells out
  to the bundled `vapor` CLI instead of writing the plist itself, so
  the definition cannot diverge between surfaces.
- Uninstall semantics: removing the agent (`vapor service uninstall`,
  the app's autolaunch-off toggle) boots the job out of launchd, and
  launchd terminates the running daemon as part of bootout. That is how
  launchd works; `--keep-running` therefore only suppresses the
  *explicit* stop signal and is meaningful on service managers that
  keep a disabled unit running (e.g. systemd on Linux).
- Install is idempotent: when the plist on disk is the one the
  installer would write and the job is loaded, install changes
  nothing, so a running daemon is not restarted by a second install.
- Start never kills: `launchctl kickstart` runs without `-k`, so a
  start request for a daemon that already runs leaves it running.
- A daemon of another build is replaced: an update puts a new `vapord`
  where the old one runs, and neither rule above would restart it.
  After `vapor service install` or `bootstrap`, the CLI asks the
  running daemon for its build (`vapord/<version>` in the status) and
  restarts it when the build is not the CLI's own, or when the daemon
  speaks another IPC version. A daemon of the same build is left alone.
- Stop waits: after the stop signal the installer waits up to 10
  seconds for the daemon to exit, so its shutdown flush completes
  before the caller goes on. The intent to stop is written to
  `lifecycle.json` (`stopRequestedAtMs`) before the signal. A daemon
  that takes longer to exit (a worker inside a network call) is "on
  its way out" for 180 seconds: a supervision tick in that time does
  not count a crash and does not restart it.
- `ProcessType`: `Background` so the daemon participates in background
  resource-management policy. This has a price that was measured on an
  idle Apple silicon Mac with `taskpolicy -b`, which applies the same
  policy: a SHA-256 over 768 MiB took 6.5 to 7.0 s against 2.4 to
  2.5 s, about 2.7 times slower, and a file copy took 0.55 s against
  0.30 s. The policy also keeps the daemon off the performance cores
  and lowers its I/O priority, which is the point for a background
  sync. Whether `Standard` serves users better is an open decision
  (`docs/specs/2026-11-14-full-review.md`).

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
   explicit restart via menubar). The menubar restart exists only while
   the job is registered: with auto-launch off there is no job, `vapor
   service restart` answers `not-installed`, and the app disables the
   control with a hint that Start Vapor at login is what runs sync in
   the background.
2. On an unclean exit (panic, SIGSEGV, SIGKILL from external signal, OOM),
   the lifecycle owner registers the crash with the shared
   `CrashLoopGuard` and applies exponential backoff before attempting
   restart. The canonical schedule (locked in by
   `core/lifecycle/tests/crash_loop_parity.rs` and mirrored by the Swift
   tests) with the default policy: `delayStartsAfterFailures = 1`,
   `baseDelay = 2s`, `maxDelay = 120s`,
   `maxConsecutiveFailuresBeforePause = 5`: is:
   `crash 1 → restart immediately, crash 2 → 2s, crash 3 → 4s,
   crash 4 → 8s, paused on the 5th crash`. The longer theoretical
   sequence (`16s, 32s, …, 120s`) is only reachable if a deployment
   raises `maxConsecutiveFailuresBeforePause`; the default never reaches
   it. Crashes age out of the sliding `failureWindow = 600s`, so a run
   that stays healthy for the window length resets the schedule. The
   delays are shorter than the 30-second supervision tick, so what a
   user sees is a restart on the next tick or the one after it; the
   schedule decides which tick, not the second.
   Detection belongs to the supervisor job (next section): every 30
   seconds it runs `vapor service check`, one supervision tick that
   detects an unexpected daemon exit, registers the crash, and restarts
   or defers per the schedule above. Between two ticks it asks the
   daemon's socket twice a second, so it runs the tick within a second
   of an exit and at the end of a backoff, not up to 30 seconds later.
   It is the one caller of that check, so one exit is counted once. The macOS app restarts nothing:
   its 30-second timer (`DaemonHealthMonitor`,
   `VaporConstants.Daemon.healthTickIntervalSeconds`) runs `vapor
   service status`, which reports the `health` a check would find and
   changes nothing, and the app shows it. Expected stops are never
   counted, and the `awaitingRestart` marker prevents the same exit
   from being counted twice. Crash-loop bookkeeping
   (`consecutiveCrashes`, `lastCrashAtMs`, `pausedIndefinitely`,
   `awaitingRestart`) persists durably in
   `<vapor_dir>/state/lifecycle.json` (owned by `core/lifecycle`), so
   backoff and pause survive process restarts and are shared across
   surfaces. Each process reads the file again before it acts and
   writes its change before it starts the daemon, so the supervisor's
   tick and a `vapor service` command do not undo each other.
3. After 5 consecutive crashes within the `failureWindow` (default 10
   minutes), the guard enters a durable `CrashLoopPaused` state, stops
   attempting auto-restart (`vapor service status` reports
   `crash-loop-paused`), and the app surfaces a reasoned diagnostic to
   the menubar ("Vapor paused: repeated crashes, click to inspect
   logs"). The user must explicitly acknowledge (via `vapor service
   acknowledge`, which the menubar action invokes) before restarts
   resume.
4. `launchd` is NEVER expected to be the source of a daemon restart. If
   a contributor finds code or scripts that set `KeepAlive = true` on the
   daemon's job, that is a policy violation and must be reverted.

## The supervisor

Every install registers a second LaunchAgent next to the daemon's,
`sh.arn.vapor.supervisor`: `vapor service install`, `vapor service
bootstrap` (what the app runs at its start), and `vapor init` all do
it. Its program is the `vapor` binary itself running `service check
--loop`: one supervision tick every 30 seconds (`--interval` to change
it), and one as soon as the daemon stops answering its socket,
printing an outcome only when it changes. A crashed daemon so comes
back within about a second, with the app open, with the app closed,
and on a device that never opens the app.

A daemon whose process runs and whose socket gives no answer for 30
seconds hangs. The supervisor ends that process (`SIGKILL`), which the
next tick counts as a crash and restarts under the same budget. A
daemon that starts or stops answers inside that time, so neither is
ended. `vapor service stop` and `restart` have the same last step: a
daemon that does not exit 10 seconds after `SIGTERM` is ended, so no
second daemon ever starts beside a first one that still holds the
runtime directory.

This job is the one job launchd keeps alive (`KeepAlive = true`): it is
the supervisor, not the daemon, so the crash-loop guard still owns
every daemon restart and its budget. Its output lands in
`<vapor_dir>/logs/vapor-supervisor.log`. `vapor service status` reports
`supervisorInstalled`, and `vapor service uninstall` removes both jobs.
The same command reads the state of the job from launchd and from the
process table, not from the daemon, and says in words when the process
runs and gives no answer (`answers: false` with `--json`).
A daemon that the user stopped (`vapor service stop`, Quit Vapor) reads
as stopped on purpose, and the supervisor leaves it stopped.

The supervisor is also what carries an update through. An installer
replaces the binaries and starts nothing. At its next tick the
supervisor sees that its own binary was replaced and ends itself;
launchd starts the new one, which stops a daemon of the old build and
starts the new daemon. So the CLI, the daemon, and the supervisor run
the same version within about a minute of an install, with no step by
the user.

The `--full` e2e run installs both jobs, kills the daemon, and asserts
the supervisor alone brings it back.

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
- **SIGKILL scenario**: with no app running, start the daemon, `kill
  -9` its pid, and assert the supervisor restarts it within one or two
  ticks. The restart comes from the supervisor, never from `launchd`:
  with the supervisor job taken off launchd by hand, the daemon stays
  down.
- **Crash-loop pause scenario**: force 5 crashes within 10 minutes, assert
  the coordinator reaches `CrashLoopPaused` and stops restart attempts, and
  that the menubar surfaces the paused state with the documented reason.
- **Clean shutdown scenario**: menubar-quit the app, assert both app and
  daemon exit cleanly, assert neither `launchd` nor the supervisor
  relaunches the daemon until the next login or explicit app launch.

The service lifecycle path (install of both jobs → start → status →
crash-loop supervision through backoff and pause → acknowledge → stop →
uninstall) is exercised against real `launchd` in CI by the `--full`
phase of `./scripts/e2e.sh` (`test.yml`'s macOS job runs the e2e step
with `--full`). The steps that walk the guard run `service check` by
hand, so the scenario sets the supervisor aside for them and installs
it again for the restart check. That phase installs real LaunchAgents,
so it is opt-in and runs only on disposable CI runners.
