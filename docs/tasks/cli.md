# Vapor CLI task list

Plan reference: `docs/plans/cli.md`
Core tasks (runtime + platform): `docs/tasks/core.md`

Status legend:

- `[ ]` pending
- `[~]` in progress
- `[x]` complete

Binary names are fixed:

- **`vapor`** — the CLI.
- **`vapord`** — the daemon.

## Phase L0 - Crate and build skeleton

- [x] L0-1 Add `core/cli` to the Cargo workspace as a new binary crate named
      `vapor` with a single entrypoint `core/cli/src/main.rs`.
- [x] L0-2 Pick CLI dependencies: `clap` (args/subcommands), `serde_json`
      (JSON output mode), `indicatif` (progress), `crossterm` (terminal
      capability detection). Keep the dep list small. *(`indicatif` and
      `crossterm` are deferred — no progress / TTY rendering surface
      ships in Wave 6.)*
- [x] L0-3 Add workspace-level script entrypoints: `scripts/cli/build.sh`,
      `scripts/cli/test.sh`, consistent with the existing Rust wrapper
      discipline.
- [x] L0-4 Add `vapor --version` (uses the same build-info macro as
      `vapord`).
- [x] L0-5 Add `install-from-source` instructions to `core/cli/README.md`
      (`cargo install --path core/cli`).

## Phase L1 - Core commands (no daemon IPC yet)

Depends on: `docs/tasks/core.md` C1–C3.

- [x] L1-1 `vapor run [--foreground]` — starts the daemon in-process using
      the existing `DaemonRuntime::start` surface from `core/daemon`. Exits
      non-zero if another daemon is already attached to this `VAPOR_DIR`
      (detect via a lock file under `<vapor_dir>/vapord.lock`). *(Lock
      file detection: deferred — `DurableStateDb` already surfaces a
      conflict when two processes try to open the same SQLite file in
      WAL mode, which gives us the exit-non-zero behavior for free.)*
- [x] L1-2 `vapor config get|set <key> [value]` — reads/writes `vapor.json`
      using the same shape the macOS Swift app uses. Must never clobber
      unknown keys.
- [x] L1-3 `vapor version` — prints version + git commit short.
- [x] L1-4 `vapor doctor` — platform-aware sanity checks:
      - `VAPOR_DIR` exists + is writable + has private permissions on Unix.
      - Daemon binary `vapord` is discoverable (PATH or sibling).
      - On macOS: LaunchAgent plist presence.
      - On Linux: `/proc/sys/fs/inotify/max_user_watches` value + advice.
      - On Windows: Task Scheduler task presence.
      *(macOS-flavor checks ship in Wave 6; Linux + Windows checks land
      with Waves 13 / 12.)*

## Phase L2 - Service lifecycle (autolaunch on every OS)

Depends on: `docs/tasks/core.md` C3-3 (`ServiceInstaller` + macOS impl) and
C4 (lifecycle moves to Rust).

- [x] L2-1 `vapor service install [--user|--system]` — invokes the
      platform-appropriate `ServiceInstaller`. Default `--user`.
      *(`--system` flag deferred — only `--user` ships in Wave 6.)*
- [x] L2-2 `vapor service uninstall` — reverse of install.
- [x] L2-3 `vapor service start|stop|restart` — drives the installed
      service.
- [x] L2-4 `vapor service status` — reports `Running` / `Stopped` /
      `CrashLoopPaused` / `NotInstalled` with a human reason.
- [ ] L2-5 Automated end-to-end test on macOS CI: install → start → status
      → stop → uninstall round-trip.
- [ ] L2-6 Same round-trip automated on Linux CI (systemd user unit) once
      `docs/tasks/core.md` C7-2 lands.
- [ ] L2-7 Same round-trip automated on Windows CI (Task Scheduler) once
      `docs/tasks/core.md` C6-2 lands.

## Phase L3 - IPC-driven commands (talks to a running daemon)

Depends on: `docs/tasks/core.md` C5 (IPC channel).

- [ ] L3-1 `vapor status [--json]` — queue depth, throttle state, reason,
      effective ceilings, utilization, idle-boost state. JSON schema
      documented in `docs/architecture/ipc-contracts.md`.
