# End-to-end verification (Tier E2E)

Authoritative reference for Vapor's end-to-end verification tier. The
policy summary lives in `AGENTS.md §9.8`; this document is the full
process: what Tier E2E is, when it is required, how to run it, how to
extend it, and what it deliberately does not cover.

## Why this tier exists

Vapor is coded autonomously. Tier 1 (`./scripts/test.sh`) proves that
modules and composed behaviors are correct in-process, but a coding
agent that only ever runs unit and integration tests has never watched
the product work: real binaries, real process boundaries, real
FSEvents, a real durable DB on disk, real IPC over a real socket, real
signals. Tier E2E closes that gap. It is the closest an autonomous
agent gets to "I ran the app and it worked" without touching the
machine it runs on.

## What Tier E2E is

`./scripts/e2e.sh` builds the harness (`tools/e2e`, crate `vapor-e2e`)
and the two shipping Rust binaries (`vapor`, `vapord`), then drives the
daemon black-box through the CLI only, the way a headless user would,
against disposable sandboxes. Every scenario gets its own sandbox:

```
<repo>/.vapor/e2e/run-<id>/<scenario id>/
├── home/          ← VAPOR_DIR: vapor.json, logs/, state/, vapord.sock, vapord.lock
├── local/         ← the watched local sync root (created by the daemon itself)
├── cloud/Vapor/   ← the cloud root the filesystem provider treats as the cloud side
└── primary-daemon.out   ← the daemon's stdout/stderr
```

A scenario that needs more than one daemon provisions extra homes
(`<label>-home/`, `<label>-local/`, `cloud/Vapor-<label>/`) in the same
sandbox. Nothing is shared between scenarios.

Observation channels are the product's own surfaces, never test hooks:
`vapor status --json` and the other `--json` commands (parsed with the
same types the CLI serializes), exit codes, the daemon log, and
read-only queries against the durable state DB. The harness runs on
the host it is built on: macOS and Linux today (the launchd round-trip
and the disk-image scenarios are macOS-only and skip elsewhere by
name), Windows once its native platform traits ship (until then its
daemon scenarios skip, by name). A Linux run over a macOS checkout
works from a container with `CARGO_TARGET_DIR` pointing outside the
host's `target/` (so the two toolchains never overwrite each other's
binaries) and a tmpfs mounted over `.vapor/`: a Docker Desktop bind
mount refuses to bind a Unix socket, so a daemon started on one never
answers `vapor status`.

## The three shared checks

Every scenario ends with the same epilogue, so a scenario that passes
its own assertions can still fail on the invariants the product must
hold everywhere:

1. **Clean shutdown.** Every daemon the scenario started is stopped
   with SIGTERM and must exit cleanly within the grace period.
2. **Tree oracle.** The local root and the cloud root of every home
   must hold the same files: same paths, sizes, content hashes, and
   (on Unix) executable bits, after removing ignored names and the
   provider's internal files. Directories are not compared on their
   own, since Vapor materializes parents when it applies children.
   A scenario whose trees cannot match by construction opts out with
   a reason that shows up in the report.
3. **Log hygiene.** No `[ERROR]` line in any daemon log, and no
   warning the scenario did not declare it expects (a keep-both
   resolution, a restart-required key). A healthy run has a warning
   budget of zero. Any `failed_intents` row fails the scenario unless
   it said otherwise.

A queue that is empty is not a converged queue. Scenarios wait for the
intents their changes produce (`mark` then `converge_from`) or for a
quiet window with nothing enqueued (`settle`), then assert on files.

## Safety contract (hard rules)

The default run must be safe to run unattended on a contributor
machine or CI:

- Everything lives under the repo-local `.vapor/e2e/` sandbox.
  `./scripts/clean.sh` removes all residue and stops any manual
  sandbox daemon first. It signals a process only when that process is
  a `vapor` or `vapord` that runs from this repository's build: a pid
  file can outlive its daemon, and the number can go to the installed
  daemon. Never touch `~/.vapor`.
