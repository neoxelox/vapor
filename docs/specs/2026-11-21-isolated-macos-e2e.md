# Tests of the Mac app in a virtual machine

Status: implemented 2026-11-21

## Goal

The project owner asked for a way that an agent builds a feature and
then tests the whole product as a user does: the CLI, the daemon, and
the Mac app. In the owner's words: "test whatever you need to test,
and then tear it down", "make screenshots or videos to show me later",
and the tests "should not interfere with any application or data or
folder, other than the temporary folder for that test".

What the owner gets: an agent verifies a change to the app or to the
install with the real product, and its report shows screenshots. The
Mac that the agent works on holds nothing from a test.

## Facts

What was true when the design started (`main` at `31d3d2a`, version
`0.2.0-alpha.3`). The design machine is an Apple silicon Mac with 12
cores and 32 GB of memory, on macOS 27.2.1 with Xcode 27.2.

### The repository

- Tier E2E runs the real `vapor` and `vapord` in a sandbox under the
  repo-local `.vapor/e2e/`, with `HOME` moved into the sandbox. It
  drives the CLI only and never starts the Mac app
  (`docs/development/e2e-verification.md`, `AGENTS.md` §9.8).
- The rules forbid what the owner asks for. An agent never opens a
  packaged app (`AGENTS.md` §7.1, §10.1). No test covers UI
  rendering, and the owner verifies the UI by hand (§9.3, §9.4).
- Two checks already change a host, and both refuse a contributor's
  Mac. `verify-package.sh --install` runs on a GitHub Actions runner
  only. The launchd round trip `R01` needs `--full` and refuses when
  a Vapor LaunchAgent exists (`tools/e2e/src/scenarios/service.rs`).
- Only `apps/macos/scripts/package.sh` builds an app that runs. It
  writes `dist/Vapor.app` and `dist/Vapor-macos-arm64.pkg` from
  release builds. With no signing identity it signs ad hoc.
  `swift run Vapor` gives an app with no CLI: the app searches for it
  at `Contents/Helpers/vapor`
  (`apps/macos/Sources/VaporCore/VaporBundleLayout.swift`).
- The release profile of the Rust workspace uses fat link-time
  optimization, one codegen unit, and no incremental build
  (`Cargo.toml`).
- `./scripts/build.sh package`, measured on the design machine: 79 s
  from an empty build directory, 4 s with no change, 5 s after a
  change to one Swift file of the app, and 56 s after a change to one
  Rust file of the daemon. The installer is 8.5 MB.
- No control of the app has an accessibility identifier
  (`apps/macos/Sources`).
- The app reads the state of the daemon on a 30-second timer
  (`apps/macos/Sources/VaporCore/DaemonHealthMonitor.swift`). A
  change in the daemon shows in the app up to 30 seconds later.
- Open tasks that need a clean Mac: the signed and notarized path
  (M5-1), the LaunchAgent and the login item across an upgrade
  (M5-3), and what a removal leaves behind (M5-5), all in
  `docs/tasks/macos.md`.

### Why the app cannot run in the session of the contributor

- At start the app runs `vapor service bootstrap`, and `autoLaunch`
  is `true` by default (`apps/macos/Sources/Vapor/AppShellViewModel.swift`,
  `core/lifecycle/src/manager.rs`). The command registers two launchd
  jobs with the fixed labels `sh.arn.vapor.daemon` and
  `sh.arn.vapor.supervisor` (`core/shared/src/constants.rs`) in the
  `gui/<uid>` domain. An install boots out a job of the same label
  first (`core/platform/src/service/macos.rs`). So an app started
  against a sandbox replaces the jobs of a real install.
- The app registers a login item with `SMAppService.mainApp`
  (`apps/macos/Sources/Vapor/SMAppServiceLoginItemController.swift`).
  macOS keys it by bundle identifier.
- `HOME` does not move the home directory of a Swift process. A probe
  program built for this design printed the real home with `HOME`
  set, and the new home with `CFFIXED_USER_HOME` set.
- Preferences do not follow `CFFIXED_USER_HOME`. The same probe still
  read a preference of the real home. The write of a preference was
  not tested.
- So four things of a run stay outside a sandbox directory in a login
  session: the launchd jobs, the preferences domain, the keychain
  items, and the menu bar item. Each can get a name of its own and a
  teardown. None can be inside the directory.

