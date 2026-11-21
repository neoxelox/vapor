---
name: vapor-vm
description: Runs Vapor's Tier VM verification (the packaged installer installed in a macOS virtual machine made with Tart, the app driven as a user does through accessibility, with screenshots and a video of each scenario under the repo-local `.vapor/vm/`) through the `tools/vm` harness, as the scripted suite, a single scenario, or a manual guest; and adds a scenario for new behaviour of the app or the install. Use before reporting a change under `apps/macos` or to the packaging as done, to see a UI change in the real app, to reproduce an app or install bug, or when a scenario must be written. Needs a Mac with Apple silicon, macOS 27, and Tart with the guest image, which the contributor installs once.
license: GPL-3.0-only
---

## What I do

I run Vapor's Tier VM verification: the installer that
`./scripts/build.sh package` writes, installed in a macOS guest, with
the app driven through the accessibility API of macOS and the
installed `vapor` CLI. The harness is the `vapor-vm` crate in
`tools/vm`; `./scripts/vm.sh` builds the installer, the harness, and
the driver, and forwards arguments. Three modes:

1. **Scenario suite** (`./scripts/vm.sh`): every scenario. The
   scenarios of the installed app share one guest, and the scenarios
   of the install each get a fresh one; two guests run side by side.
2. **One scenario** (`./scripts/vm.sh --only V11`): the loop while
   building or fixing something.
3. **Manual guest** (`./scripts/vm.sh --guest`): a guest with the
   build installed and the app started, to look at a change.

Full process doc: `docs/development/vm-verification.md`. Policy:
`AGENTS.md §9.9`. Catalog: `./scripts/vm.sh --list`.

## When to use me

- Before reporting a change under `apps/macos`, or to how the app is
  packaged or installed, as done. Tier 1 comes first, and Tier E2E
  when the change also needs it.
- While building a UI change: run the one scenario that covers it, or
  look at a manual guest.
- Not needed for a change that the app and the install do not show.

## Safety rules (absolute)

- The packaged app starts inside a guest only. Never open
  `dist/Vapor.app` or the installer on the host, and never run a
  command of a scenario outside `tart exec`.
- Never run Homebrew for the tier. Tart is installed by hand; when it
  is missing, say so and give the commands from the process doc.
- Never pull an image outside `./scripts/vm.sh --setup`, and run
  `--setup` only when the owner asked for it: it downloads 32 GB.
- Change only VMs named `vapor-vm-*`. A VM with another name belongs
  to the contributor.
- Stop what you started: `./scripts/vm.sh --guest-stop` deletes the
  manual guests of this checkout, and `./scripts/clean.sh` deletes
  every guest this checkout made.

## Mode 1 and 2: the suite and one scenario

```
./scripts/vm.sh                    # build + all
./scripts/vm.sh --skip-build       # reuse dist/ and the debug binaries
./scripts/vm.sh --only V02,V11     # by id or name
./scripts/vm.sh --keep             # keep the guest of a failure, stopped
./scripts/vm.sh --json out.json    # the report somewhere else
```

Read the verdict line of each scenario: `PASS` or `FAIL` with the
reason. The run is green only with zero `FAIL`, and it fails when it
leaves a guest behind.

Everything of a run is under `.vapor/vm/run-<id>/`. On a `FAIL` read,
in this order: the reason in `report.json`, the `NN-failure.png`
screenshot, `controls.json` (every control the app had), then
`status.txt` and `logs/` from the guest. Look at the screenshots with
the Read tool; they are the evidence, and a scenario that passes with
a screen that looks wrong is still a finding.

The app reads the daemon's state on a 30-second timer. A scenario that
waits for a state on the screen can take that long; that is not a
hang.

## Mode 3: manual guest

```
./scripts/vm.sh --guest [--skip-build]
```

It prints the name of the guest and the commands that work on it:

```
tart exec <guest> /usr/local/bin/vapor status
tart exec <guest> "/Volumes/My Shared Files/stage/vapor-vm-driver" tree sh.arn.vapor
tart exec <guest> "/Volumes/My Shared Files/stage/vapor-vm-driver" click sh.arn.vapor id=dashboard.sync-now
tart exec <guest> /usr/sbin/screencapture -x "/Volumes/My Shared Files/results/session/shot.png"
```

A screenshot lands in `.vapor/vm/manual-<id>/session/` on the host.
After a change to the app, stop the guest and start a new one: the
guest has the build that was staged when it started. Finish with
`./scripts/vm.sh --guest-stop`.

## Reporting a UI change

An agent verifies behaviour and never judges the look. The report of a
change that a user sees gives the owner:

- the scenarios that ran and their verdicts;
- the screenshots that show the change, by path, embedded in the
  reply when the surface can show them;
- a short list of what to judge: the layout, the wording, the
  alignment with the Apple Human Interface Guidelines.

## Writing a scenario

1. Pick the group file under `tools/vm/src/scenarios/` (install, app,
   states) and add an entry to its `scenarios()` list: the next free
   `Vxx` id, a kebab-case name, one line in `proves`, and the guest.
2. Choose the guest by what the scenario tests. `GuestKind::Fresh` for
   the install, a first start, a start at login, or a removal: the
   scenario installs itself, with `ctx.install()`. For a look at the
   installed app, `GuestKind::Shared` with the start state it needs:
   `Start::Unconfigured`, or `Start::Profile` for one two-way profile
   between two folders of the guest.
3. Write the body as `fn name(ctx: &mut Ctx) -> Result<(), Failure>`.
   Act through `ctx.app()` (`click`, `wait_for`, `wait`) and through
   the CLI (`ctx.vapor_ok("pause")`, `ctx.status()`); assert with
   `ensure!`; take `ctx.shot("step")` at each step a reader wants to
   see.
4. Name a control in a window by its accessibility identifier. A new
   control gets one in
   `apps/macos/Sources/VaporCore/AccessibilityID.swift`, a modifier in
   the view, and a constant in `tools/vm/src/scenario.rs`, listed in
   `APP_IDENTIFIERS`. An item of the menu bar menu has no identifier;
   name it by its catalog key (`ctx.menu_item(key)`).
5. Compare a text with the catalog (`ctx.copy.get(key)`,
   `ctx.wait_text`), never with a sentence in the scenario.
6. Run it alone (`--only Vxx`), then against a build without the
   change: it must fail before and pass after. Quote both runs.
7. Update the catalog table in `docs/development/vm-verification.md`
   (mirror of `--list`).

Rules: bounded waits with named conditions, no bare sleeps (`hold_for`
proves that something stays true); one behaviour per scenario; a
shared scenario must not depend on the one before it.

## Known limits (today)

- The look is the owner's judgement, from the screenshots.
- The guest has no network: no Google sign-in.
- Gatekeeper and System Integrity Protection are off in the image, and
  a local build is signed ad hoc, so the trust chain is not tested.
- One macOS: the newest that Tart has an image for.
- Not on CI: a GitHub-hosted macOS runner cannot run a macOS guest.