- Never install host services: no `vapor service install`, no
  LaunchAgent or launchd mutation, no login items. (`vapor doctor`
  reads host state; that is fine.)
- Never launch the macOS app (`AGENTS.md §7.1`: agents do not open
  packaged apps). Runtime behavior is verified through the CLI.
- No network. The filesystem provider is the default; the Google Drive
  mode is opt-in and needs credentials (below).
- The user's home is not the real one. Every process a scenario starts
  gets `HOME` (and `USERPROFILE`) set to `<sandbox>/user`, and the
  `XDG_*_HOME` variables removed, so a `~` in a path, a default folder,
  a marker file, the user's trash, or a service unit lands inside the
  sandbox. The check of the binaries before a run is pinned the same
  way. Only a Google Drive run and the `--full` launchd scenario keep
  the real home, because the browser sign-in and `launchd` need it.
- A failed scenario preserves its sandbox and prints diagnostics for
  every daemon it started; a green run deletes its sandboxes (keep them
  with `--keep`). Any non-zero exit preserves.

The one sanctioned exception is `--full`, which adds the service
round-trip (`R01`): it installs a real LaunchAgent, so it is meant for
disposable CI runners (`test.yml` passes it on macOS), refuses when a
`sh.arn.vapor.daemon` LaunchAgent already exists, and removes the
LaunchAgent on every exit path.

## When an agent must run it

After Tier 1 passes, run `./scripts/e2e.sh` before committing when the
change plausibly alters end-to-end runtime behavior:

- new features in `core/daemon`, `core/cli`, `core/ipc`, `core/shared`
  (config, paths, logging), `core/lifecycle`, `core/providers`, or
  `core/platform`;
- bug fixes whose failure mode a user would see through the daemon or
  CLI (sync stalls, wrong state reporting, startup or shutdown
  problems, config not applying);
- changes to startup order, signal handling, IPC contracts, durable
  schema, or the build of the shipping binaries.

Doc-only, UI-only (Swift view or menubar), or test-only changes do not
need an E2E run. When in doubt, run it.

Two obligations when Tier E2E applies:

1. **Feature coverage.** If the change adds e2e-observable behavior (a
   new CLI command, a new daemon state, a new convergence path), add a
   scenario for it in the same change set, and run it once against the
   base commit: it must fail before the feature and pass after. Running
   only the pre-existing scenarios proves that nothing broke, not that
   the feature works.
2. **The app.** If the change also affects the Mac app or its
   install, run Tier VM (`vm-verification.md`) and give the project
   owner its screenshots with a list of what to judge.

## Running it

```
./scripts/e2e.sh                       # build + every scenario the host can run
./scripts/e2e.sh --skip-build          # reuse target/debug binaries
./scripts/e2e.sh --only S16,S23        # a subset, by id or name
./scripts/e2e.sh --keep                # preserve the sandboxes after a green run
./scripts/e2e.sh --json out.json       # write the report here (default: in the run root)
./scripts/e2e.sh --daemon vapord       # start the shipped vapord binary instead of `vapor run`
./scripts/e2e.sh --provider gdrive     # Google Drive mode (needs credentials; see below)
./scripts/e2e.sh --list                # scenario catalog with needs and known gaps
./scripts/e2e.sh --full                # add the launchd round-trip (disposable runners only)
./scripts/e2e.sh --sandbox             # manual sandbox: provision + leave a daemon running
./scripts/e2e.sh --sandbox-stop        # stop and remove every manual sandbox
./scripts/e2e.sh --jobs 1              # one scenario at a time (default: one per core)
```

Scenarios run several at a time. Each has its own sandbox, runtime
directory, and socket, so they never share state; what they share is
the host's CPU and disk, and the harness treats each sandbox as a
smaller computer for it: the daemon under test gets two concurrent
transfers per direction, and every wait deadline grows by half per
extra scenario running (a bound, so a green run costs nothing extra;
a loaded host does not read as a failure). Scenarios that mount disk
images take a lock among themselves. The queue starts with the
launchd round-trip when `--full` asks for it (the longest scenario by
far: real crash-loop backoff and the supervisor's tick, in a sandbox
of its own), then the scenarios the previous run found longest, read
from the last report, so the tail is the slowest scenario and not the
sum.
The default suite takes about as long as its slowest scenario, a
little over a minute on a twelve-core laptop and under two in a
container, against nine and a half in sequence; the summary line
reports wall time. A scenario is mostly a daemon waiting on its own
timers, so one per core leaves the host far from saturated.

