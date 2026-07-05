# End-to-end verification (Tier E2E)

Authoritative reference for Vapor's end-to-end verification tier. The
policy summary lives in `AGENTS.md §9.8`; this document is the full
process: what Tier E2E is, when it is required, how to run it, how to
extend it, and what it deliberately does not cover.

## Why this tier exists

Vapor is coded autonomously. Tier 1 (`./scripts/test.sh`) proves that
modules and composed behaviors are correct *in-process*, but a coding
agent that only ever runs unit and integration tests has never watched
the product actually work: real binaries, real process boundaries, real
FSEvents, a real durable DB on disk, real IPC over a real socket, real
signals. Tier E2E closes that gap. It is the closest an autonomous
agent gets to "I ran the app and it worked" — without touching the
machine it runs on.

## What Tier E2E is

`./scripts/e2e.sh` builds the two shipping Rust binaries (`vapor`,
`vapord`), then drives the daemon **black-box through the CLI only** —
exactly like a headless user would — against a disposable sandbox:

```
<repo>/.vapor/e2e/run-<timestamp>-<pid>/
├── home/    ← VAPOR_DIR: vapor.json, logs/, state/, vapord.sock, vapord.lock
├── local/   ← the watched local sync root (created by the daemon itself)
└── cloud/   ← the configured cloud-root path (a label for the stub provider today)
```

Observation channels are the product's own observable surfaces, never
test hooks: `vapor status --json`, `vapor doctor`, exit codes, the
daemon log (`home/logs/vapord.logs`), and **read-only** queries against
the durable state DB (`home/state/vapor.sqlite`).

## Safety contract (hard rules)

Tier E2E must be safe to run unattended on a contributor machine or CI:

- Everything lives under the repo-local `.vapor/e2e/` sandbox.
  `./scripts/clean.sh` removes all residue. Never touch `~/.vapor`.
- Never install host services: no `vapor service install`, no
  LaunchAgent/launchd mutation, no login items. (`vapor doctor` *reads*
  host state; that is fine.)
- Never launch the macOS app (`AGENTS.md §7.1`: agents do not open
  packaged apps). Runtime behavior is verified through the CLI.
- No network. Today's provider is the filesystem stub; the future live
  provider tier is explicitly gated (see below) and is never part of
  the default run.
- A failed run preserves its sandbox and prints the path; a green run
  deletes it (keep it with `--keep`).

## When an agent must run it

After Tier 1 passes, run `./scripts/e2e.sh` before committing when the
change plausibly alters end-to-end runtime behavior:

- new features in `core/daemon`, `core/cli`, `core/ipc`, `core/shared`
  (config/paths/logging), `core/lifecycle`, `core/providers`, or
  `core/platform`;
- bug fixes whose failure mode a user would see through the daemon or
  CLI (sync stalls, wrong state reporting, startup/shutdown problems,
  config not applying, …);
- changes to startup order, signal handling, IPC contracts, durable
  schema, or the build of the shipping binaries.

Doc-only, UI-only (Swift view/menubar), or test-only changes do not
need an E2E run. When in doubt, run it — a green run costs well under a
minute after the build.

Two additional obligations when Tier E2E applies:

1. **Feature coverage.** If the change adds e2e-observable behavior
   (a new CLI command, a new daemon state, a new convergence path),
   extend the harness with a scenario for it *in the same change set* —
   running only the pre-existing scenarios verifies that you broke
   nothing, not that the feature works.
2. **Owner handoff.** If the change also affects a UI surface, finish
   your report with a short manual-verification checklist for the
   project owner (what to open, what to click, what they should see),
   since agents never verify UI.

## Running it

```
./scripts/e2e.sh               # scenario suite: build + all scenarios
./scripts/e2e.sh --skip-build  # reuse target/debug binaries (~15 s)
./scripts/e2e.sh --keep        # preserve the sandbox after a green run
./scripts/e2e.sh --sandbox     # manual sandbox: provision + leave running
```

Output is one `PASS`/`FAIL` line per scenario. On failure the script
dumps `vapor status --json`, the daemon log tail, and the sandbox path,
then exits non-zero. Use the `vapor-debug` skill (or read
`home/logs/vapord.logs` and query `home/state/vapor.sqlite` read-only)
to diagnose a preserved sandbox.

On CI, the suite runs as the final step of `test.yml`'s macOS job on
every PR (part of the required `test` check). macOS only — the harness
exercises the native FSEvents watcher, and macOS is the shipping
surface.

## Manual sandbox — exploratory testing and debugging

The scripted scenarios prove non-regression; they cannot explore. When
developing a feature or chasing a bug, run the product yourself, scoped
to the same disposable sandbox:

```
./scripts/e2e.sh --sandbox
```

This builds the binaries, provisions a fresh sandbox under
`.vapor/e2e/sbx-…`, starts the daemon, and prints a cheat-sheet: the
`VAPOR_DIR` export, the watched local root, the log/state-DB paths, the
daemon PID, and the CLI commands to poke at it. The daemon keeps
running after the script exits; stop it with `kill -TERM <pid>` and
remove the sandbox with `rm -rf` (or `./scripts/clean.sh`).

Typical loop, entirely inside the sandbox:

```
export VAPOR_DIR="<repo>/.vapor/e2e/sbx-…/home"   # printed by --sandbox
vapor=target/debug/vapor

echo hello > "$VAPOR_DIR/../local/demo.txt"   # feed the watcher a change
$vapor status --json                          # observe daemon state
$vapor logs --tail 50                         # watch the pipeline react
$vapor pause; $vapor resume; $vapor flush-now # drive it over IPC
sqlite3 -readonly "$VAPOR_DIR/state/vapor.sqlite" \
  'SELECT path_text, kind, state FROM queue_intents;'
```

