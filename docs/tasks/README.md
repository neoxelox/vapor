# Tasks

Execution checklists mapped to the plans in `docs/plans/`. This directory
serves two purposes:

1. **Per-surface task lists**: one file per deliverable surface, each
   tracking concrete items with `[ ] / [~] / [x]` status.
2. **Cross-surface roadmap**: this README is the orchestrator. It tells
   you **what should be done next**, which tasks block which, and how
   the waves line up.

If you only have time to read one doc before picking up work, read this
one.

## File convention

Tasks are flat and platform-named: one file per deliverable surface.

- `core.md`: portable Rust runtime tasks. The biggest file; everything
  that powers every app surface lives here.
- `macos.md`: macOS app surface tasks.
- `cli.md`: `vapor` CLI tasks.
- `windows.md`: placeholder (only created if `apps/windows` is started).
- `linux.md`: placeholder (only created if `apps/linux` is started).

Every file mirrors its plan in `docs/plans/<surface>.md`. Plans are
*intent*; these files track *execution*.

Status legend (used in every task file):

- `[ ]` pending
- `[~]` in progress
- `[x]` complete

## Scope and prioritization

**The focus is a very good, performant, and polished core runtime, CLI,
and macOS app.** Everything else is deferred or optional.

- **Primary path**: the core runtime (`core/*`), the `vapor` CLI
  running on macOS, and the macOS app: shipped, polished, and
  maintained. The portable foundation stays part of it: `core/*`
  builds and passes its tests on every OS in CI, and OS-specific code
  sits behind the `core/platform` traits.
- **Deferred / optional**: Windows and Linux app surfaces, the Windows
  platform implementations, and cross-OS CLI distribution. Work in
  this bucket starts only when the project owner decides to ship a
  non-macOS surface.

The task files hold open work only. What already landed is in
`CHANGELOG.md` and the git history, and the decisions behind it are in
`docs/specs/`.

## What is next

A wave is a coherent set of tasks across surfaces that lands together.
Within a wave, tasks can be worked in parallel unless a dependency is
called out. The runtime, the CLI, and the first Mac app are in place;
the waves below are what remains, in order.

### Before the next wave: follow-ups of the 2026-11-14 review

Status: open. The review (`docs/specs/2026-11-14-full-review.md`)
fixed what it found, and left a short list that is not built and a
list of choices that wait for the owner.

- Engine: `core.md` E-1 … E-5.
- Providers: `core.md` P-1 … P-5. P-4 and P-5 need the owner: a Drive
  test account, and the Google verification decision.
- Platform: `core.md` PL-1, PL-2.
- The open points of the spec: each one is a question with the choice
  the build made in the meantime.

None of them blocks the Mac app work.

### Wave 9: macOS app UX

Status: pending and unblocked.

- `macos.md` M3-2 … M3-15: the menubar state model, the diagnostics
  panel, the per-item "why stuck" view, the live timeline tab, the
  decisions pane (conflicts included), the trash pane, the root-state
  display, notifications, and the ignored-paths view. `core.md` C8-71
  (per-path timeline detail) is the one open core dependency, only for
  naming files inside notifications.
- `macos.md` M4-1 … M4-5: profiles UX: create, rename, select, enable,
  disable, and delete flows; profile-scoped settings (including the
  per-profile `syncMode` selector and its one-way question); safe
  disconnect; multi-provider fan-out. Needs `cli.md` L1-7 for the
  enable, disable, and rename commands.

### Wave 9.5: First run and onboarding

Status: pending. The runtime and the CLI side landed: an unconfigured
install creates nothing, and `vapor profiles add` sets the first
profile up with no restart.

- `macos.md` O-1 … O-4: the first-run flow: provider for the first
  profile, sign-in, roots, sync mode, start at login; the gate that
  opens it while the install is unconfigured; re-entry from Settings
  for a second profile; copy, logic tests, and the owner's manual
  checklist.

The project owner's decision behind it: most real installs will use
Google Drive, so a pair of filesystem folders created before anyone
chose is the wrong first impression.

### Wave 10: macOS distribution hardening

Status: pending.

- `macos.md` M5-1 … M5-4: signed + notarized end-to-end verification
  on a clean macOS host every release cycle, entitlements drift check,
  LaunchAgent/login-item stability across upgrades (N-1 → N), rollback
  artifact preservation.

### Wave 11: CLI distribution (macOS)

Status: pending. Depends on Wave 10 (the macOS signing identity and
the `release-macos` GitHub Environment are shared).

- `cli.md` L5-1 … L5-3: headless / server ergonomics
  (`--user-activity`, Docker recipe, deployment recipes).
