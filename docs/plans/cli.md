# Vapor CLI plan

Core runtime plan: `docs/plans/core.md`
Execution checklist: `docs/tasks/cli.md`

## 0) Scope

This plan covers the `vapor` CLI binary (source in `core/cli`, ships alongside
the daemon on every supported OS).

Binary names are fixed:

- **`vapor`**: the CLI.
- **`vapord`**: the daemon.

## 1) Mission

`vapor` is the reference consumer of the portable runtime and the universal
control plane every other app can shell out to. The primary shipping target
is macOS, alongside the macOS app. Linux and Windows CLI binaries are
deferred behind the optional platform-impl waves in
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
vapor status [--json]                   # what the daemon does and why
vapor pause|resume [--json]
vapor reconcile [--json]                # scan now: any throttle state but suspended, plus the boost
vapor profiles list|add|remove          # the folders Vapor syncs; the first add is the setup
vapor decisions list|show|resolve       # conflicts and parked questions (resolve --choose <option>)
vapor ignored [--profile <id>] [--json] # what the ignore rules keep out of sync, with the rule
vapor trash list|restore|empty          # what Vapor removed from this device (empty asks, or --yes)
vapor config get|set <key> [value]      # edits vapor.json
vapor auth login [--profile <id>]       # signs the profile in to its provider (--token - reads stdin)
vapor auth logout [--profile <id>]
vapor auth status [--profile <id>] [--json]  # signed-in / signed-out / not-needed, never the token
vapor service bootstrap                 # install+start only when autolaunch is enabled
vapor service install [--supervise]     # native ServiceInstaller
vapor service uninstall [--keep-running]
vapor service start|stop|restart|status
vapor service check [--loop]            # one crash-loop supervision tick
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

- `run`: runs the daemon in the current process (useful for Docker / WSL /
  servers / devs). Exits non-zero if another daemon is already attached to
  this `VAPOR_DIR`.
- `service install`: picks the right installer for the host OS
  automatically, for the current user. Configures the OS restart
  policy to match `core/lifecycle::CrashLoopGuard`.
- `service status`: reports `running` / `stopped` / `not-installed` /
  `crash-loop-paused` (the crash-loop pause is overlaid from durable
  lifecycle state) plus `label`, `autoLaunch`, and a `crashLoop`
  object with `--json`.
- `service start` / `restart`: act on the registered service
  definition only. With none registered (auto-launch off, or a host
  that never ran `service install`) both answer `not-installed` with
  exit 0 instead of asking the service manager to kick a job it does
  not have; the text form names the command that registers one.
- `service bootstrap` / `check` / `acknowledge`: the surfaces' shared
  lifecycle entry points (app startup, the periodic supervision tick,
  and clearing a crash-loop pause). All crash-loop policy runs in
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

Aligned with the prioritization in `docs/tasks/README.md`: the CLI ships
on macOS first as part of the primary deliverable. Linux and Windows
binaries are deferred and gated on the optional Waves 12 to 14 in
`docs/tasks/README.md`.

### 4.1 Primary (macOS)

- Pure Rust binary. Target triple: `aarch64-apple-darwin`. There is
  no Intel Mac build.
- Packaged as zstd-compressed tarballs with SHA256 checksums.
- Signed with the shared macOS Developer ID identity (shares
  `release-macos` GitHub Environment secrets with the app bundle).
- Published to GitHub Releases under the same tag as the macOS app
  bundle.

### 4.2 Deferred (Linux and Windows)

Only shipped when the project owner opts into a non-macOS surface. Each
platform's CLI distribution rides the matching platform-impl wave in
`docs/tasks/README.md` (Windows: Wave 12; Linux: Wave 13; the cross-OS
CLI distribution itself: Wave 14).

- Linux targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`
  (musl variants a stretch goal for Alpine/Docker users). GPG signature
  + checksum; `release-linux` GitHub Environment.
- Windows targets: `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`.
  EV-cert signing via `signtool`; `release-windows` GitHub Environment.
- Published under the same GitHub Release tag as whichever other
  artifacts ship that cycle.

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
- Binary size stays reasonable (single-digit MB once stripped + zstd'd).

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
