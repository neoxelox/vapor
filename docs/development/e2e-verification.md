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
the host it is built on: macOS today, Linux and Windows once their
native platform traits ship (until then their daemon scenarios skip,
by name).

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
  sandbox daemon first. Never touch `~/.vapor`.
- Never install host services: no `vapor service install`, no
  LaunchAgent or launchd mutation, no login items. (`vapor doctor`
  reads host state; that is fine.)
- Never launch the macOS app (`AGENTS.md §7.1`: agents do not open
  packaged apps). Runtime behavior is verified through the CLI.
- No network. The filesystem provider is the default; the Google Drive
  mode is opt-in and needs credentials (below).
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
2. **Owner handoff.** If the change also affects a UI surface, finish
   the report with a short manual-verification checklist for the
   project owner, since agents never verify UI.

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
```

Output is one line per scenario:

```
[e2e] PASS S03 — local writes become durable intents and drain; the cloud root matches byte for byte (1.9s)
[e2e] SKIP R01 — needs full: run with --full to include it
[e2e] KNOWN-GAP S33 — both colliding payloads must exist locally (one as a conflict copy); local has: ["Readme.md"] (5.0s)
[e2e] FAIL S16 — timed out after 30s waiting for: .../cloud/Vapor/gone.txt to be removed (30.1s)
[e2e] OK — 38 passed, 0 failed, 1 skipped, 1 known gaps, 0 unexpected passes (245.0s)
[e2e] report: .../.vapor/e2e/run-711543-14708/e2e-result.json
```

On a failure the harness prints, for every home in the scenario,
`vapor status --json` and `vapor diagnostics --json` (if the daemon is
still up), the queue and failed-intent rows, the last 40 daemon log
lines, and the daemon's stdout/stderr tail, then keeps the sandbox.
Use the `vapor-debug` skill on the preserved directory.

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
`hdiutil`, used to mount a case-sensitive or tiny volume).

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
runs in `IdleDrain`. To watch the real throttle react, start a daemon
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

`--provider gdrive` runs the same catalog against a real Google Drive
with real credentials. It is wired in the harness and every scenario
that manipulates the cloud root directly declares `filesystem-provider`
and skips under it; the Drive-side operations (a `CloudSide` the
harness performs through the provider crate, a per-run
`VaporE2E-<run-id>` folder created and deleted by the harness, longer
wait budgets, and the `gdrive-provider` scenarios for token refresh and
rate limits) are open work tracked in `docs/tasks/core.md`, waiting on
a dedicated test account from the project owner. No scenario contacts
the network in the default mode.

## Scenario catalog

`./scripts/e2e.sh --list` is the source of truth; this table mirrors
it. A scenario marked "known gap" asserts the intended behavior and is
expected to fail until the named work lands (`docs/tasks/core.md`).

| Id | Proves |
|---|---|
| S01 | config set/get round-trips through `vapor.json`; structured keys are stored as JSON |
| S02 | daemon reaches Running, creates the missing local sync root, binds the IPC socket |
| S03 | local writes become durable intents and drain; the cloud root matches byte for byte |
| S04 | pause flips run_state over IPC, resume restores it, the paused backlog drains |
| S05 | a second daemon on the same `VAPOR_DIR` exits non-zero saying one is already running |
| S06 | `vapor doctor` reports no failures inside the sandbox, in text and `--json` |
| S07 | clean SIGTERM shutdown, restart on the same state DB, post-restart writes converge |
| S08 | a healthy run across two restarts emits no ERROR line and no warning |
| S09 | an over-budget `VAPOR_DIR` relocates the IPC socket; status and doctor still work |
| S10 | uploads land byte for byte, a cloud-born file downloads through reconcile, POSIX modes survive |
| S11 | a path that diverged on both sides while the daemon was down keeps both payloads |
| S12 | pull-only materializes cloud content locally and removes a local-only file without uploading |
| S13 | diagnostics answers over IPC; the support bundle exports config, logs, live captures, manifest |
| S14 | ignored names never sync in either direction and never manufacture a conflict copy |
| S15 | conflicts list finds a keep-both copy; resolve promotes it; the resolution syncs; the list drains |
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
| S31 | a burst of local deletions is held whole behind a `mass-deletion` decision while other work continues; `vapor decisions resolve --choose apply` releases it |
| S32 | with the state DB deleted, a restart rebuilds the index from both trees without loss or invented conflicts |
| S33 | two cloud files differing only by case both materialize locally, the second as a conflict copy (known gap) |
| S34 | the cloud root disappears mid-run: sync blocks with Error; it returns: work resumes and converges |
| S35 | the shipped `vapord` binary starts, syncs, and shuts down cleanly like `vapor run` |
| S36 | nested directories flow up and down; an emptied directory's files are removed |
| S37 | push-only overwrites a same-size cloud edit with no index row for the pair |
| S38 | a cloud name that would alias a differently-cased local file never rewrites either cloud object, never loops, and lands on the timeline |
| S39 | a write reported moments before SIGTERM becomes a durable intent at shutdown and uploads right after the restart |
| S40 | a burst of cloud deletions is held before it touches this device; `--choose discard` restores the cloud copies from the local ones |
| S41 | a cloud edit made while the daemon was down that keeps the byte count is found by the startup reconcile and downloaded, with no conflict copy |
| S42 | a file removed on this device because the cloud deleted it lands in the trash; `vapor trash list` shows it and `vapor trash restore` brings it back and re-uploads it |
| S43 | an empty folder appearing where the adopted local root was opens a `root-replaced` decision and syncs nothing; `reattach` merges the cloud into it with no deletion anywhere |
| S44 | a daemon started while the adopted local root is missing parks the profile with a `root-missing` decision, never re-creates the folder, and resumes on its own when the volume returns |
| S45 | a deleted cloud root is never re-created on Vapor's own; the `root-missing` decision answered `recreate` re-creates it and re-uploads this device's files |
| S46 | a name that is a file here and a folder in the cloud opens a `type-mismatch` decision and touches nothing; `keep-both` moves the file to a conflict name and brings the folder down |
| R01 | install → start → status → crash-loop supervision through backoff and pause → acknowledge → stop → uninstall against real launchd (`--full`) |

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
- **Budget.** The default suite runs in a few minutes; each known-gap
  scenario costs its timeout until the product catches up. Long or
  load-shaped runs belong to the soak tier, not here.

## What Tier E2E deliberately does not cover (today)

- **Load and duration.** Hours of churn, fault injection at random
  points, and the model-checked no-loss oracle belong to the soak
  driver (`tools/soak`, `docs/development/soak-testing.md`).
- **Live Google Drive.** The mode exists as a flag; the Drive-side
  operations and the credentials wait on the project owner (above).
- **macOS app UI.** Owner-verified manually, per the standing test
  carve-out. Tier E2E's job there is the handoff checklist.
- **Linux and Windows daemons.** Their scenarios skip until the native
  platform traits ship; the harness itself builds and runs `S01` there
  on every PR.

## Relationship to the other tiers

| Tier | Entry point | Gate | Proves |
|---|---|---|---|
| Tier 1 | `./scripts/test.sh` | every PR | module + composed correctness, in-process |
| Tier E2E | `./scripts/e2e.sh` | every PR (all three OS jobs; `--full` on macOS) + locally for runtime-affecting changes | the shipped binaries work black-box, end to end |
| Tier 2 | `./scripts/perf.sh` | release pipeline | one soak cell with the SLO checks asserted on its report |
| Tier S | `./scripts/soak.sh` | nightly + on demand | hours of churn with faults; nothing lost, both trees converged (`soak-testing.md`) |

Tier E2E complements Tier 1; it never replaces the Tier 1 tests that
`AGENTS.md §9.2` requires.