- `cli.md` L6-1 … L6-4 for `aarch64-apple-darwin`: build, strip + zstd
  + SHA256, sign with Developer ID (shares the macOS trust chain),
  publish alongside the macOS app bundle under the same GitHub Release
  tag. The Mac builds are Apple silicon only.

After this wave, the primary deliverable is complete: polished core +
CLI + macOS app, all under one release tag.

## Deferred / optional: Windows and Linux (waves 12+)

**Start condition:** the project owner explicitly decides to ship a
non-macOS surface. Until then, these waves do not progress. Nothing in
the primary path is blocked by any of them.

Picking any of these up is a matter of filling in native
implementations behind a stable trait surface, not a rewrite.

### Wave 12: Windows platform implementations (optional)

- `core.md` C6-1 … C6-9: Windows native impls for every trait
  (ReadDirectoryChangesW, Task Scheduler, Credential Manager,
  power/thermal signals, `GetLastInputInfo`, NTFS ADS, console-ctrl),
  the Windows distribution trust chain doc, and the named-pipe IPC
  transport.
- `cli.md` L2-7: `vapor service install` round-trip automated on
  Windows CI.

### Wave 13: Linux as a shipping surface (optional)

The native traits are in: the daemon runs on Linux with inotify, a
systemd user unit, the freedesktop trash, `/proc` and `/sys` throttle
inputs, and a secret store that is either the `VAPOR_SECRETS_COMMAND`
shim or the desktop Secret Service. Left before Linux ships a surface:

- `core.md` C7-7: the Linux trust chain doc; then the
  `release-linux` environment's secrets (the environment itself exists,
  protected like `release-macos`).
- `cli.md` L2-6: `vapor service install` round-trip automated on
  Linux CI (a `systemctl --user` session on the runner).
- `core.md` C3-11 (Linux part): the interface speed, display-server
  idle, PSI.
- `core.md` TR-10: the rest of the Linux soak.

### Wave 14: Cross-OS CLI distribution (optional)

Depends on whichever of Waves 12, 13 has shipped.

- `cli.md` L6-1 remaining targets (`x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`,
  `aarch64-pc-windows-msvc`).
- `cli.md` L6-5: Windows EV-cert signing.
- `cli.md` L6-6: GPG-signed Linux binaries + `Checksums.txt.asc`.

### Wave 15: `apps/windows` app shell (optional)

Only if the project owner decides to ship a Windows GUI. Creates
`docs/plans/windows.md` and `docs/tasks/windows.md` as new surfaces
with their own plan + task list; picks the UI tech (WinUI 3, WPF, or
Tauri, see `docs/plans/core.md §8`); the `release-windows` GitHub
Environment (already created and protected) loaded with EV cert secrets
and a `package` matrix entry in `release.yml`. The icon family is already
in place under `assets/windows/` (ICO, PNG, tray glyphs, the MSIX asset
set with its manifest fragment); see `assets/README.md`.

### Wave 16: `apps/linux` app shell (optional)

Only if the project owner decides to ship a Linux GUI. Creates
`docs/plans/linux.md` and `docs/tasks/linux.md` as new surfaces; picks
the UI tech (GTK4-rs, Qt, or Tauri); the `release-linux` GitHub
Environment (already created and protected) loaded with GPG key secrets
and a `package` matrix entry in `release.yml`; AppImage first, then
`.deb` / `.rpm` / Flatpak / Snap as demand surfaces. The icon theme
tree, the desktop entry, and an icon install script are already in place
under `assets/linux/`; see `assets/README.md`.

## Runs continuously: tests, tools, operations

Not a wave. `core.md` "Tests and tools" and "Operations" hold work
that lands when its trigger comes: the Drive leg of the harness when
the test account exists (TR-8, TR-12), fuzz and `loom` in the Tier 2
gate (CT-9, CT-10), the runbooks (R-1). The standing invariants that
every change must keep (crash and restart recovery, throttle
discipline, a light watcher callback, echo suppression, the conflict
policy, scope safety) are rules, not tasks: `AGENTS.md` §9.2 and §11
state them, and `docs/architecture/testing-strategy.md` says which
tier proves each one.

## How to pick up a task

1. Read `docs/plans/README.md` and the matching `docs/plans/<surface>.md`
   to understand intent, and the spec of the area under `docs/specs/`
   when one exists.
2. Consult the wave list above to see which waves are open and which
   tasks are unblocked.
3. Only touch Wave 12+ work if the project owner has explicitly
   opted into a non-macOS surface.
4. Move the task to `[~]` in its task file when you start. Remove it
   from the file in the change that completes it, with its
   `CHANGELOG.md` line.
5. Update this README when your work closes a wave or unblocks a new
   one.
