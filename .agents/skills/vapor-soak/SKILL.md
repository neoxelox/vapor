---
name: vapor-soak
description: Runs and oversees Vapor's Tier S soak (the real `vapor` and `vapord` under hours of seeded file churn on both sides, fault injection, and a model-checked no-loss oracle) through `./scripts/soak.sh`, watching `soak-status.json` on a loop and triaging the first violation from `ops.jsonl`, the daemon log, and the preserved sandbox. Use when asked to prove the product is safe for real data, before a release, after a change to the executor, reconcile, deletion, or conflict paths, or to reproduce a bug that only shows up under sustained load. macOS only today.
license: GPL-3.0-only
---

## What I do

I run a soak: a real daemon in a sandbox under `<repo>/.vapor/e2e/`,
a seeded workload on both the local root and the cloud root, faults
(crashes, freezes, pauses, a vanished cloud root, a full disk, a
throttle walk), and after every phase an oracle that checks both trees
against the model of what must survive. Then I watch it, and when it
freezes I find out why.

Process doc: `docs/development/soak-testing.md`. Driver:
`tools/soak` (`vapor-soak`), built on the e2e harness library.

## Safety rules (absolute)

- Everything happens inside `<repo>/.vapor/e2e/soak-<id>/`. Never
  `~/.vapor`, never a service install, never the macOS app, no network
  with the filesystem provider.
- The model is the specification. I never edit the model, the oracle,
  or the workload to make a run green. A violation is a finding until
  the project owner decides otherwise.
- I do not poll the daemon while a soak runs; the driver already
  writes everything to `soak-status.json`. Two readers of the socket
  and the state DB only add noise.
- I stop what I started: `./scripts/clean.sh` stops the daemon,
  detaches the disk image, and removes the sandbox.

## Starting a run

```
./scripts/soak.sh --duration 2h --faults all --throttle walk --cloud-image-mb 512 --release
./scripts/soak.sh --duration 1h --mode pull-only --load bulk
./scripts/soak.sh --duration 30m --load large --faults crash
./scripts/soak.sh --seed 42 --duration 20m --continue-on-violation   # collect everything
```

Run it in the background (it prints the sandbox and the status path
first). Pick `--release` for any run whose CPU numbers matter. Pick a
seed and write it down: the same seed replays the same operations.

## Watching a run

Loop every 10 to 20 minutes (Claude Code's `/loop`) on the status
line:

```
./scripts/soak.sh --status <sandbox>/soak-status.json
```

It prints the state, the phase, the counts, elapsed over planned time,
the latest memory and CPU, and the driver's last note. Stop the loop
only when the state leaves `running`: `finished` (read the report),
`frozen` (triage), `failed` (the driver itself broke; read its
output). Nothing to report on a tick means nothing to say.

## When it freezes

The run stops generating, the daemon stays up, the volume stays
mounted, and `soak-report.json` names every violation: kind (`loss`,
`wrong-content`, `divergence`, `invention`, `not-reverted`,
`no-convergence`, `daemon-error`), path, and detail. Triage in this
order and quote what you find:

1. `grep '<path>' <sandbox>/ops.jsonl`: which side wrote which version
   when, and which faults fired around it.
2. `<sandbox>/home/logs/vapord.logs` around those timestamps: what the
   daemon decided. Then `export VAPOR_DIR=<sandbox>/home` and
   `target/debug/vapor diagnostics --json` against the frozen daemon.
3. The file's first line names its seed, path, version, and size.
4. Decide: product bug, or a rule the model does not encode yet (with
   the doc pointer that proves it). Hand off to the `vapor-debug`
   skill for the product side.

States the driver expects and you should not mistake for findings: a
`guard-trip` fault event (the mass-deletion guard held a subtree
removal and the driver answered `apply`), a `root-missing` decision
while the driver has the cloud root parked (it withdraws itself on
restore), and the daemon in `Error` during that parking. A daemon that
re-creates the parked root, or any other open decision, fails the run
on purpose.

Write the finding up with the seed, the flags, the phase, the op
sequence, and the daemon log window, so it can be replayed.

## After a clean run

Quote the report's summary: phases, operations, faults, health
maximums, and the SLO checks. `.vapor/e2e/soak-last-report.json`
holds it when the sandbox was removed. A clean run is evidence for the
cells it ran, not for the ones it did not: say which mode, load,
faults, and throttle it covered.
