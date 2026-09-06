# Soak testing (Tier S)

Authoritative reference for Vapor's long-run tier: the real `vapor` and
`vapord` under hours of file churn, with faults, and a model that says
whether anything was lost. The policy summary lives in `AGENTS.md
§9.5`; the agent procedure is the `vapor-soak` skill.

## Why this tier exists

Tier 1 proves the modules; Tier E2E proves the shipped binaries on
scripted scenarios that each last seconds. Neither answers the question
a user asks before pointing Vapor at their own files: does it still do
the right thing after the ten-thousandth operation, after the daemon
was killed mid-transfer for the fifth time, after the disk filled up
while a folder was being renamed? A soak answers that with evidence:
every operation is recorded, every phase ends with an oracle, and the
first violation freezes the run with the sandbox intact.

## What a soak is

`./scripts/soak.sh` builds the driver (`tools/soak`, crate
`vapor-soak`) and the product binaries, provisions a sandbox under
`.vapor/e2e/soak-<id>/`, starts a daemon on it, and runs phases until
the planned duration elapses:

| Phase | What happens |
|---|---|
| `seed` | the local side creates a tree of files |
| `local-churn` | the local side creates, overwrites, edits in place, appends, renames, moves, deletes, removes subtrees, makes directories, flips modes |
| `cloud-churn` | the same, on the cloud root (another device, the web UI) |
| `both-churn` | both sides at once, on disjoint halves of the files, interleaved |
| `conflicts` | the same paths edited on both sides at once (two-way only) |
| `quiet` | nothing, so idle health can be sampled |

The cycle repeats. Every operation is seeded: the same `--seed` replays
the same operations in the same order, and every file's content is
derived from `(seed, path, version)` and begins with a header line that
names all three, so a preserved sandbox is readable by hand.

After every phase the driver waits for the daemon to go quiet (queue
empty and no new intent for ten seconds, a requested reconcile, the
same again) and then runs the oracle.

## The oracle

The model holds the last write per path. Inside a phase a path has one
writer, and phases are separated by quiescence, so once the daemon has
converged both trees must hold exactly the model. The checks, in
order:

1. **Convergence.** The local root and the cloud root hold the same
   files (paths, sizes, content hashes; ignored and provider-internal
   names excluded).
2. **No loss, no wrong content.** Every path in the model exists on
   both sides with the recorded hash; a deleted path is gone from both.
3. **Contested paths.** For the conflict phase only: both payloads
   survive somewhere (canonical or a replicated `~conflict-` copy) and
   the canonical is one of the two.
4. **Reverts** (one-way modes): a write on the non-authoritative side
   has been undone.
5. **No invention.** Every file's content on either side was written by
   the workload at some point. A hash the model has never seen is a
   truncated or corrupted payload.

Plus, per phase: no `[ERROR]` line in the daemon log (a violation),
and any warning outside the expected set is listed in the report for
triage (never a violation by itself). The executable bit is not part
of the contract: modes carry over on transfers, but a permissions-only
change does not propagate (`data-flow.md`).

The model adopts what the daemon legitimately creates (conflict
copies, a restored file) after a clean phase, so later phases can
touch those files too.

## Faults

`--faults` selects what the driver does to the daemon and its world:

| Fault | Mechanism | Fires |
|---|---|---|
| `crash` | SIGKILL, then a restart | between operations |
| `crash-mid-transfer` | waits for a leased intent, SIGKILL, restart | between operations |
| `freeze` | SIGSTOP for 3 to 15 s, then SIGCONT | between operations |
| `pause-resume` | `vapor pause`, a few operations, `vapor resume` | between operations |
| `config-reload` | `resourceLimits.bandwidthPercent` to 5 and back through `vapor config set` | between operations |
| `cloud-root-vanish` | the cloud root is renamed away for the phase and restored after it | per phase |
| `throttle-walk` | with `--throttle walk`, the throttle inputs file is set to idle, battery, user-active, or overloaded for the phase | per phase |
| `disk-full` | with `--cloud-image-mb`, the cloud volume is filled to the last block for the phase | per phase |

`none`, `crash`, `all`, or a comma-separated list. Extra operations a
fault performs stay on the phase's side and path subset, so the
one-writer rule holds through a fault. After a crash the driver
restarts the daemon itself (a CLI-only install has no supervisor); the
mass-deletion guard holding a subtree removal is answered with `vapor
decisions resolve <id> --choose apply` and recorded as a `guard-trip`,
since that is the product working as designed. While the driver has
the cloud root parked, the daemon holds the profile with a
`root-missing` question; the driver expects that state, leaves the
question alone, and after restoring the root waits for the daemon to
lift the hold on its own. Any other decision the daemon opens fails
the run: the driver does not know the right answer and never guesses.
A daemon that re-creates the parked root is a finding.