### The options

| # | Option | What it gives | Cost or risk |
|---|---|---|---|
| 1 | A dev copy of the app in the login session | A loop of seconds | The four things above. No test of the login item or the installer. Product changes: launchd labels scoped by runtime directory, a second bundle identifier, a check for what a run leaves. |
| 2 | A macOS virtual machine | The real bundle, labels, login item, and installer. The host holds nothing from a test. | A tool to install, an image to download, a boot for each fresh machine. |
| 3 | Screen capture and clicks from outside the app, in the login session | Real pixels of everything | The Screen Recording and Accessibility permissions, which cover the whole session. It takes the pointer and the focus. |

### Tart

Read on 2026-11-21 from the Tart source and README
(`github.com/cirruslabs/tart`, branch `main`) and from its image
templates (`github.com/cirruslabs/macos-image-templates`). Nothing was
installed or run.

- Tart runs macOS and Linux guests on Apple silicon through Apple's
  Virtualization framework. The install command in the README is
  `brew install openai/tools/tart`. The license is FSL-1.1-ALv2:
  internal use is permitted, and a competing product is not. The
  license file and the site carry an OpenAI copyright.
- The newest images are for macOS 27, under the name `golden-gate`.
  The template installs macOS 27.2 and says that it needs Tart 2.33.0
  or newer and macOS 27 on the host.
- `macos-golden-gate-vanilla` has the tags `latest`, `27.2`, and
  `27.0`. It has the user `admin` with the password `admin`,
  auto-login, Remote Login, Screen Sharing, sudo with no password,
  and no screen lock, screen saver, or sleep. Gatekeeper is off. It
  has no guest agent.
- `macos-golden-gate-base` has the tag `latest` only, and it is built
  again each month. It adds the Tart guest agent, Homebrew, tools for
  CI, and Rosetta. Spotlight is off and System Integrity Protection
  is off. Its privacy database grants Accessibility, Screen
  Recording, and the right to post input events to the guest agent,
  to `osascript`, and to the SSH wrapper.
- `tart exec` runs a command in the guest through the guest agent,
  over a control socket of the VM. It needs no network, and it
  returns the exit code of the command.
- `tart clone` uses the copy-on-write of APFS: a clone takes no space
  until the guest writes.
- `tart run --no-graphics` opens no window on the host. `--dir`
  shares a host directory, read-only on request; a macOS guest sees
  it under `/Volumes/My Shared Files`. `--net-host` limits the guest
  to a host-only network. No flag removes the network interface.
- `tart run --vnc-experimental` serves the screen of the VM over VNC.
  Tart has no command for a screenshot, a recording, or input.
- `tart suspend` saves a running VM that was started with
  `--suspendable`, and the next `tart run` resumes it.
- Tart keeps images, clones, and its cache under `~/.tart`. The
  variable `TART_HOME` moves all of it.
- A macOS guest cannot run inside a VM (the Tart FAQ: nested
  virtualization is "only for Linux VMs"). A GitHub-hosted macOS
  runner is a VM, so this tier cannot run there.
