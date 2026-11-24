# Vapor CLI plan

Core runtime plan: `docs/plans/core.md`
Execution checklist: `docs/tasks/cli.md`

## 0) Scope

This plan covers the `vapor` CLI binary (source in `core/cli`, ships inside
the install package of every supported OS, next to the daemon).

Binary names are fixed:

- **`vapor`**: the CLI.
- **`vapord`**: the daemon.

## 1) Mission

`vapor` is the reference consumer of the portable runtime and the universal
control plane every other app can shell out to. The primary shipping target
is macOS, inside the macOS installer. On Linux and Windows it ships inside
the install package of the platform, behind the optional waves in
`docs/tasks/README.md`; the crate is designed so they cost only a
recompile + CI job once those waves land, not a rewrite.

## 2) Non-negotiables

1. **Headless-first.** No GUI dependencies; works over SSH, in Docker, and in
   CI. No interactive-TTY requirements: every command has a deterministic
   non-interactive code path.
2. **Parity by default.** Every runtime capability exposed in a GUI app must
   also be reachable from the CLI. If a feature is CLI-only, it must be
   because GUI ergonomics would hurt it, not because someone forgot to wire
   it.
3. **Scriptable.** Every command that reports state supports `--json`
   output (`logs`, `config get|set`, `version`, and `run` print plain
   text); exit codes are stable and documented.
4. **Per-OS native when it matters.** The CLI inherits platform-native
   implementations from `core/platform`: it uses FSEvents on macOS, inotify
   on Linux, ReadDirectoryChangesW on Windows. No "portable but slow" path.
5. **Respects the same config surface as GUI apps.** Reads `VAPOR_DIR`,
   `VAPOR_ENV`, `vapor.json`. Writes and reads the same locale catalogs.

## 3) Command surface

```
vapor init [--provider --local --cloud --mode --token - --auto-launch on|off] [--json]
                                        # the first run: profile, sign-in, background service
vapor status [--json]                   # what the daemon does and why
vapor pause|resume [--json]
vapor reconcile [--json]                # scan now: any throttle state but suspended, plus the boost
vapor profiles list|add|enable|disable|rename|remove   # the folders Vapor syncs; `init` runs the first add
vapor decisions list|show|resolve       # conflicts and parked questions (resolve --choose <option>)
vapor ignored [--profile <id>] [--json] # what the ignore rules keep out of sync, with the rule
vapor trash list|restore|empty          # what Vapor removed from this device (empty asks, or --yes)
vapor config get|set <key> [value]      # edits vapor.json
vapor auth login [--profile <id>]       # signs the profile in to its provider (--token - reads stdin)
vapor auth logout [--profile <id>]
vapor auth status [--profile <id>] [--json]  # signed-in / signed-out / not-needed, never the token
vapor service bootstrap                 # install+start only when autolaunch is enabled
vapor service install                   # native ServiceInstaller, with the supervisor (--linger on|off on Linux)
vapor service uninstall [--keep-running]
vapor service start|stop|restart|status
vapor service check [--loop]            # one crash-loop supervision tick; the supervisor job runs the loop
vapor service acknowledge               # clear a crash-loop pause
vapor timeline [--json]                 # recent daemon activity
vapor diagnostics [--json]              # per-intent "why is this stuck"
vapor logs [--tail N]
vapor doctor [--json]                   # sanity checks, exit 1 on failure
vapor report [--output DIR] [--keep-paths] [--json]  # support report; names replaced by codes
vapor run                               # run the daemon in the foreground
vapor version
```

Names follow one rule: single-word commands and subcommands, camelCase
keys and kebab-case values in every `--json` output. A command that acts
on exactly one profile takes `--profile` and needs it only when several
profiles exist; a command that reads takes it as a filter and reads every
profile without it.

### 3.1 Rules per command

- `init`: the first run of an install, and the same onboarding as an
  app's: the same steps in the same order (`AGENTS.md` §2). The order is
  provider, sign-in when the provider has an account, folder on this
  device, folder in the cloud, sync mode with the one-way question,
  start at login, then a summary. Each step reads its state from what
  is true on the device (a profile exists, a sign-in is stored, the
  service is registered or `autoLaunch` is off), so a later run does
  only what is left and no marker says "initialized". The sign-in comes
  before the folders, stored under the id that the first profile gets,
  so the folder step can look into the account; it stays when the user
  stops, and `auth logout` removes it. With no terminal every answer is
  a flag, `--local` and `--cloud` are required, and no flag answers the
  one-way question. Exit code 0 when initialized, 1 when a step failed
  or waits for a person. Open work: a menu of the cloud's folders in
  place of the typed folder (`docs/tasks/cli.md`).

- `run`: runs the daemon in the current process (useful for Docker / WSL /
  servers / devs). Exits non-zero if another daemon is already attached to
  this `VAPOR_DIR`.
- `service install`: picks the right installer for the host OS
  automatically, for the current user. Configures the OS restart
  policy to match `core/lifecycle::CrashLoopGuard`. It registers two
  jobs: the daemon, which the service manager never restarts, and the
  supervisor (`service check --loop`), which the service manager keeps
  alive and which is the one thing that restarts a crashed daemon. On a
  Linux host with no desktop it turns lingering on, so the daemon keeps
  running after the last logout; `--linger`, which exists only in the
  Linux build, is the user's own answer.
- `service status`: reports `running` / `stopped` / `not-installed` /
  `crash-loop-paused` (the crash-loop pause is overlaid from durable
  lifecycle state) plus `label`, `autoLaunch`, `supervisorInstalled`,
  a `crashLoop` object, `health`, and `answers` with `--json`. `health`
  is what a `check` would find, read with nothing changed; it is what
  an app shows. `answers` is `false` when the daemon's process runs and
  its socket gives no answer, and absent when no process runs.