The same safety contract applies: with `VAPOR_DIR` pointing into the
sandbox, the daemon's entire universe (config, logs, durable state,
socket, lock) is path-scoped there by design — nothing on the host is
touched. Never run manual experiments against `~/.vapor` or with
`VAPOR_DIR` unset; that is the project owner's real runtime dir.

Note on deep paths: macOS caps Unix-socket paths at ~104 bytes. When
`<vapor_dir>/vapord.sock` exceeds the budget, daemon and CLI
deterministically rendezvous at a short per-`vapor_dir` socket under
the OS temp dir instead (the S9 scenario covers this; `vapor doctor`'s
`ipc_socket_path` probe explains it when active). The harness first
surfaced this failure mode — pre-fix, a deep `VAPOR_DIR` silently cost
the daemon its IPC endpoint.

## Why not a Docker container?

Considered and deliberately not used, for now:

- A container on macOS is a Linux VM: the daemon inside it would use
  the Linux fs-watch path, not the native FSEvents watcher that
  actually ships. Linux is not a shipping surface yet; the E2E tier
  must exercise the real one.
- Isolation is already achieved by design: `VAPOR_DIR` scoping makes
  the repo-local sandbox the daemon's entire universe, the default
  provider has no network side, and the harness never installs host
  services. There is no residual host risk for a container to remove.
- The Docker toolchain is not part of the contributor baseline, and a
  VM boundary would slow the agent's feedback loop.

Revisit when Linux becomes a shipping surface (containerized Linux E2E
in CI is the natural fit then) or when a live cloud-provider tier needs
network egress control.

## Scenario catalog (current)

| # | Scenario | Proves |
|---|----------|--------|
| S1 | Config round-trip | `vapor config set/get` writes and reads `vapor.json` under `VAPOR_DIR` |
| S2 | Daemon startup | `vapor run` reaches `Running`, creates the missing local sync root itself, binds the IPC socket |
| S3 | Local ingest converges | real file writes → FSEvents → debounce → durable intents (≥3 captured, observed via the `queue_intents` high-water mark) → executor → queue drains, `failed_intents` stays empty |
| S4 | Pause/resume | IPC pause flips `run_state` to `Paused`, resume restores `Running`, backlog written while paused drains |
| S5 | Singleton lock | a second daemon on the same `VAPOR_DIR` exits non-zero with "already running" |
| S6 | Doctor | `vapor doctor` reports no failures inside the sandbox |
| S7 | Restart recovery | clean SIGTERM shutdown, restart on the same state DB, post-restart writes still converge |
| S8 | Log hygiene | a healthy run emits zero `[ERROR]` lines |
| S9 | Socket relocation | with an over-budget `VAPOR_DIR` the IPC socket relocates deterministically under the OS temp dir; `vapor status` still reaches the daemon and `vapor doctor` explains the relocation |

## Extending the harness — discipline rules

- **Observe through product surfaces.** CLI exit codes and `--json`
  output, the daemon log, and read-only state-DB queries. Never add a
  test-only hook to the daemon for the harness's benefit.
- **Bounded waits, never bare sleeps.** Every wait is a
  `wait_until <deadline> <description> <predicate>` poll with a hard
  deadline and a named condition. A scenario that needs "sleep 5 and
  hope" is not deterministic enough to land.
- **Keep it fast.** The whole suite must stay under ~60 s after the
  build. Long-running or load-shaped scenarios belong to Tier 2
  (`scripts/perf.sh`), not here.
- **Agent-friendly failures.** A failing scenario must say what was
  expected, dump enough context to debug (status, log tail, sandbox
  path), and preserve the sandbox.
- **One scenario, one behavior.** Same rule as Tier 1: if the name
  needs "and", split it.
- **JSON parsing:** the `--json` shapes are locked by Tier 1 snapshot
  tests, so simple `grep` extraction is acceptable; do not add new
  host-tool dependencies beyond what macOS/CI ship (`sqlite3` is fine,
  `jq` is not assumed).

## What Tier E2E deliberately does not cover (today)

- **Byte replication to a cloud.** The pre-GA default provider is the
  inert `FilesystemStubProvider`, so S3/S7 prove pipeline convergence
  (capture → durable queue → executor → drained), not remote content.
  When the Wave 8 filesystem reference provider lands (C8-1..13),
  extend S3 to assert actual file content on the "cloud" side; the
  same scenario then covers remote→local when bidirectional sync
  ships.
- **Live cloud providers.** A future, explicitly gated tier: real
  Google Drive against a dedicated test account, enabled only by an
  explicit opt-in flag, riding the release pipeline like Tier 2 —
  never a PR gate, never run implicitly by an agent. Design it when
  C8-48 lands.
- **`vapor service install` round-trips.** Installing a LaunchAgent
  mutates the host; that round-trip runs in CI per `AGENTS.md §9.7`,
  not on contributor machines.
- **macOS app UI.** Owner-verified manually, per the standing test
  carve-out. Tier E2E's job there is the handoff checklist, not the
  verification.

## Relationship to the other tiers

| Tier | Entry point | Gate | Proves |
|------|-------------|------|--------|
| Tier 1 | `./scripts/test.sh` | every PR | module + composed correctness, in-process |
| Tier E2E | `./scripts/e2e.sh` | every PR (macOS CI job) + locally for runtime-affecting changes | the shipped binaries work black-box, end to end |
| Tier 2 | `./scripts/perf.sh` | release pipeline | performance SLOs, long property/fuzz/loom runs |

Tier E2E complements Tier 1 — it never replaces writing the Tier 1
tests that `AGENTS.md §9.2` requires.
