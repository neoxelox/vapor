---
name: vapor-e2e
description: Runs Vapor's Tier E2E verification (the real `vapor` and `vapord` binaries, black-box through the CLI, one disposable sandbox per scenario under the repo-local `.vapor/e2e/`) through the `tools/e2e` harness, as the scripted suite, a single scenario, or a manual sandbox with a live daemon; and adds a scenario for new behaviour. Use after a feature or fix that changes daemon- or CLI-observable behaviour once `./scripts/test.sh` passes and before committing; to watch a feature work or reproduce a bug in the real product; or when a scenario must be written or a known gap flipped. Runs on the host it is built on (macOS today; daemon scenarios skip elsewhere until the native traits ship).
license: GPL-3.0-only
---

## What I do

I run Vapor's Tier E2E verification: the real `vapor` + `vapord`
binaries, driven only through the CLI, inside disposable sandboxes
under `<repo>/.vapor/e2e/`. The harness is the `vapor-e2e` crate in
`tools/e2e`; `./scripts/e2e.sh` builds it and forwards arguments.
Three modes:

1. **Scenario suite** (`./scripts/e2e.sh`): every scenario the host can
   run, each in its own sandbox, each ending with the tree oracle and
   the log-hygiene check.
2. **One scenario** (`./scripts/e2e.sh --only S23`): the loop while
   implementing or fixing something.
3. **Manual sandbox** (`./scripts/e2e.sh --sandbox`): a provisioned,
   running daemon I can poke at.

Full process doc: `docs/development/e2e-verification.md`. Policy:
`AGENTS.md §9.8`. Catalog: `./scripts/e2e.sh --list`.

## When to use me

- After a feature or bug fix that changes behavior observable through
  the daemon or CLI: run the suite after Tier 1 passes and before
  committing.
- While developing: run the one scenario that covers the change, or
  poke at a manual sandbox.
- Not needed for doc-only, UI-only, or test-only changes.

## Safety rules (absolute)

- Everything happens inside `<repo>/.vapor/e2e/`. Never touch
  `~/.vapor`.
- Never pass `--full` on a developer machine: it installs a real
  LaunchAgent (the `R01` round-trip) and is for disposable CI runners.
  It refuses when a `sh.arn.vapor.daemon` LaunchAgent exists.
- Never open the macOS app or any packaged app.
- No network. The filesystem provider is the default; `--provider
  gdrive` is opt-in and needs credentials the owner provides.
- Stop what you started: `./scripts/e2e.sh --sandbox-stop` removes
  manual sandboxes and their daemons; `./scripts/clean.sh` does the
  same for all of `.vapor`.

## Mode 1 and 2: the suite and one scenario

```
./scripts/e2e.sh                     # build + all
./scripts/e2e.sh --skip-build        # reuse target/debug binaries
./scripts/e2e.sh --only S16,S23      # by id or name
./scripts/e2e.sh --keep              # keep sandboxes after a green run
./scripts/e2e.sh --json out.json     # machine-readable report
./scripts/e2e.sh --daemon vapord     # drive the shipped daemon binary
```

Read the verdict line per scenario: `PASS`, `FAIL`, `SKIP` (with the
need the host lacks), `KNOWN-GAP` (expected failure, marker names the
gap), `FIXED?` (a known-gap scenario passed; remove the marker in the
same change). The run is green only with zero `FAIL` and zero
`FIXED?`. The JSON report holds the same facts plus notes and
preserved sandbox paths.

On `FAIL` the harness prints status, diagnostics, queue rows, and log
tails for every daemon in the scenario and keeps the sandbox. Read
those first, then use the `vapor-debug` skill on the preserved
directory.

## Mode 3: manual sandbox

```
./scripts/e2e.sh --sandbox [--skip-build] [--daemon vapord]
```

Prints the layout, the daemon PID, and a cheat-sheet. Typical loop:

```
export VAPOR_DIR="<printed home path>"
vapor=target/debug/vapor

echo hi > "<printed local root>/demo.txt"    # feed the watcher
$vapor status --json                         # daemon state
$vapor diagnostics --json                    # why an intent is stuck
$vapor logs --tail 50                        # watch the pipeline react
$vapor pause / resume / flush-now / reconcile
target/debug/vapor-e2e verify-trees "<local>" "<cloud>"   # tree oracle by hand
```

Finish with `./scripts/e2e.sh --sandbox-stop`.

## Writing a scenario

1. Pick the group file under `tools/e2e/src/scenarios/` (basics, sync,
   cli, structure, resilience, modes, service) and add an entry to its
   `scenarios()` list: next free `Sxx` id, kebab-case name, one-line
   `proves` stating the invariant, `needs`, `expect`.
2. Write the body as `fn name(ctx: &mut Ctx) -> Result<(), Failure>`.
   Configure the scope (`ctx.configure_scope(&home)`), start a daemon
   (`ctx.start_daemon()`), make the change, wait with a `mark` +
   `converge_from(&mark, n, timeout)` or `settle(timeout)`, then
   assert on files with `ensure!`. Declare expected warnings
   (`ctx.allow_warning`), and opt out of the oracle only with a reason.
3. Run it alone (`--only Sxx`), then run it against the base commit:
   it must fail before your product change and pass after. Quote both
   runs in the report.
4. If the scenario documents behavior the product does not have yet,
   mark it `Expect::KnownGap("what is missing, in words")` and add the
   task to `docs/tasks/core.md`; never weaken the assertion.
5. Update the catalog table in `docs/development/e2e-verification.md`
   (mirror of `--list`).

Rules: observe only through product surfaces; bounded waits with named
conditions, no bare sleeps (`hold_for` proves that something stays
true); one behavior per scenario; own your setup.

## Known limits (today)

- The filesystem provider plays the cloud; the Google Drive mode waits
  on credentials and the Drive-side operations (`docs/tasks/core.md`).
- Linux and Windows: the harness builds and runs `S01` on their CI
  jobs; daemon scenarios skip until the native traits ship.
- Long runs, random fault injection, and the no-loss model checker are
  the soak tier's job, not this one's.
