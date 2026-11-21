# Virtual machine verification (Tier VM)

How a contributor, human or agent, tests the installed Mac app as a
user does: the installer, the first start, the menu bar item, the
windows, and a restart of the Mac. Policy lives in `AGENTS.md` §9.9;
the procedure is the `vapor-vm` skill; the decisions behind the tier
are in `docs/specs/2026-11-21-isolated-macos-e2e.md`.

## Why this tier exists

Tier E2E proves the daemon and the CLI in a sandbox on the host, and it
never starts the app. The app cannot be tested on a contributor's Mac:
at start it registers launchd jobs with fixed labels and a login item,
which would replace those of a real install. CI installs the installer
on a runner and checks what it holds, and nothing started the app
after that. So no test covered the first start, the start at login, or
any screen of the app, and an agent could not see what it built.

Tier VM runs all of that in a macOS virtual machine. The Mac of the
contributor holds nothing of a test.

## What Tier VM is

- **The harness**: `tools/vm`, the `vapor-vm` crate. A dev tool in the
  Cargo workspace that never ships. `./scripts/vm.sh` builds it and
  forwards arguments.
- **The guest**: a macOS 27 virtual machine made with
  [Tart](https://tart.run) from one image, pinned by digest in
  `tools/vm/src/tart.rs`. Every run tests the same guest on every
  machine.
- **The build under test**: the installer that
  `./scripts/build.sh package` writes, unchanged. The app has no test
  hook.
- **The driver**: `tools/vm/driver`, a small Swift tool that runs
  inside the guest only. It reads the controls of the app through the
  accessibility API of macOS and sends pointer clicks, the way a
  user's hand does.
- **A scenario** proves one behaviour. It acts through the driver and
  through the installed `vapor` CLI in the guest, and it keeps a
  screenshot of each step.

A scenario finds a control in a window by its accessibility
identifier (`apps/macos/Sources/VaporCore/AccessibilityID.swift`), so a
change of copy does not break it. The items of the menu bar menu have
no identifier, because macOS builds them from the SwiftUI views and
drops it; a scenario finds one by the text of its key in
`assets/locales/en.json`. A text that a scenario compares comes from
that catalog too, never from a sentence in the scenario.

## Safety contract (hard rules)

1. **The packaged app starts inside a guest only.** The harness never
   starts the app or the installer on the host; both run through
   `tart exec`. `AGENTS.md` §7.1 and §10.1 still forbid a start on the
   host.
2. **A guest sees two host directories and nothing else.** The stage
   directory (the installer, the driver, the e2e binaries) is shared
   read-only. The directory of the run is shared read-write for the
   evidence.
3. **A guest has no window, no sound, and no shared clipboard.**
4. **A guest has no route to the Internet.** Its network reaches the
   host only. One limit stays: a service that the host itself offers
   on that network is reachable from the guest.
5. **A run never downloads.** With no image in Tart's store it stops
   and names `./scripts/vm.sh --setup`, the one command that uses the
   network.
6. **The harness changes only its own VMs.** Every VM it makes is
   named `vapor-vm-*`, and it stops or deletes no other name. A VM of
   the contributor is never touched.
7. **A run deletes every guest it made**, on a pass and on a failure,
   and fails when one is left. Only `--keep` leaves a guest, on
   request.
8. **What stays on the host is known.** The evidence under the
   repo-local `.vapor/vm/`; one empty lock file, `vapor-vm.lock`, in
   the user's temporary directory; and in Tart's store the image and
   the two prepared guests.
9. **No script of the repository runs Homebrew.** The contributor
   installs Tart by hand.

## What a contributor installs, one time

Tier VM needs a Mac with Apple silicon and macOS 27, and Tart 2.33 or
newer.

```sh
brew tap openai/tools
brew trust --formula openai/tools/tart openai/tools/softnet
brew install openai/tools/tart
./scripts/vm.sh --setup
```

`--setup` pulls the image into Tart's store, `~/.tart`: about 32 GB to
download and 36 GB on disk. Then it makes the two prepared guests (see
below).

To remove everything:

```sh
./scripts/vm.sh --clean
tart delete ghcr.io/cirruslabs/macos-golden-gate-base@<the digest in tools/vm/src/tart.rs>
HOMEBREW_NO_AUTOREMOVE=1 brew uninstall openai/tools/tart openai/tools/softnet
```

Set `HOMEBREW_NO_AUTOREMOVE=1` for the uninstall. Without it Homebrew
also removes every formula it counts as an unused dependency, which
has nothing to do with Tart.

## When an agent must run it

- Before it reports as done a change under `apps/macos`, or a change
  to how the app is packaged or installed. This comes after Tier 1,
  and after Tier E2E when the change needs that tier too.
- As a leg of `./scripts/release.sh`, before a release.

It is not a check of a pull request: a GitHub-hosted macOS runner is a
virtual machine, and a macOS guest cannot run inside one.

When a change adds behaviour that a user sees in the app or in the
install, it adds a scenario in the same change set. The scenario fails
once against a build without the change and passes with it.

An agent verifies behaviour. The owner judges the look: the report of
a change to the UI shows the screenshots that matter and says what to
judge. The pull request of such a change carries those screenshots in
its description, and a video when the change moves, so the reviewer
sees what changed.

## Running it

```sh
./scripts/vm.sh                    # build the installer, run every scenario
./scripts/vm.sh --only V02,V11     # by id or name
./scripts/vm.sh --skip-build       # reuse dist/ and the debug binaries
./scripts/vm.sh --keep             # leave the guest of a failed scenario running
./scripts/vm.sh --only V11 --video # also record a video of each scenario
./scripts/vm.sh --guest-stop       # delete every guest of this checkout
./scripts/vm.sh --list             # the catalog
./scripts/vm.sh --clean            # delete this checkout's guests and the prepared guests
```

The run prints one line for each scenario and ends with the path of
`report.json`. The exit code is 0 for a green run, 1 when a scenario
failed, and 2 when the host cannot run the tier.

With `--keep` the guest of a failed scenario stays up as the tier
started it, with no window and no route to the Internet. The run
prints its name. Look at it with `tart exec <guest> ...`, as with a
manual guest, and delete it with `./scripts/vm.sh --guest-stop`. Do not
start a guest of the tier with a plain `tart run`: that opens a window
and gives the guest the Internet.

### Evidence

Everything of a run is under `.vapor/vm/run-<id>/`, which
`./scripts/clean.sh` removes and git ignores:

| File | What it is |
|---|---|
| `report.json` | Each scenario with its verdict, its reasons, and its notes. |
| `<scenario>/NN-<step>.png` | A screenshot of the guest's screen at a named step. |
| `<scenario>/scenario.mov` | With `--video` only: a video of the scenario. A scenario that restarts the guest has `scenario-2.mov` for the time after the restart. |
| `<scenario>/controls.json` | Every control of the app, written at a failure. |
| `<scenario>/logs/`, `status.txt`, `launchd.txt` | The daemon's logs and state from the guest. |
| `<guest>.tart.log` | What Tart printed for a guest. |

A run records no video unless it gets `--video`. Most scenarios show a
screen that does not move, and the screenshots say everything about
it. Ask for a video when the change itself moves: a window that opens,
a transition, a state that changes while you look.

The harness closes the notifications of macOS before a screenshot, so
they do not cover the app. The scenario that asserts a notification
keeps them.

## Guests

- **The prepared guests.** `--setup`, or the first run, makes two: a
  clone of the image that booted once, accepted the screen capture
  question of macOS, and was saved to disk. A scenario never runs in
  one.
- **The start of a guest** is a clone of a prepared guest, which
  resumes in about 10 s. A boot takes about 25 s. After the resume the
  harness mounts the shared directories again and sets the clock of
  the guest from the host: a guest that was saved keeps the clock and
  the view of the shares of the moment it was saved.
- **A fresh guest** has no Vapor. A scenario that tests the install,
  the first start, or the start at login gets one, because a second
  install proves nothing about the first.
- **The shared guest** has the build installed. The scenarios that
  test the screens and the states of the installed app run in it, one
  after the other. Before each of them the harness removes what the
  earlier one left (the service, `~/.vapor`, the sync folders) and
  starts the app again, so their order does not matter.
- **Two guests at a time.** Apple permits two macOS guests on one
  host. The shared guest and the fresh guests run side by side. Each
  has a prepared guest of its own, because two clones of one saved
  guest have the same network address and cannot run together. A run
  waits when a place is taken, also by a run of another checkout. Do
  not run `./scripts/vm.sh --clean` while another checkout runs the
  tier: it deletes the prepared guests that the other run clones.
- **A checkout touches its own guests only.** Tart's store is shared
  by every checkout on the machine, so the name of a guest carries a
  tag made from the path of its checkout.

## Manual guest

For a look at a change while it is built:

```sh
./scripts/vm.sh --guest        # a guest with the build installed, app started
./scripts/vm.sh --guest-stop   # stop and delete every guest of this checkout
```

`--guest` prints the commands that work on the guest: `tart exec` for
a command, the driver for the controls, and `screencapture` for a
screenshot, which lands in `.vapor/vm/manual-<id>/`. The guest takes
the place of the shared guest until it is stopped, and a run of the
suite waits for that place.

## Scenario catalog

`./scripts/vm.sh --list` is the source of truth. The first version:

| Id | Guest | Proves |
|---|---|---|
| V01 `install` | fresh | The installer puts `Vapor.app` in `/Applications` and `vapor` on the `PATH` at the version of the build, and starts nothing. |
| V02 `first-start` | fresh | The first start registers the daemon, its supervisor, and the login item, shows the menu bar item, and opens no window. |
| V03 `start-at-login` | fresh | After a restart of the Mac the app, the daemon, and the supervisor run again, and no window opens. |
| V04 `service-round-trip` | fresh | The launchd round trip of Tier E2E (`R01`) passes with the real launchd. |
| V10 `menu` | shared | A click on the menu bar item opens a menu with a status line and every action; a second click closes it. |
| V11 `main-window` | shared | Open Vapor shows the main window, a second Open Vapor focuses it, and closing it leaves the menu bar item and the daemon. |
| V12 `settings` | shared | A setting changed in the Settings window reaches `vapor.json`. |
| V13 `quit` | shared | Quit Vapor stops the daemon and ends the app, and nothing starts them again. |
| V20 `state-unconfigured` | shared | With no sync profile the app shows that nothing syncs yet, with the daemon's reason. |
| V21 `state-idle` | shared | With a profile and nothing to do the app shows Idle. |
| V22 `state-syncing` | shared | Files that change in the local folder reach the cloud folder, and the app shows the work while it lasts. |
| V23 `state-paused` | shared | After `vapor pause` the app shows Paused. |
| V24 `state-needs-answer` | shared | A one-way mode that the user did not confirm shows Error, the reason, and that one question waits. |
| V25 `state-restart-required` | shared | A setting that needs a restart of the daemon shows the restart notice. |

The app reads the daemon's state on a 30-second timer, so a scenario
that waits for a state on the screen waits up to one read.

## Extending the harness

- A scenario is a function in `tools/vm/src/scenarios/` with an entry
  in that module's `scenarios()`: an id, a kebab-case name, one line
  that says what a pass proves, and the guest it needs.
- Choose the guest by what the scenario tests. A scenario of the
  install or of a first start is `Fresh`. A scenario that looks at the
  installed app is `Shared`, with the start state it needs.
- A new control in a window gets an identifier in
  `AccessibilityID.swift` and a constant in
  `tools/vm/src/scenario.rs`. A Tier 1 test reads every `"id=..."`
  selector in the harness and fails when the app does not define its
  identifier.
- Wait with `wait_until`, with a deadline and a description. A bare
  sleep hides what the scenario waits for.
- Take a screenshot at each step a reader of the evidence wants to
  see: `ctx.shot("name")`.

## What Tier VM does not cover (today)

- **The look.** A scenario proves that a control exists and what it
  says. Whether the screen looks right is the owner's judgement, from
  the screenshots.
- **The Google sign-in.** The guest has no network.
- **Gatekeeper and notarization.** The image has Gatekeeper and System
  Integrity Protection off, and a local build is signed ad hoc.
- **An upgrade** from an older release, and what a removal leaves.
- **The daemon at full pace.** The pointer events of the driver count
  as a user at the device, so the daemon in a guest works at the pace
  of `Throttled` while a scenario acts.
- **Another macOS.** The tier tests on the newest macOS that Tart has
  an image for. The minimum that Vapor supports is not tested.

## Relationship to the other tiers

Tier 1 tests the logic of the app and of the core in process. Tier E2E
tests the real daemon and CLI on the host, on every pull request. Tier
VM tests the installed product on a Mac that is not the contributor's,
for a change to the Mac app or its install and before a release. A behaviour of the daemon belongs in
Tier E2E even when the app shows it; Tier VM proves that the app shows
it.