- Apple permits two macOS guests at one time on one host (stated in
  the Lume documentation, not in Tart's).

Numbers from other people, not measured here: a clone in 0.03 s, and a
boot to a logged-in desktop in 20 to 30 s. The README states 25 GB
for the download of the macOS 26 base image. No page states the size
of a macOS 27 image.

### What a Tart image does not show

The base image is not a user's Mac in these points: Gatekeeper is
off, System Integrity Protection is off, Spotlight is off, the user is
an administrator with sudo and no password, no Apple account is
signed in, and the GPU is virtual. How the glass and the translucency
of the app look on a virtual GPU was not checked.

### The spike, 2026-11-21

Run on the design machine after D4, with Tart 2.42.0 and the image
`ghcr.io/cirruslabs/macos-golden-gate-base` at the digest
`sha256:4859ea9483a1f6e377301f23a5243798044266a373fc36635e9dcd045e09292f`.
The guest ran macOS 27.2 with 4 CPUs, 8 GB of memory, and the guest
agent 0.15.0. Every number below is one measurement.

The install and the image:

- Homebrew 7 refuses a formula of a tap until the user trusts it.
  `brew trust --formula` for `openai/tools/tart` and for its
  dependency `openai/tools/softnet` was enough.
- The pull was 31.7 GB compressed and took 19 minutes. The image uses
  36 GB on disk.
- A clone by digest works: `tart clone <image>@sha256:<digest> <name>`.

The life of a guest:

| Step | Time |
|---|---|
| Clone | 0.05 s |
| First boot, until `tart exec` answers | 25 s |
| One `tart exec` call | 0.04 s |
| Install of `Vapor-macos-arm64.pkg` | 2.6 s |
| Start of the app, until its process runs | 0.7 s |
| Reboot of the guest, until the app runs again | 31 s |
| `tart stop` | 0.2 s |
| Suspend, until the state is on disk | 7.7 s |
| Resume, until `tart exec` answers | 6.5 s |
| Start of a clone of a suspended guest | 11 s |
| Delete of a clone | 0.3 s |

- Two clones and one suspend state used 3.6 GB on top of the image. A
  suspend state is 3.2 GB.
- The guest ran with `--no-graphics --no-audio --no-clipboard
  --net-host`. `tart exec` and the shared directories worked. The
  guest had an address on a host-only network, no route to the
  Internet, and no name resolution.
- A share with `:ro` refused a write from the guest. A file written
  to the results share appeared on the host.
- `tart exec` runs a command as the user `admin` in the login
  session and returns its exit code. Its `PATH` has no `/sbin`.
- `tart stop` ends the VM at once. Whether the guest shuts down
  cleanly first was not checked.
- A clone of a suspended guest resumes with the processes and the
  windows of the original. Its clock stays behind by the time it was
  suspended, and one `sudo date` command sets it from the host.

To see and to act:

- A probe tool built on the host, read from a read-only share, and
  started with `tart exec` had the Accessibility, Screen Recording,
  and input event permissions. Nothing was granted by hand.
- `screencapture -x` wrote a 1440x900 PNG of 1.9 MB in 1.1 s.
  `screencapture -v -V 10` wrote a 10-second H.264 video of 1.5 MB.
- The first capture in a guest opens a system dialog: the guest
  agent "is requesting to bypass the system private window picker".
  A press on its Allow button through accessibility closed it, and
  macOS keeps the approval for 30 days in the guest.
- The probe read all controls of Vapor in 0.2 s: the menu bar item,
  its menu, the main window, and the checkbox. With no identifiers in
  the app it found a control by its title or its description.
- A pointer event on the menu bar item opened the menu. A pointer
  event on "Open Vapor" opened the main window. A press on "Start
  Vapor at login" changed `autoLaunch` in `vapor.json`.
- After the first start macOS showed three notifications for Vapor:
  one for the login item and one each for `vapor` and `vapord` as
  background items. They stay on the screen, they cover its top right
  corner, and accessibility reads their text.
- The menu and the translucent sidebar of the main window rendered
  on the virtual GPU. The agent looked at the screenshots. The owner
  did not judge them yet.

The product in the guest:

- The installer put `Vapor.app` in `/Applications` and the link at
  `/usr/local/bin/vapor`. The first start of the app registered the
  daemon job, the supervisor job, and the login item, and made
  `~/.vapor` in the guest.
- After a reboot of the guest the app, the daemon, and the supervisor
  ran again, and no window opened.

The host:

- Tart wrote to these places: its store `~/.tart`, a cache and an
  HTTP storage directory under `~/Library` with the name
  `com.github.cirruslabs.tart`, and what Homebrew installs (two
  formulae, the tap, shell completions, a trust file).
- While a guest runs, the host has two more network interfaces
  (`vmenet0`, `bridge100`). They go when the guest stops.
- After the removal a comparison with a listing from before the spike
  showed nothing of Tart on the host.
- The removal has one hazard. `brew uninstall` also removes every
  formula that Homebrew counts as an unused dependency. On the design
  machine it removed 29 formulae that had no relation to Tart. A
  removal must set `HOMEBREW_NO_AUTOREMOVE=1`.

## Decisions

### Round 1, 2026-11-21: where the app runs

| # | Decision |
|---|---|
| D1 | The Mac app is tested in a macOS virtual machine, not in the login session of the contributor. Only a VM keeps everything of a test off the host (owner, 2026-11-21). |
| D2 | The VM tool is Tart. A spike on the design machine confirms it before the build starts. If the spike fails, the choice goes back to the owner (owner, 2026-11-21). |
| D3 | The guest runs the newest macOS that Tart has an image for. On 2026-11-21 that is macOS 27 (owner, 2026-11-21). |

### Round 2, 2026-11-21: the image, the build, and the rules

| # | Decision |
|---|---|
| D4 | The spike runs on the design machine. It installs Tart with Homebrew and pulls the image into Tart's default store. It ends with a full removal and a list of what stays on the host (owner, 2026-11-21). |
| D5 | The image is `macos-golden-gate-base`, pinned by digest. It has the guest agent and the permissions that an agent needs. The cost is CI tools that Vapor does not use, and no version tag (owner, 2026-11-21). |
| D6 | The VM tests the installer that `package.sh` writes, unchanged. The app gets no test hook. The measured build times permit it (owner, 2026-11-21). |
| D7 | An agent acts on the app through the accessibility of macOS inside the guest: it finds a control by its identifier and clicks it with a real pointer event. A driver tool in the repository does this, and it never ships. The owner left this choice to the agent ("whatever is more efficient for you when testing", 2026-11-21). The spike confirms the path. Narrowed by D18. |
| D8 | The first version proves: the install from the installer, the first start, the menu bar item and its menu, the main window, Settings, each sync state, quit, start at login after a reboot of the guest, and the launchd round trip. It does not prove the Google sign-in, Gatekeeper and notarization, or an upgrade from an older release (owner, 2026-11-21). |
| D9 | An agent may install and start the packaged app inside the VM, and never on the host. The agent verifies behaviour and attaches screenshots. The owner judges the look. Tier 1 keeps its ban on UI tests (owner, 2026-11-21). |
| D10 | A dev copy of the app in the login session of the contributor is out of this spec. It comes back only if the VM loop proves too slow (owner, 2026-11-21). |

### Round 3, 2026-11-21: how the tier runs

| # | Decision |
|---|---|
| D11 | Tart and the image of D5 are installed again on the design machine, and they stay there between runs. A removal after each run costs a pull of 19 minutes (owner, 2026-11-21). |
| D12 | The contributor installs Tart by hand. The script of the tier checks for Tart, prints the commands when it is missing, and pulls the image itself. No script of the repository runs Homebrew (owner, 2026-11-21). |
| D13 | A scenario states which guest it needs, and the need follows from what it tests. A scenario that tests the install, the first start, the start at login, or a removal gets a fresh clone. Scenarios that test screens and states of the installed app share one guest. The owner: "having a fresh clone or not depends on the tests" (2026-11-21). |
| D14 | Screenshots and video go under the repo-local `.vapor/vm/`, one directory for each scenario: a PNG at each named step and one video. `./scripts/clean.sh` removes them, and nothing is committed. `.gitignore` already covers `.vapor/` (owner, 2026-11-21). The video part is replaced by D19. |
| D15 | The tier runs on request. An agent runs it before it reports a change under `apps/macos` or to the packaging as done. It is a leg of `./scripts/release.sh`. It is not a gate of a pull request, because a GitHub-hosted runner cannot run it. The manual handoff checklist of `AGENTS.md` §9.8 becomes the screenshots of the run and a list of what the owner judges (owner, 2026-11-21). |
| D16 | The tier is Tier VM: the test tier that runs the product in a virtual machine. Its entry point is `./scripts/vm.sh`, its harness is `tools/vm`, its skill is `vapor-vm`, and its document is `docs/development/vm-verification.md` (owner, 2026-11-21). |
| D17 | The first version tests Vapor on one guest: the newest macOS that Tart has an image for (D3). A second image at the minimum macOS that Vapor supports is out, and Linux guests are out (owner, 2026-11-21). |

### Round 4, 2026-11-21: the build

| # | Decision |
|---|---|
| D18 | D18 narrows D7: an item of the menu of the menu bar item is found by the text of its key in the English catalog, not by an identifier. macOS builds those items from the SwiftUI views and drops the identifier that a view sets. A control in a window is found by its identifier, as D7 says. The agent chose this under the delegation of D7 (2026-11-21). |

### Round 5, 2026-11-21: after the first runs

| # | Decision |
|---|---|
| D19 | D19 replaces the video part of D14: a run records no video by default. The owner saw the videos of the first runs: "some videos that im seeing of the vm its just a plain image" (2026-11-21). A run records a video of each scenario only when it gets `--video`, for a change that moves on the screen. The screenshots at each named step stay. |
| D20 | The description of a pull request that changes something visual carries pictures of the change: the screenshots from a Tier VM run, and a video when the change moves. Both are optional. The owner: "so the person reviewing the PR has more context and can do a better review seeing what changed" (2026-11-21). The GitHub CLI uploads them with its `--attach` option. A picture shows a guest only, because the repository is public. |

### Round 6, 2026-11-21: a window for a person

| # | Decision |
|---|---|
| D21 | A manual guest opens a window on the contributor's Mac when a person asks to use it by hand: `./scripts/vm.sh --guest --window`. Only the manual guest, and only with the option. A run of the suite never opens a window. The clipboard stays unshared and the guest still has no route to the Internet. The owner asked for it: "so that i can test that feature/vapor myself in that vm ... to see how it looks inside the simulator" (2026-11-21). |

## Design

Tier VM runs the installer of the build in a macOS guest and acts on
the app as a user does. Each part names the decision behind it.

### What a contributor installs, one time

- Tart, by hand (D12): `brew tap openai/tools`, `brew trust --formula`
  for `openai/tools/tart` and `openai/tools/softnet`, then
  `brew install openai/tools/tart`. The document of the tier gives
  the commands, and the removal with `HOMEBREW_NO_AUTOREMOVE=1`.
- The image of D5, pulled by `./scripts/vm.sh --setup` into Tart's
  store: 36 GB. The digest is one constant in `tools/vm`. A change of
  the digest is a change set of its own that runs the tier.

### The parts in the repository

- **`./scripts/vm.sh`** (D16). It checks the host: Apple silicon,
  macOS 27 or newer, and `tart` on the `PATH`. When one is missing it
  stops and prints what to do. It builds the installer with
  `./scripts/build.sh package` and the driver, then starts the
  harness. Its options follow `./scripts/e2e.sh`: the suite, `--only`,
  `--list`, `--keep`, `--skip-build`, a manual guest, and `--setup`.
- **`tools/vm`**, the crate `vapor-vm` (D16). A member of the Cargo
  workspace that never ships, like `tools/e2e`, and it uses that
  harness library where it fits. One module is the only code that
  starts `tart`. Every VM that the harness makes has a name that
  starts with `vapor-vm-`, and the harness deletes only such names.
- **The driver** (D7), a Swift command line tool under `tools/vm`.
  The script builds it on the host. It runs in the guest only, from a
  read-only share, and it is never inside `Vapor.app`. It lists the
  controls of an app, finds one by its identifier, presses it or
  sends a pointer event to it, lists the windows, and reads the
  notifications of macOS. Its output is JSON.
- **Accessibility identifiers** in `apps/macos`: one identifier for
  each control and each status text that a scenario uses, defined in
  one file of `VaporCore`. This is the one product change (D6). A
  scenario finds a control by its identifier, never by its title, so
  a change of copy or of language does not break it.

### Guests

- **The prepared guest.** `--setup` makes it once for each digest: a
  clone of the image with a 1440x900 display, booted, with the screen
  capture dialog accepted, then suspended. A scenario never runs in
  it.
- **The start of a guest.** A clone of the prepared guest, started
  with `--no-graphics --no-audio --no-clipboard --net-host
  --suspendable`. The harness waits until `tart exec` answers and
  sets the clock of the guest from the host. The spike measured 11 s
  for this start.
- **Shares.** `dist/` read-only, the directory of the driver
  read-only, and the results directory of the scenario read-write.
  The guest sees nothing else of the host.
- **A fresh guest or the shared guest (D13).** A scenario declares
  one of the two. A fresh guest has no Vapor: the scenario does the
  install itself. The shared guest has the installer installed and
  the app started, and the scenarios that share it run one after the
  other. Before each of them the harness puts Vapor in the guest into
  the start state that the scenario names, so the order does not
  matter (`AGENTS.md` §9.1).
- **Two guests at most.** Apple permits two macOS guests on one host.
  One guest runs the shared scenarios while the other runs the fresh
  scenarios in turn. The harness counts the guests that already run
  and waits for a free place.
- **The end of a guest.** The harness stops and deletes every clone
  of the run, on a pass and on a failure. `--keep` leaves the guest
  of a failed scenario for a look. `./scripts/clean.sh` deletes every
  VM with the `vapor-vm-` prefix, the prepared guest included, and
  never the pulled image.
- **The manual guest.** A shared guest that stays up until the
  contributor stops it, for an agent that explores a change.

### Evidence (D14)

Each scenario writes to `.vapor/vm/<run>/<scenario>/`: a PNG at each
named step, one video of the scenario, the control list at a failure,
and the logs of the daemon from the guest. The report of a run lists
each scenario with its verdict and its files. The harness closes the
notifications of macOS before a screenshot, except in the scenario
that asserts them.

### Scenarios of the first version (D8)

On a fresh guest:

- The install: the app is in `/Applications`, `vapor` is on the
  `PATH` with the version of the build, and nothing of Vapor runs
  before the first start.
- The first start: the app registers the daemon job, the supervisor
  job, and the login item, and the menu bar item appears.
- The start at login: after a reboot of the guest the app, the
  daemon, and the supervisor run, and no window opens.
- Quit: "Quit Vapor" stops the daemon and ends the app.
- The launchd round trip of Tier E2E (`R01`), which no contributor
  machine runs today. The harness of Tier E2E runs it inside the
  guest with `--full`.

On the shared guest, with the filesystem provider:

- The menu bar item and each item of its menu.
- The main window: it opens, a second "Open Vapor" focuses the same
  window, and a close leaves the menu bar item and the daemon.
- Settings: a changed setting reaches `vapor.json`.
- Each sync state as the menu and the main window show it:
  unconfigured, idle, syncing, paused, error, a waiting decision, and
  restart required.

### What the build changed in this design

The build found these points where the design above does not hold as
written. The text above stays as the record of the design.

- **Menu items have no identifier (D18).** The identifiers reach the
  controls of the windows only. A scenario names a menu item by its
  catalog key, and every text of the app that a scenario compares
  comes from the catalog. Three texts of macOS itself are written in
  the scenarios: the Settings item of the app menu, the title of
  Finder's Applications window, and the name of the app in a
  notification.
- **Two prepared guests, not one.** A clone resumes with the network
  address of the guest it came from, and Tart refuses two VMs with one
  address. So each of the two places for guests has a prepared guest
  with an address of its own.
- **A resumed guest reads the shares again.** A guest that resumes
  from a saved state can miss a file that the host replaced after the
  save. The harness mounts the shared directories again after each
  start.
- **The stage directory, not `dist/`.** A run copies the installer,
  the driver, and the e2e binaries into `.vapor/vm/stage/`, and the
  guests share that directory read-only.
- **A checkout deletes its own guests only.** Tart's store is shared
  by every checkout on a machine, so the name of a guest carries a tag
  made from the path of its checkout. `./scripts/clean.sh` deletes the
  guests with that tag. `./scripts/vm.sh --clean` also deletes the
  prepared guests. A lock file in the user's temporary directory
  keeps two runs from one place for a guest.
- **A kept guest keeps running.** `--keep` leaves the guest of a
  failure up as the tier started it, and `--guest-stop` deletes it. A
  plain `tart run` of a stopped guest opens a window and gives the
  guest the Internet.
- **A window on request (D21).** The rule "a guest has no window"
  holds for every run. A manual guest shows Tart's window when a
  person asked to use it.
- **A video for each boot.** A restart of the guest ends the
  recording, so the scenario of the start at login has two files
  when a run records (D19).

The scenarios also differ in two points. Quit runs on the shared guest:
it needs no fresh Mac. One scenario covers both the error state and a
waiting decision, because an unconfirmed one-way mode gives both.

### How the constraints hold

- **Nothing of a test on the host (D1).** A run writes to
  `.vapor/vm/` and to Tart's store, and it deletes its clones. The
  harness never starts the app or the installer on the host: both run
  only through `tart exec`.
- **No network in a test (`AGENTS.md` §9.1).** The guest has no route
  to the Internet. A run never pulls: with no image it stops and
  names `--setup`. One limit stays: on the host-only network the
  guest can reach a service that the host itself offers there, and no
  flag of Tart removes the interface.
- **Tier E2E stays as it is**: the CLI and the daemon in a sandbox on
  the host, on every pull request.
- **Tier 1 keeps its ban on UI tests (D9).** Tier VM proves
  behaviour. The owner judges the look from the screenshots.

### Rules and documents that the build changes

- `AGENTS.md`: §7.1 and §10.1 (a packaged app starts inside a guest
  of Tier VM and never on the host), §9.3 and §9.4 (what Tier VM
  covers of the app), §9.5 (the new tier), §9.8 (the handoff of D15),
  a new section with the contract of Tier VM, and §13.
- The skill `vapor-vm` with its mirror link, and the tier in
  `vapor-validate`.
- `docs/development/vm-verification.md`, the testing strategy, the
  toolchain baseline, the runbook, `tools/README.md`, and the tasks
  of the Mac app that needed a clean Mac.
- `./scripts/release.sh` gets the leg of D15, and
  `./scripts/clean.sh` the deletion of the guests.

## Verification

### The spike

The spike of D2 and D4 ran on 2026-11-21 and every step passed. The
results are under "The spike" in the facts.

1. Install Tart and pull the macOS 27 base image. Record the size on
   disk and the time.
2. Measure a clone, a boot until `tart exec` answers, a stop, and a
   delete.
3. Run the guest with no window and with the network limited, and
   confirm that `tart exec` and the shared directories still work.
4. Install `Vapor-macos-arm64.pkg` in the guest and start the app.
5. Take a screenshot and a video of the guest from a command, and
   look at how the app renders.
6. Read the controls of the app and press one, from a command.
7. Reboot the guest and check that Vapor starts at login.
8. Measure a suspend and a resume.
9. Remove everything and list what stays on the host.

Step 9 left nothing of Tart on the host. It also removed 29 Homebrew
formulae that had no relation to Tart, through the hazard named in
the facts. 28 were installed again. One, a formula that Homebrew
disabled, was not.

### The build

- `./scripts/vm.sh` passes on the design machine with every scenario
  of the first version.
- Each scenario is seen to fail once, against a build in which the
  behaviour that it proves is broken by hand. The broken build is not
  committed.
- A run ends with no VM of its own, and the harness asserts it. A
  listing of the host from before and after one run shows a change
  only under `.vapor/vm/`.
- `format`, `lint`, and `test` pass with the new crate and the
  driver, and Tier E2E passes unchanged.
- The owner looks at the screenshots of one run.

### What was verified, 2026-11-21

On the design machine, with the build of this change:

- `./scripts/vm.sh` passed with all 14 scenarios. The last run took
  422 s, of which about 80 s made the two prepared guests. The four
  scenarios of the install ran in fresh guests while the ten scenarios
  of the installed app ran in the shared guest.
- 13 of the 14 scenarios failed against builds that were broken by
  hand, each for its own break: an installer that makes no `vapor`
  link, a login item that is not registered, a menu item that is
  removed, a wrong version in Diagnostics, a setting that is not
  saved, a Quit that leaves the daemon, and a status that always says
  Idle with no detail and no notice. The broken builds were not
  committed. The fourteenth scenario runs `R01` of Tier E2E, which
  that tier proves. The review of the change then made six
  assertions stricter; those were run against the real build only.
- A listing of the host from before and after the run showed no
  difference, and Tart's store held no clone of the run.
- `./scripts/format.sh`, `./scripts/lint.sh`, and `./scripts/test.sh`
  passed with the new crate and the driver. Tier 1 ran in 19 s.
- A manual guest held the place of the shared guest, a run of the
  suite waited for it and said why, and the run went on when
  `./scripts/vm.sh --guest-stop` deleted the guest.
- The launchd round trip `R01` passed inside a guest. It is the first
  run of that scenario on a contributor's machine.

The owner did not look at the screenshots of a run yet.

What the run of the tier found in the product, reported to the owner
and not changed here. Each has a task in `docs/tasks/macos.md`:

- macOS writes a new login item to disk a few seconds after the app
  registers it. A restart of the Mac before that loses the item:
  after the restart the daemon runs and the app does not start. The
  app registers the item again at its next start.
- Before its first read of the daemon, 30 seconds after its start,
  the app shows "Idle" with the provider "Filesystem", whatever the
  daemon does.

One limit of the tier itself: the pointer events of the driver count
as a user at the device, so the daemon in a guest works at the pace of
"Throttled" while a scenario acts.

## Open points

None.
