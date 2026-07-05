# Vapor CLI plan

Core runtime plan: `docs/plans/core.md`
Execution checklist: `docs/tasks/cli.md`

## 0) Scope

This plan covers the `vapor` CLI binary (source in `core/cli`, ships alongside
the daemon on every supported OS).

Binary names are fixed:

- **`vapor`** — the CLI.
- **`vapord`** — the daemon.

## 1) Mission

`vapor` is the reference consumer of the portable runtime and the universal
control plane every other app can shell out to. The primary shipping target
is macOS, alongside the macOS app. Linux and Windows CLI binaries are
deferred behind the optional platform-impl waves in
`docs/tasks/README.md`; the crate is designed so they cost only a
recompile + CI job once those waves land, not a rewrite.

## 2) Non-negotiables

1. **Headless-first.** No GUI dependencies; works over SSH, in Docker, and in
   CI. No interactive-TTY requirements — every command has a deterministic
   non-interactive code path.
2. **Parity by default.** Every runtime capability exposed in a GUI app must
   also be reachable from the CLI. If a feature is CLI-only, it must be
   because GUI ergonomics would hurt it, not because someone forgot to wire
   it.
3. **Scriptable.** Every command supports `--json` output; exit codes are
   stable and documented.
4. **Per-OS native when it matters.** The CLI inherits platform-native
   implementations from `core/platform` — it uses FSEvents on macOS, inotify
   on Linux, ReadDirectoryChangesW on Windows. No "portable but slow" path.
5. **Respects the same config surface as GUI apps.** Reads `VAPOR_DIR`,
   `VAPOR_ENV`, `vapor.json`. Writes and reads the same locale catalogs.

## 3) Command surface

```
vapor run [--foreground]                # run the daemon in-process
vapor service install [--user|--system] # native ServiceInstaller
vapor service uninstall [--keep-running]
vapor service bootstrap                 # install+start only when autolaunch is enabled
vapor service start|stop|restart|status
vapor service check                     # one crash-loop supervision tick
vapor service acknowledge               # clear a crash-loop pause
vapor config get|set <key> [value]      # edits vapor.json
vapor auth login <provider>             # OAuth PKCE via localhost loopback
vapor auth logout <provider>
vapor status [--json]                   # queue depth, throttle state, reason
vapor pause|resume
vapor flush-now                         # force-flush pending intents
vapor reconcile                         # request whole-scope reconcile
vapor timeline [--tail] [--json]        # diagnostics timeline
vapor logs [--tail] [--level=debug]
vapor doctor                            # checks inotify watches, permissions, etc.
vapor version
```

### 3.1 Rules per command

- `run` — runs the daemon in the current process (useful for Docker / WSL /
  servers / devs). Exits non-zero if another daemon is already attached to
  this `VAPOR_DIR`.
- `service install` — picks the right installer for the host OS
  automatically; `--user` (default) vs `--system` switch. Configures the OS
  restart policy to match `core/lifecycle::CrashLoopGuard`.
- `service status` — reports `running` / `stopped` / `not_installed` /
  `crash_loop_paused` (the crash-loop pause is overlaid from durable
  lifecycle state) plus `label`, `auto_launch`, and a `crash_loop`
  object with `--json`.
- `service bootstrap` / `check` / `acknowledge` — the surfaces' shared
  lifecycle entry points (app startup, the periodic supervision tick,
  and clearing a crash-loop pause). All crash-loop policy runs in
  `core/lifecycle` against `<vapor_dir>/state/lifecycle.json`, so
  backoff and pause survive restarts and are shared across surfaces;
  the macOS app shells out to these same subcommands.
- `auth login <provider>` — runs PKCE in the user's browser with a
  localhost-loopback redirect; stores tokens via `core/platform/secrets`.
- `status`, `pause`, `resume`, `flush-now`, `reconcile` — talk to a running
  daemon via the IPC channel (`docs/architecture/ipc-contracts.md`). Exit
  non-zero with a clear message if no daemon is running; never hang.
- `doctor` — reports platform-specific sanity checks: inotify watch limits on
  Linux, Task Scheduler task presence on Windows, LaunchAgent plist presence
  on macOS, `VAPOR_DIR` permissions, daemon binary location + version.
- `--user-activity=always|never|auto` — on CLI / server hosts without HID
  signal, force a specific user-activity interpretation. Default `auto`:
  always-idle on headless hosts, event-driven on desktop hosts.

## 4) Distribution

Aligned with the prioritization in `docs/tasks/README.md`: the CLI ships
on macOS first as part of the primary deliverable. Linux and Windows
binaries are deferred and gated on the optional waves 12–14.

### 4.1 Primary (macOS)

- Pure Rust binary. Target triples:
  - `aarch64-apple-darwin`
  - `x86_64-apple-darwin`
- Packaged as zstd-compressed tarballs with SHA256 checksums.
- Signed with the shared macOS Developer ID identity (shares
  `release-macos` GitHub Environment secrets with the app bundle).
- Published to GitHub Releases under the same tag as the macOS app
  bundle.

### 4.2 Deferred (Linux and Windows)

Only shipped when the project owner opts into a non-macOS surface. Each
platform's CLI distribution rides the matching platform-impl wave in
`core.md §10` (Windows ⇒ wave 9; Linux ⇒ wave 10).

- Linux targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`
  (musl variants a stretch goal for Alpine/Docker users). GPG signature
  + checksum; `release-linux` GitHub Environment.
- Windows targets: `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`.
  EV-cert signing via `signtool`; `release-windows` GitHub Environment.
- Published under the same GitHub Release tag as whichever other
  artifacts ship that cycle.

## 5) Developer ergonomics

- Local run: `cargo run -p vapor -- <command>`.
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

- It does not ship its own sync engine — it runs the same `core/daemon`.
- It does not persist its own config — it reads/writes `vapor.json`.
- It does not replace GUI diagnostics; it exposes the same information in
  text/JSON form.

## 8) Testing scope (CLI)

Logic + snapshot + integration tests. The full policy lives in
`AGENTS.md §9` and `docs/architecture/testing-strategy.md`; the CLI-
specific scope is:

- **Tested** — argument parsing (`clap`), exit code discipline,
  `--json` output schema via `insta` snapshots (one snapshot per
  `--json` command), IPC client correctness against a fake daemon,
  `vapor doctor` detection logic, the `vapor service` round-trip
  (install → start → status → crash-loop supervision → acknowledge →
  stop → uninstall) on macOS CI via the `--full` phase of
  `./scripts/e2e.sh` (and on each additional OS once its optional
  wave lands), "no daemon running" error paths (every IPC-backed
  command exits non-zero within 1 s; never hangs).
- **Not tested** — color codes, cursor positioning, terminal resize
  handling, ncurses or TTY-capability interactions, progress-bar
  rendering timing.

Snapshot review flow: `cargo insta review` after any intentional
`--json` schema change; CI fails the PR otherwise.
