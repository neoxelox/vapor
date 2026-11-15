# Vapor CLI task list

Plan reference: `docs/plans/cli.md`
Core tasks (runtime + platform): `docs/tasks/core.md`

Status legend:

- `[ ]` pending
- `[~]` in progress
- `[x]` complete

Binary names are fixed:

- **`vapor`**: the CLI.
- **`vapord`**: the daemon.

This file holds open work only. A task that is done leaves the file:
`CHANGELOG.md` and the git history record what landed. Task ids are
stable and are never reused.

## Commands

- [ ] L1-7 `vapor profiles enable|disable|rename`: the rest of what the
      app's profile screens need (`macos.md` M4-1). `list`, `add`, and
      `remove` ship.
- [ ] L5-1 `--user-activity=always|never|auto` global flag that sets the
      interpretation for hosts without HID signal (default `auto`:
      always-idle on headless, event-driven on desktop).

## Service lifecycle

- [ ] L2-6 The `vapor service` round-trip (install, start, status,
      crash-loop supervision, acknowledge, stop, uninstall) automated on
      Linux CI against a systemd user unit. The e2e scenario needs a
      `systemctl --user` session on the runner, which `ubuntu-latest`
      does not give a plain job.
- [ ] L2-7 The same round-trip automated on Windows CI (Task Scheduler)
      once `docs/tasks/core.md` C6-2 lands.

## Headless and server use

- [ ] L5-2 Dockerfile + example `docker-compose.yml` demonstrating
      `vapor run` in a container.
- [ ] L5-3 `core/cli/README.md` deployment recipes: a systemd user unit
      through `vapor service install --supervise`, and Docker.

## Distribution

Depends on: `docs/tasks/core.md` C6-8 and C7-7 (per-OS trust chains).
The Mac app is built for Apple silicon; there is no Intel Mac build of
the app, and no `x86_64-apple-darwin` CLI either.

- [ ] L6-1 Release-pipeline jobs that build `vapor` per supported target
      triple:
      - `aarch64-apple-darwin`
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

## Tests

Policy: `AGENTS.md §9`. Full taxonomy:
`docs/architecture/testing-strategy.md`.

- [ ] LT-1 `insta` snapshots for the `--json` shape locks, one per
      command with a fixed fixture (`core.md` CT-4). Explicit
      field-by-field tests lock the shapes today.
- [ ] LT-2 Exit codes are documented and stable. A `--help`-style
      snapshot or a direct exit-code assertion test per command.
- [ ] LT-3 Binary size stays reasonable (single-digit MB stripped +
      zstd'd). CI guard-rail, not a soft target.
- [ ] LT-5 IPC client correctness tested against a fake daemon that
      speaks the IPC protocol from `docs/architecture/ipc-contracts.md`.
      Covers every IPC-backed command; tests the "no daemon running"
      error path (non-zero exit within 1 s; never hang).
