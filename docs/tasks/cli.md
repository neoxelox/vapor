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
- [ ] L1-8 A menu of the cloud's folders in `vapor init`, in place of
      the typed folder: the user picks a folder that exists or makes a
      new one. The sign-in already comes before the folder step for
      it. Needs a short design first: an account with thousands of
      folders, shared drives, and two folders with one name, which
      Google Drive allows. The app onboarding gets the same menu
      (`macos.md` O-1), from the same listing.
- [ ] L1-9 `vapor init` ends with "daemon: unconfigured (no sync
      profile yet; set Vapor up with `vapor init`)" right after it set
      the profile up. The daemon composes the profile a moment later.
      Wait for the daemon to report the profile before that line.
- [ ] L1-10 Times a person can read. `vapor logs` and `vapor timeline`
      print the time as milliseconds since 1970 (`[1795363810332]`),
      and `vapor diagnostics` prints "next attempt at SystemTime {
      tv_sec: 1795364849, tv_nsec: 500000000 }". Print the local time,
      and "in 4 min" for a retry; `--json` keeps the numbers.
- [ ] L1-11 `vapor auth login` waits 5 minutes for the consent and
      then gives up, while the Google page in the browser stays open
      and looks alive. A person who signs in on it later lands on a
      browser error, because nothing listens on the redirect port any
      more. Wait longer, and answer a late redirect with a page that
      says "run `vapor auth login` again". For a profile that never
      signed in, `vapor status` says "Vapor needs to sign in to gdrive
      again"; drop "again" there.
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
- [ ] L2-8 `vapor service start` says "service: daemon started" and
      exits 0 when a daemon that no longer answers still holds the
      launchd job, and `vapor service status` says "Running" for it
      (`core.md` PL-4). Both must ask the daemon on its socket, and a
      `restart` that could not stop the old daemon must say that sync
      is down and what to do.

## Headless and server use

- [ ] L5-2 Dockerfile + example `docker-compose.yml` demonstrating
      `vapor run` in a container.
- [ ] L5-3 `core/cli/README.md` deployment recipes: a server set up
      with `vapor init` and its flags (the systemd user unit, the
      supervisor, lingering), and Docker (`vapor init --auto-launch
      off`, then `vapor run`).
- [ ] L5-4 `vapor doctor` on Linux says when lingering is off on a host
      with no desktop: the daemon then stops at the last logout.
      `vapor init` reports a refusal one time, in its summary; a later
      run does not look again.

## Distribution

The CLI has no download of its own: it ships inside the install
package of each platform, which puts it on the `PATH` (`AGENTS.md`
§7.5, `docs/specs/2026-11-21-platform-release-assets.md`). The macOS
installer is in place. The Linux packages are `docs/tasks/core.md`
C7-10, and the Windows installer is C6-11.

## Tests

Policy: `AGENTS.md §9`. Full taxonomy:
`docs/architecture/testing-strategy.md`.

- [ ] LT-1 `insta` snapshots for the `--json` shape locks, one per
      command with a fixed fixture (`core.md` CT-4). Explicit
      field-by-field tests lock the shapes today.
- [ ] LT-2 Exit codes are documented and stable. A `--help`-style
      snapshot or a direct exit-code assertion test per command.
- [ ] LT-3 Binary size stays reasonable (single-digit MB stripped).
      CI guard-rail, not a soft target.
- [ ] LT-5 IPC client correctness tested against a fake daemon that
      speaks the IPC protocol from `docs/architecture/ipc-contracts.md`.
      Covers every IPC-backed command; tests the "no daemon running"
      error path (non-zero exit within 1 s; never hang).