- `service start` / `restart`: act on the registered service
  definition only. With none registered (auto-launch off, or a host
  that never ran `service install`) both answer `not-installed` with
  exit 0 instead of asking the service manager to kick a job it does
  not have; the text form names the command that registers one.
- `service bootstrap` / `check` / `acknowledge`: the shared lifecycle
  entry points (app startup, the supervision tick that the supervisor
  job runs, and clearing a crash-loop pause). The supervisor ends
  itself when an update replaces its binary; the service manager then
  starts the new one, which replaces a daemon of the old build. All crash-loop policy runs in
  `core/lifecycle` against `<vapor_dir>/state/lifecycle.json`, so
  backoff and pause survive restarts and are shared across surfaces;
  the macOS app shells out to these same subcommands.
- `auth login`: the profile's provider decides what it does. Google
  Drive runs PKCE in the user's browser with a localhost-loopback
  redirect (or reads a token on stdin when there is no terminal); the
  filesystem provider needs no sign-in and the command says so. Tokens
  go through `core/platform/secrets`.
- `status`, `pause`, `resume`, `reconcile`, `timeline`, `diagnostics`:
  talk to a running daemon via the IPC channel
  (`docs/architecture/ipc-contracts.md`). Exit non-zero with a clear message
  if no daemon is running; never hang. `reconcile` is the user's own
  request for a whole-scope scan: it runs under any throttle state but
  `suspended`, and `--json` returns the daemon's ack for the app shim.
- `doctor`: reports platform-specific sanity checks: inotify watch limits on
  Linux, Task Scheduler task presence on Windows, LaunchAgent plist presence
  on macOS, `VAPOR_DIR` permissions, daemon binary location + version.
- `--user-activity=always|never|auto` (planned, not implemented) would
  force a user-activity interpretation on CLI / server hosts. Today the
  daemon detects the absence of a window-server session and treats such a
  host as always idle; `VAPOR_THROTTLE_INPUTS=static` pins neutral inputs.

## 4) Distribution

The CLI has no download of its own. A user installs one thing and
updates one thing: the install package of the platform holds the app,
`vapor`, and `vapord`, and its installer puts `vapor` on the `PATH`
(`AGENTS.md` §1 and §7.5; the decisions and their reasons are in
`docs/specs/2026-11-21-platform-release-assets.md`).

### 4.1 Primary (macOS)

- `Vapor-macos-arm64.pkg` puts `Vapor.app` in `/Applications`, with the
  CLI at `Contents/Helpers/vapor`, and links `/usr/local/bin/vapor` to
  it. There is no Intel Mac build.
- The CLI resolves a link before it looks for `vapord`, so it finds
  the daemon inside the app when it is started through the link, and a
  service definition pins the binary and not the link.
- Signed with the app, under the `release-macos` GitHub Environment.

### 4.2 Deferred (Linux and Windows)

Only shipped when the project owner opts into a non-macOS surface.

- Linux: a `.deb` and an `.rpm` for x64 and for ARM64, with `vapor` in
  `/usr/bin`. The package installs on a host with no desktop, where
  the CLI and the daemon work: the desktop libraries are optional
  dependencies. It may ship before the Linux app window exists, with
  `vapor` and `vapord` only. GPG signature inside the package;
  `release-linux` GitHub Environment.
- Windows: one installer for x64 and one for ARM64. `vapor.exe` gets a
  directory of its own, because `Vapor.exe` cannot share one with it on
  a filesystem that ignores case, and that directory goes on the
  `PATH`. EV-cert signing via `signtool`; `release-windows` GitHub
  Environment.
- A Linux distribution with neither package format builds from source.

## 5) Developer ergonomics

- Local run: `cargo run -p vapor-cli -- <command>`.
- `vapor doctor` is the first command a contributor runs on a new host.
- Install-from-source: `cargo install --path core/cli` (kept working for
  every supported host).

## 6) Definition of done

- Every command works on macOS CI for the primary deliverable;
  Linux/Windows CI validation lands with the matching optional wave.
- `vapor service install` + `vapor run` + `vapor status` pass a full
  round-trip on macOS for the primary deliverable (and on each
  additional OS once its optional wave lands).
- `vapor doctor` detects the known misconfigurations on every shipping
  OS.
- `--json` mode is stable (documented schema; covered by snapshot tests).
- Binary size stays reasonable (single-digit MB once stripped).

## 7) What the CLI deliberately does not do

- It does not ship its own sync engine: it runs the same `core/daemon`.
- It does not persist its own config: it reads/writes `vapor.json`.
- It does not replace GUI diagnostics; it exposes the same information in
  text/JSON form.

## 8) Testing scope (CLI)

Logic + snapshot + integration tests. The full policy lives in
`AGENTS.md §9` and `docs/architecture/testing-strategy.md`; the CLI-
specific scope is:

- **Tested**: argument parsing (`clap`), exit code discipline,
  `--json` output shape via explicit assertion tests (one per `--json`
  command; `insta` snapshots are an open task), IPC client correctness
  against a fake daemon,
  `vapor doctor` detection logic, the `vapor service` round-trip
  (install → start → status → crash-loop supervision → acknowledge →
  stop → uninstall) on macOS CI via the `--full` phase of
  `./scripts/e2e.sh` (and on each additional OS once its optional
  wave lands), "no daemon running" error paths (every IPC-backed
  command exits non-zero within 1 s; never hangs).
- **Not tested**: color codes, cursor positioning, terminal resize
  handling, ncurses or TTY-capability interactions, progress-bar
  rendering timing.

An intentional `--json` schema change updates the matching shape test in
the same change set; CI fails the PR otherwise.