Output is one line per scenario:

```
[e2e] PASS S03: local writes become durable intents and drain; the cloud root matches byte for byte (1.9s)
[e2e] SKIP R01: needs full: run with --full to include it
[e2e] KNOWN-GAP S99: the assertion the scenario cannot meet yet, quoted (5.0s)
[e2e] FAIL S16: timed out after 30s waiting for: .../cloud/Vapor/gone.txt to be removed (30.1s)
[e2e] OK: 49 passed, 0 failed, 2 skipped, 0 known gaps, 0 unexpected passes (93.6s)
[e2e] report: .../.vapor/e2e/run-711543-14708/e2e-result.json
```

On a failure the harness prints, for every home in the scenario,
`vapor status --json` and `vapor diagnostics --json` (if the daemon is
still up), the queue and failed-intent rows, the last 60 daemon log
lines that say something (the polling chatter left out), and the
daemon's stdout/stderr tail, then keeps the sandbox. Use the
`vapor-debug` skill on the preserved directory; on CI the preserved
sandboxes' logs ride along in the `e2e-report-<os>` artifact.

The JSON report (`e2e-result.json`, schema version 1) carries the host
facts, every scenario's verdict, reasons, seconds, notes, and preserved
sandbox path, and a summary. The run's exit code is 0 only when no
scenario failed and no known-gap scenario unexpectedly passed.

### Verdicts

| Verdict | Meaning |
|---|---|
| `PASS` | the scenario's assertions and the shared epilogue held |
| `FAIL` | something did not; sandbox preserved, diagnostics printed |
| `SKIP` | the host cannot meet a need the scenario declares; the reason names the need |
| `KNOWN-GAP` | the scenario asserts behavior the product should have and is marked as a known gap; it failed as expected. The marker names the gap in words |
| `FIXED?` | a known-gap scenario passed: remove its marker in the same change that fixed the product. Counts as red |

### Needs

A scenario declares what it needs; the runner skips it, by name, when
the host cannot provide it: `native-watcher` (the daemon can start on
this OS), `unix`, `fifo`, `posix-mode`, `xattr`, `launchd` (with
`--full`, no existing Vapor LaunchAgent, a usable `gui/<uid>` domain),
`full`, `filesystem-provider`, `gdrive-provider`,
`case-insensitive-fs`, `case-sensitive-fs`, `disk-image` (macOS
`hdiutil`, used to mount a case-sensitive or tiny volume),
`non-utf8-names` (the sandbox filesystem accepts a file name that is
not UTF-8: ext4 and tmpfs do, APFS refuses), `secret-store` (the
daemon can read tokens: always on macOS, on Linux only with
`VAPOR_SECRETS_COMMAND` or a Secret Service session).

## Manual sandbox

The scripted scenarios prove non-regression; they cannot explore. When
developing a feature or chasing a bug, run the product yourself, scoped
to the same disposable layout:

```
./scripts/e2e.sh --sandbox
```

This builds, provisions `.vapor/e2e/sbx-<id>/`, configures the scope,
starts a daemon, writes its PID to `daemon.pid`, and prints a
cheat-sheet: the `VAPOR_DIR` export, the watched local root, the cloud
root, the log and state-DB paths, the CLI commands to poke at it, and
the `verify-trees` command that runs the tree oracle by hand. The
daemon keeps running after the script exits; stop it and remove the
sandbox with `./scripts/e2e.sh --sandbox-stop` (`./scripts/clean.sh`
does the same for every sandbox).

The harness sets `VAPOR_THROTTLE_INPUTS=static` for every daemon it
starts, scripted or manual, so the throttle sits on neutral inputs
instead of tracking your keyboard: with host inputs a daemon on a
machine someone is typing on stays `Throttled`, and reconcile only
runs in `Idle`. To watch the real throttle react, start a daemon
with that variable unset.