## Throttle walk

`--throttle walk` starts the daemon with
`VAPOR_THROTTLE_INPUTS=file:<sandbox>/throttle-inputs.json`. The daemon
re-reads that document on every sample (a `ThrottleInputs` object under
`inputs`, plus `idle_seconds`), so the driver walks it through
`IdleDrain`, `Light`, `Throttled`, and `Suspended` while the workload
runs. A missing or half-written document samples as the neutral
defaults, so the daemon never wedges on the driver. `vapor doctor`
names the source.

## Health and SLOs

Every ten seconds the driver samples the daemon's resident memory, CPU
utilisation since the previous sample, thread count, and (every sixth
sample) open descriptors, through `ps` and `lsof`. The report carries
the maximum and the 95th percentile of each, and the checks from
`docs/performance/acceptance-budgets-and-benchmark-harness.md`: RSS
p95 under the storm budget, CPU average under the active budget, every
injected crash followed by a converged phase with no loss, every phase
converged inside its deadline. The CPU budget only means something
against the release profile (`--release`); a debug daemon burns CPU no
user sees.

## Running it

```
./scripts/soak.sh --duration 2h                     # two-way, mixed load, no faults
./scripts/soak.sh --duration 1h --faults all --throttle walk --cloud-image-mb 512
./scripts/soak.sh --duration 30m --mode pull-only --load bulk
./scripts/soak.sh --duration 20m --load large --faults crash --release
./scripts/soak.sh --seed 42 --duration 10m --continue-on-violation   # collect, do not freeze
./scripts/soak.sh --status .vapor/e2e/soak-<id>/soak-status.json     # one line
./scripts/soak.sh --verify .vapor/e2e/soak-<id>                      # oracle on a preserved sandbox
```

Load shapes: `mixed` (default), `trickle` (an operation every few
seconds), `coding` (many small edits of code and text), `bulk`
(thousands of small files), `large` (files of tens of megabytes).

The console prints one line per phase and one per fault. The files
that matter live in the sandbox:

| File | Rewritten | Holds |
|---|---|---|
| `soak-status.json` | every five seconds | run state (`running`, `frozen`, `finished`, `failed`), phase, counts, latest and summary health, the last verdict, the daemon pid |
| `soak-report.json` | at the end, or at the freeze | every phase's verdict, every fault, every violation, the SLO checks, the daemon's error lines |
| `ops.jsonl` | continuously | one line per operation, fault, and note, with the version and hash written |
| `model.json` | after every phase | the model, for `--verify` |
| `home/logs/vapord.logs` | by the daemon | the daemon log |

A clean run deletes its sandbox (keep it with `--keep`) and leaves the
report at `.vapor/e2e/soak-last-report.json`. The first violation
freezes the run: the workload stops, the daemon stays up, the cloud
volume stays mounted, the report is written, and the exit code is 1.
`--continue-on-violation` records violations and keeps going instead,
for a run meant to collect. `./scripts/clean.sh` stops the daemon,
detaches the volume, and removes everything.

## Reading a violation

A violation names its kind, the path, and the detail: which side, which
version, how many bytes, which hashes. The triage order:

1. `ops.jsonl`, filtered on the path: which side wrote which version
   when, and which faults fired around it.
2. `home/logs/vapord.logs` around those timestamps: what the daemon
   decided (a keep-both, a refused delete, a retry).
3. The trees: the file's header line names its version.
4. `vapor status --json` and `vapor diagnostics --json` against the
   frozen daemon (`export VAPOR_DIR=<sandbox>/home`).

The model is the specification, not a suggestion. A violation is a
finding until a person decides otherwise; the driver and the model are
never edited to make a run green. The one legitimate reason to change
the model is a documented product rule it does not encode yet (the way
mode-only changes are excluded), and that change comes with the doc
pointer.

## Sandbox discipline

The same rules as Tier E2E: everything under `.vapor/e2e/`, never
`~/.vapor`, no service install, no app launch, no network with the
filesystem provider. A disk image is created and mounted under the
sandbox with `hdiutil` and detached by the driver or by `clean.sh`.

## Where the long runs happen

Locally, a run of a few minutes is a smoke check; the runs that find
things are hours long. The scheduled `soak.yml` workflow runs a matrix
of cells on the macOS runner; `scripts/perf.sh` runs one bounded cell
in the release pipeline and asserts the SLO checks on its report. Linux
cells wait for the native Linux traits (`docs/tasks/core.md`).
