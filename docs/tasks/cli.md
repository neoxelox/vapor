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

- [ ] L1-8 A menu of the cloud's folders in `vapor init`, in place of
      the typed folder: the user picks a folder that exists or makes a
      new one. The sign-in already comes before the folder step for
      it. Needs a short design first: an account with thousands of
      folders, shared drives, and two folders with one name, which
      Google Drive allows. The app onboarding gets the same menu
      (`macos.md` O-1), from the same listing.
- [ ] L1-9 Change one profile from the command line. The folders, the
      provider, and the mode of a profile are in its entry of the
      `profiles` list once the install has a list (a second profile,
      or a `disable` or a `rename` of the first one). `vapor config
      set syncMode` then sets the value for a profile that has none of
      its own, and says which profiles it did not change, but no
      command changes the entry: the user edits `vapor.json` or sets
      the whole list (`vapor config set profiles '<json>'`). A command
      such as `vapor profiles set <id> <key> <value>` closes that, and
      `vapor config get localRoot` on such an install prints an empty
      line today. Found by the review of the pull request, 2026-11-24.
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