Never run manual experiments against `~/.vapor` or with `VAPOR_DIR`
unset; that is the project owner's real runtime dir.

Deep paths: macOS caps Unix-socket paths at about 104 bytes. When
`<vapor_dir>/vapord.sock` exceeds the budget, daemon and CLI rendezvous
at a short per-`vapor_dir` socket under the OS temp dir instead (S09
covers this; `vapor doctor` explains it when active).

## Provider modes

`--provider filesystem` (default) needs nothing: the cloud root is a
directory and scenarios write to it directly to play "another device".
The harness builds `vapor` and `vapord` without a Google Drive OAuth
client and refuses a filesystem run over binaries that carry one
(`vapor doctor` reports it). A sandbox cannot reach the developer's
sign-in: secrets are scoped to the runtime directory that stored them
(`docs/operations/provider-auth-operations.md`), and each sandbox has
its own. The missing client is the second guard. S21 and S55 use
`gdrive` profiles with no client or a placeholder one, so neither
reaches the network.

`--provider gdrive` runs the same catalog against a real Google Drive
with real credentials. It is a release-gate leg, run by hand on the
maintainer's machine by `./scripts/release.sh` before a version
bump, with a dedicated test account and `VAPOR_GDRIVE_CLIENT_ID` set;
it never runs in CI, never on an ordinary change, and never against a
personal account. Because of the secret scope, the leg has to put the
test account's token into each sandbox (`vapor auth login --token -`
against the sandbox's runtime directory); that step is part of the
open work below. It is wired in
the harness and every scenario that manipulates the cloud root
directly declares `filesystem-provider` and skips under it; the
Drive-side operations (a `CloudSide` the harness performs through the
provider crate, a per-run `VaporE2E-<run-id>` folder created and
deleted by the harness, longer wait budgets, and the `gdrive-provider`
scenarios for token refresh and rate limits) are open work tracked in
`docs/tasks/core.md`, waiting on the test account. No scenario
contacts the network in the default mode. `./scripts/e2e.sh
--providers` lists the providers a run can target.

## Scenario catalog

`./scripts/e2e.sh --list` is the source of truth; this table mirrors
it. A scenario marked "known gap" asserts the intended behavior and is
expected to fail until the named work lands (`docs/tasks/core.md`).

