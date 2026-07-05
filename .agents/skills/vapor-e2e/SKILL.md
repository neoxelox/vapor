---
name: vapor-e2e
description: Verify Vapor end to end by running the real vapor CLI and vapord daemon in a disposable repo-local sandbox — scripted regression scenarios plus a manual sandbox for exploratory feature testing and bug reproduction
license: MIT
metadata:
  audience: developers
  platform: macos
---

## What I do

I run Vapor's **Tier E2E** verification: the real `vapor` + `vapord`
binaries, black-box, driven only through the CLI, inside a disposable
sandbox under the repo-local `.vapor/e2e/` directory. Two modes:

1. **Scenario suite** (`./scripts/e2e.sh`) — the scripted regression
   pass: startup, config, ingest→converge, pause/resume, singleton
   lock, doctor, restart recovery, log hygiene.
2. **Manual sandbox** (`./scripts/e2e.sh --sandbox`) — a provisioned,
   running daemon I can poke at interactively to exercise a new
   feature or reproduce a bug.

Full process doc: `docs/development/e2e-verification.md`. Policy:
`AGENTS.md §9.8`.

## When to use me

- After a big feature or bug fix that changes behavior observable
  through the daemon or CLI — run the suite after Tier 1
  (`./scripts/test.sh`) passes and before committing.
- While developing: use the manual sandbox to watch the feature work
  (or the bug reproduce) in the real product before/while writing
  tests.
- Not needed for doc-only, UI-only, or test-only changes.

## Safety rules (absolute)

- Everything happens inside `<repo>/.vapor/e2e/`. Never touch
  `~/.vapor` — that is the project owner's real runtime dir.
- Never run `vapor service install` / `launchctl` mutations, and never
  open the macOS app or any packaged app — UI verification is the
  project owner's job.
- No network. Today's default provider is the inert filesystem stub;
  the live cloud tier is future work and explicitly gated.
- Residue is removed by `rm -rf` of the run directory or
  `./scripts/clean.sh`. Stop any daemon you started (`kill -TERM
  <pid>`) before finishing.

## Mode 1 — scenario suite

```
./scripts/e2e.sh               # build + all scenarios
./scripts/e2e.sh --skip-build  # reuse target/debug binaries (~15 s)
./scripts/e2e.sh --keep        # keep the sandbox after a green run
```

One `PASS`/`FAIL` line per scenario. On failure the script dumps
`vapor status --json`, the daemon log tail, and the preserved sandbox
path — investigate with the `vapor-debug` skill or directly:

- daemon log: `<sandbox>/home/logs/vapord.logs`
- durable state (read-only!):
  `sqlite3 -readonly <sandbox>/home/state/vapor.sqlite 'SELECT path_text, kind, state FROM queue_intents;'`

The same suite runs on CI as the last step of `test.yml`'s macOS job,
so a locally green run is what keeps the PR green.

## Mode 2 — manual sandbox

```
./scripts/e2e.sh --sandbox [--skip-build]
```

Prints the sandbox layout, the daemon PID, and a command cheat-sheet,
then leaves the daemon running. Typical loop:

```
export VAPOR_DIR="<printed home path>"
vapor=target/debug/vapor

echo hi > "<printed local root>/demo.txt"   # feed the watcher
$vapor status --json                        # daemon state
$vapor logs --tail 50                       # watch the pipeline react
$vapor pause / resume / flush-now           # drive it over IPC
$vapor doctor                               # sanity checks
```

Finish by stopping the daemon (`kill -TERM <pid>`, verify with
`pgrep -fl "vapor run"`) and removing the sandbox directory.

## After the run — obligations

1. **Feature coverage.** If your change added e2e-observable behavior
   (new CLI command, new daemon state, new convergence path), add or
   extend a scenario in `scripts/e2e.sh` in the same change set.
   Discipline rules (bounded `wait_until` polls, product-surface
   observation only, ~60 s budget, agent-friendly failures) are in
   `docs/development/e2e-verification.md`.
2. **Report honestly.** Quote the scenario output in your summary. A
   red run blocks the commit — fix or explain, never skip silently.
3. **Owner handoff for UI.** If the change also touches a UI surface,
   end your report with a short manual checklist for the project
   owner: what to open, what to click, what they should see.

## Known limits (today)

- The stub provider has no cloud side: the suite proves pipeline
  convergence (watch → debounce → durable queue → executor → drained),
  not byte replication. Replication assertions arrive with the Wave 8
  filesystem reference provider.
- `vapor timeline` returns an empty list until C8-30 lands.
- macOS caps Unix-socket paths (~104 bytes); the harness guards this,
  but hand-rolled deep sandbox paths cost the daemon its IPC endpoint
  (it logs a WARNING and keeps syncing without `vapor status`).