- [ ] L3-2 `vapor pause` / `vapor resume` — control endpoints.
- [ ] L3-3 `vapor flush-now` — force-flush pending intents.
- [ ] L3-4 `vapor reconcile` — request whole-scope reconcile.
- [ ] L3-5 `vapor timeline [--tail] [--json]` — streams the diagnostics
      timeline.
- [ ] L3-6 `vapor logs [--tail] [--level=debug]` — tails
      `<vapor_dir>/logs/vapord.logs` with the existing log line format and
      redaction rules.
- [ ] L3-7 Consistent behavior when no daemon is running: every IPC-backed
      command exits non-zero within 1s with `vapor: daemon not running —
      try \`vapor service start\``, never hangs.

## Phase L4 - Auth flows

Depends on: `docs/tasks/core.md` C3-4 (`SecretStore` + macOS impl) and
C8-48 (Google Drive provider).

- [ ] L4-1 `vapor auth login <provider>` — starts OAuth PKCE, opens the
      user's browser, listens on a localhost loopback, stores tokens via
      `core/platform/secrets`. Also supports `--no-browser` for headless
      flows (prints URL, accepts pasted auth code).
- [ ] L4-2 `vapor auth logout <provider>` — removes tokens for the named
      provider (or all providers with `--all`).
- [ ] L4-3 `vapor auth status` — lists bound providers/accounts without
      revealing tokens.

## Phase L5 - Headless / server ergonomics

- [ ] L5-1 `--user-activity=always|never|auto` global flag that sets the
      interpretation for hosts without HID signal (default `auto`:
      always-idle on headless, event-driven on desktop).
- [ ] L5-2 Dockerfile + example `docker-compose.yml` demonstrating
      `vapor run` in a container.
- [ ] L5-3 `core/cli/README.md` deployment recipes: systemd user unit via
      `vapor service install --user`, system unit via `--system`, Docker.

## Phase L6 - Distribution

Depends on: `docs/tasks/core.md` C6-8 and C7-7 (per-OS trust chains).

- [ ] L6-1 Release-pipeline jobs that build `vapor` per supported target
      triple:
      - `aarch64-apple-darwin`, `x86_64-apple-darwin`
      - `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`
      - `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`
- [ ] L6-2 Strip + zstd-compress + SHA256 checksum each asset.
- [ ] L6-3 Publish to the same GitHub Release tag as the macOS app bundle.
- [ ] L6-4 Sign macOS binaries with Developer ID (shares the macOS trust
      chain from `docs/operations/macos/distribution-trust-chain.md`).
- [ ] L6-5 Sign Windows binaries with the EV cert from
      `docs/operations/windows/distribution-trust-chain.md` when C6-8 lands.
- [ ] L6-6 GPG-sign Linux binaries + publish `Checksums.txt.asc` when
      C7-7 lands.

## Phase LT - Testing discipline (CLI)

Policy: `AGENTS.md §9`. Full taxonomy:
`docs/architecture/testing-strategy.md`. CLI-specific scope lives in
`docs/plans/cli.md §8`.

- [ ] LT-1 `vapor --json` schema is stable and covered by `insta`
      snapshot tests. One snapshot per `--json` command, with a fixed
      input fixture. Schema drift fails the PR; intentional changes
      reviewed with `cargo insta review`. Applies to every command
      from L1 onward; tracked as a standing requirement per `core.md`
      CT-8.
- [ ] LT-2 Exit codes are documented and stable. A `--help`-style
      snapshot or a direct exit-code assertion test per command.
- [ ] LT-3 Binary size stays reasonable (single-digit MB stripped +
      zstd'd). CI guard-rail, not a soft target.
- [ ] LT-4 End-to-end flows (`install → start → status → stop →
      uninstall`) pass on macOS CI for the primary deliverable
      (`core.md` L2-5). Linux and Windows CI validation lands with
      the matching optional wave (L2-6, L2-7).
- [ ] LT-5 IPC client correctness tested against a fake daemon that
      speaks the IPC protocol from `docs/architecture/ipc-contracts.md`.
      Covers every IPC-backed command; tests the "no daemon running"
      error path (non-zero exit within 1 s; never hang).
- [ ] LT-6 **Do not add interactive TTY tests.** No color-code
      assertions, no cursor-positioning assertions, no terminal-resize
      simulations, no progress-bar rendering timing. The reviewer
      cites `AGENTS.md §9.3`.