| Id | Proves |
|---|---|
| S01 | config set/get round-trips through `vapor.json`; structured keys are stored as JSON |
| S02 | daemon reaches Running, creates the missing local sync root, binds the IPC socket |
| S03 | local writes become durable intents and drain; the cloud root matches byte for byte |
| S04 | pause flips the run state over IPC, resume restores it, the paused backlog drains |
| S05 | a second daemon on the same `VAPOR_DIR` exits non-zero saying one is already running |
| S06 | `vapor doctor` reports no failures inside the sandbox, in text and `--json` |
| S07 | clean SIGTERM shutdown, restart on the same state DB, post-restart writes converge |
| S08 | a healthy run across two restarts emits no ERROR line and no warning |
| S09 | an over-budget `VAPOR_DIR` relocates the IPC socket; status and doctor still work |
| S10 | uploads land byte for byte, a cloud-born file downloads through reconcile, POSIX modes survive |
| S11 | a path that diverged on both sides while the daemon was down keeps both payloads, and the `conflict` timeline entry names the file |
| S12 | pull-only materializes cloud content locally and removes a local-only file without uploading |
| S13 | diagnostics answers over IPC; `vapor report` exports config, logs, live captures, manifest |
| S14 | ignored names never sync in either direction and never manufacture a conflict copy |
| S15 | three versions of one file are one `conflict` decision naming each device; keeping the third device's version applies at once and trashes the others; the resolution syncs; the question closes |
| S16 | a plain local rm removes the cloud copy |
| S17 | a FIFO in the watched root never becomes a remote object and never wedges the queue |
| S18 | a downstream reader that closes the pipe early does not make the CLI panic |
| S19 | a cloud-side delete of an uploaded file propagates through the live changes feed, no reconcile |
| S20 | an offline edit that keeps the byte count is found by the startup reconcile and uploaded |
| S21 | a daemon whose only profile cannot be composed stays up and names the reason in status |
| S22 | a resource ceiling reaches the running daemon live; a restart-required key is reported |
| S23 | SIGKILL mid-upload, restart: the upload completes, trees match, nothing duplicated |
| S24 | SIGKILL mid-download, restart: the download completes and the local copy matches |
| S25 | file rename, directory rename with children, and a move across subtrees converge to the same shape in the cloud |
| S26 | `rm -rf` of a tree removes it from the cloud; the name coming back as a file converges on both sides |
| S27 | a file deleted locally while the daemon was down is deleted in the cloud on restart when the cloud copy is unchanged, and restored when the cloud copy changed meanwhile |
| S28 | a file deleted in the cloud while the daemon was down is removed here into the trash on restart when the local copy is unchanged, and re-uploaded when the local copy changed meanwhile |
| S29 | push-only uploads, overwrites a divergent cloud edit, removes a cloud-only file, never downloads |
| S30 | two profiles in one daemon sync their own roots with their own durable state and never cross |
| S31 | a burst of local deletions is held whole behind a `mass-delete` decision while other work continues; `vapor decisions resolve --choose apply` releases it |
| S32 | with the state DB deleted, a restart rebuilds the index from both trees without loss or invented conflicts |
| S33 | two cloud files differing only by case both materialize locally, the second as a conflict copy aliased to its own cloud object; an edit to the copy reaches that object and the cloud gains no third file |
| S34 | the cloud root disappears mid-run: sync blocks with Error; it returns: work resumes and converges |
| S35 | the shipped `vapord` binary starts, syncs, and shuts down cleanly like `vapor run` |
| S36 | nested directories flow up and down; an emptied directory's files are removed |
| S37 | push-only overwrites a same-size cloud edit with no index row for the pair |
| S38 | a cloud name that would alias a differently-cased local file never rewrites either cloud object, materializes once as a conflict copy, and a second reconcile adds nothing |
| S39 | a write reported moments before SIGTERM becomes a durable intent at shutdown and uploads right after the restart |
| S40 | a burst of cloud deletions is held before it touches this device; `--choose discard` restores the cloud copies from the local ones |
| S41 | a cloud edit made while the daemon was down that keeps the byte count is found by the startup reconcile and downloaded, with no conflict copy |
| S42 | a file removed on this device because the cloud deleted it lands in the trash; `vapor trash list` shows it and `vapor trash restore` brings it back and re-uploads it |
| S43 | an empty folder appearing where the adopted local root was opens a `root-replaced` decision and syncs nothing; `reattach` merges the cloud into it with no deletion anywhere |
| S44 | a daemon started while the adopted local root is missing parks the profile with a `root-missing` decision, never re-creates the folder, and resumes on its own when the volume returns |
| S45 | a deleted cloud root is never re-created on Vapor's own; the `root-missing` decision answered `recreate` re-creates it and re-uploads this device's files |
| S46 | a name that is a file here and a folder in the cloud opens a `type-mismatch` decision and touches nothing; `keep-both` moves the file to a conflict name and brings the folder down |
| S47 | renaming a synced file locally moves the cloud object in place (same inode, no re-upload) and re-keys the index |
| S48 | renaming a synced file in the cloud renames the local file in place (same inode, no download) and leaves nothing in the trash |
| S49 | a sync root that is a whole volume of its own gets its trash at `<volume>/.vapor/trash/`: a cloud deletion is a rename on that volume (same inode, no copy onto the runtime directory's volume), `vapor trash list` shows it, restore puts it back, and the `.vapor` directory inside the root never syncs (needs a disk image) |
| S50 | a local file whose name is not valid UTF-8 opens an `unsyncable-name` decision, never reaches the cloud, and the question is withdrawn once the file is renamed; `skip` stops the asking (needs a filesystem that accepts such a name: Linux) |
| S51 | with a user at the keyboard, on battery, and Low Power Mode on, the startup scan runs with no request and an offline divergence resolves as a keep-both copy; under critical thermal pressure a new file waits and status says why; it uploads when the pressure clears |
| S52 | push-only removes a cloud-only folder and everything under it, one entry at a time, and never leaves the delete retrying |
| S53 | pull-only restores a synced folder removed on this device without leaving the folder's own event as a download that never completes, and removes a local-only folder file by file |
| S54 | an append made in place on the cloud object, which keeps Vapor's op-id tag, reaches this device as the canonical file and is neither dropped as an echo nor turned into a conflict copy |
| S55 | a `gdrive` profile with no stored sign-in holds its queue instead of failing it, and `vapor status` names the profile and the command that ends the hold (needs a secret store) |
| S56 | renaming a synced file on this device so that only the case of its name changes moves the cloud object to the new spelling (same inode), with no conflict copy, no deletion, and nothing in the trash (needs a volume that folds case) |
| S57 | renaming a synced file in the cloud so that only the case of its name changes renames the local file to the new spelling (same inode), with no conflict copy, no deletion, and nothing in the trash (needs a volume that folds case) |
| S58 | a synced file moved out of the folder and moved back a few seconds later stays in the cloud |
| S59 | an upload that waits for bandwidth leaves the daemon near idle: well under half a core while the transfer is held |
| S60 | after a keep-both conflict whose download outlasts the debounce window, the version that won stays in the cloud and both versions end on both sides |
| S61 | a home with no sync scope starts a daemon that syncs nothing and creates no folder, and status says unconfigured and names the command; setting the two roots, one config set each, starts the profile with no restart |
| S62 | setting pull-only prints what the mode does; the daemon then syncs nothing, in either direction, and status says the mode needs a confirmation; after the decision is answered enable the mirror runs |
| S63 | vapor profiles add sets a fresh install up with one command and no restart, and refuses when no terminal can ask and a folder is left out; it refuses a cloud folder inside the local folder, and a local folder inside the one of another profile; remove refuses while the daemon runs, then forgets the sync state and leaves the files on both sides |
| S64 | a folder rule hides a folder and not a file of that name; a negation under an excluded folder stays excluded for the watcher as it does for the walk; a file that a rule change frees syncs with no restart and no reconcile request |
| S65 | vapor report replaces every folder path and file name by a code in the config, the logs, and the live state, and says so; --keep-paths keeps them and says that the report is not for a public issue |
| S66 | vapor status counts the paths that the ignore rules keep out of sync, and vapor ignored lists each one with its rule, a folder one time and never a file that only shares a folder rule's name |
| S67 | in push-only, a file whose name the cloud respells so that only the case changes gets this device's spelling back in the cloud, with no restart and no reconcile request (needs a volume that folds case) |
| S68 | a file that left both folders while the daemon was down, and that is put back on this device with the same bytes, is uploaded and never moved to the trash |
| S69 | a folder that moves into the sync root with a .gitignore of its own syncs by those rules: the files the rules hide never reach the cloud |
| S70 | vapor init takes a fresh install to a running sync with one command and no terminal, refuses when a folder is left out, and a second run changes nothing and says initialized |
| S71 | `vapor pause --profile` stops one profile while the other keeps syncing, `resume --profile` lets its file through, `reconcile --profile` is accepted, and a name with no profile behind it is refused with the names there are |
| S72 | an upload that finds the cloud folder's volume full is not failed: the profile reports that uploads wait; once there is room, `vapor reconcile` tries the waiting upload at once, with no wait for the next automatic try (needs a disk image) |
| S73 | a synced folder that is renamed in the cloud while Vapor does not run: the next start moves its files on this device and removes the old folder, which the move left empty |
| S74 | a profile in `vapor.json` whose local folder is inside the one of an earlier profile does not run: `vapor status` lists it as suspended with the reason, neither of its folders is created, and the earlier profile syncs |
| S75 | an empty folder where the adopted cloud root was never becomes deletions on this device, also when a sync is asked for before the next check of the root: no local file goes to the trash, a `root-replaced` decision opens, and `reattach` sends the local files to the new folder |
| R01 | install registers the daemon and its supervisor with real launchd; then, with the supervisor set aside, start → status → crash-loop supervision through backoff and pause → acknowledge → stop → uninstall; then the supervisor of a new install restarting a killed daemon on its own (`--full`, disposable runners only) |

## Extending the harness

Scenarios are Rust functions in `tools/e2e/src/scenarios/<group>.rs`,
registered in that file's `scenarios()` list with a stable id, a
kebab-case name, a one-line `proves`, the `needs`, and an `expect`
(`Pass`, or `KnownGap("what is missing, in words")`). The context
(`Ctx`) provides homes, daemon control (start, stop, kill, signals),
the CLI bound to a home, read-only DB access, marks and converge or
settle waits, and the epilogue knobs (`allow_warning`, `allow_error`,
`tolerate_failed_intents`, `skip_oracle`, `oracle_ignore`).

Discipline rules:

- **Observe through product surfaces.** CLI exit codes and `--json`
  output, the daemon log, the trees, and read-only state-DB queries.
  Never add a test-only hook to the daemon for the harness's benefit.
- **Bounded waits, never bare sleeps.** Every wait has a deadline on
  the clock and a named condition. To prove something stays true, use
  `hold_for`, which polls the condition for the window.
- **Wait for the intents, then for the drain.** Take a `mark` before
  the change and `converge_from(mark, n)` after it, or `settle` when
  the number of intents is not predictable (renames, directories,
  reconciles). A queue that is empty because the debounce window has
  not elapsed proves nothing.
- **One scenario, one behavior.** If the name needs "and", split it.
- **Own the setup.** Every scenario provisions its own files; nothing
  chains on another scenario's leftovers.
- **Name the gap.** A scenario that documents behavior the product
  does not have yet is a `KnownGap` with a plain-words reason and a
  task in `docs/tasks/core.md`, never a skipped or weakened assertion.
- **Agent-friendly failures.** The failure message says what was
  expected; the epilogue and diagnostics do the rest.
- **Budget.** The default suite runs in about the time of its slowest
  scenario, a little over a minute;
  each known-gap scenario costs its timeout until the product catches
  up, and a scenario that waits out a product timer sets the floor for
  the whole run, so keep such waits to what the proof needs. Long or
  load-shaped runs belong to the soak tier, not here.

## What Tier E2E deliberately does not cover (today)

- **Load and duration.** Hours of churn, fault injection at random
  points, and the model-checked no-loss oracle belong to the soak
  driver (`tools/soak`, `docs/development/soak-testing.md`).
- **Live Google Drive.** The mode exists as a flag; the Drive-side
  operations and the credentials wait on the project owner (above).
- **macOS app UI.** Tier E2E never starts the app. Tier VM
  (`vm-verification.md`) tests the installed app in a virtual machine,
  and the project owner judges the look.
- **The Windows daemon.** Its scenarios skip until the native
  platform traits ship; the harness itself builds and runs `S01` there
  on every PR. Linux runs the daemon scenarios; what it skips is the
  launchd round-trip and the disk-image scenarios; a systemd
  round-trip of its own is still to be written.

## Relationship to the other tiers

| Tier | Entry point | Gate | Proves |
|---|---|---|---|
| Tier 1 | `./scripts/test.sh` | every PR | module + composed correctness, in-process |
| Tier E2E | `./scripts/e2e.sh` | every PR (all three OS jobs; `--full` on macOS) + locally for runtime-affecting changes | the shipped binaries work black-box, end to end |
| Tier 2 | `./scripts/perf.sh` | release pipeline | one soak cell with the SLO checks asserted on its report |
| Tier S | `./scripts/soak.sh` | release pipeline + on demand | five cells of churn with faults; nothing lost, both trees converged (`soak-testing.md`) |

Tier E2E complements Tier 1; it never replaces the Tier 1 tests that
`AGENTS.md §9.2` requires.
