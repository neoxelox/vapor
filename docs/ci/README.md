# CI

GitHub Actions workflows and the policy around them. Use this directory
when you want to understand which checks run, where they run, what
triggers them, and which ones gate a release.

## How to use this group

- **Adding a new workflow or modifying an existing one?** Read
  `overview.md` first for the trigger policy and toolchain defaults.
- **Touching branch protection or renaming a matrix leg?** Use
  `required-checks.md`: it records the live `main` ruleset (the nine
  required checks, bypass policy, and the command to recreate it) and
  which gates are release-only.
- **Running the same validation locally?** Every CI workflow runs the
  same `./scripts/*` entry points a contributor would run locally. See
  `docs/development/runbook.md` for the local command set.

## Documents

- `overview.md`: workflow catalog (`lint.yml`, `test.yml`, `build.yml`,
  `perf.yml`, `soak.yml`, `release.yml`), triggers (`pull_request`,
  `push` to `main`, `workflow_call`, `workflow_dispatch`, `v*` tags),
  toolchain defaults (`macos-latest`,
  latest-stable Xcode/Swift, stable Rust), pinned CI action versions,
  dependency caching strategy.
- `required-checks.md`: the `main` ruleset (`lint`, `test`, and
  `build` × macOS/Linux/Windows required on `main`; `perf` and `soak`
  release-only), how to inspect and recreate it, the squash-only merge
  method, workflow-to-script mapping, local-parity command set.

## Test tier model

Testing runs in four tiers. Authoritative definition:
`docs/architecture/testing-strategy.md §CI tier execution`.

- **Tier 1**: `./scripts/test.sh` in `test.yml` (runs on every PR; a
  required check on `main` via the `main` ruleset, next to `lint.yml`
  and `build.yml`). Unit + integration + platform-trait contract +
  property + shape-lock + guard-rail timing tests. **Budget: under
  5 minutes per OS on CI for the run; the script builds first and
  does not count the compiler.** If a change pushes this past the
  budget, split slow tests out to Tier 2 or make them faster.
- **Tier 2**: `perf.yml` (release gate, invoked by `release.yml`, and
  `workflow_dispatch` for a run by hand; no schedule). One soak cell
  against the release profile with the SLO checks on its report. Long
  property cases, fuzz targets, and `loom` tests are open work and
  join this tier when they exist. **Not a PR gate.**
- **Tier S**: `soak.yml` (release gate, invoked by `release.yml`, and
  `workflow_dispatch` for a run by hand; no schedule). Five soak cells
  across sync modes, loads, faults, and throttle walks, on macOS and
  Linux (the disk-full cell on macOS only), each 45 minutes of seeded
  churn against the real binaries with the no-loss oracle after every
  phase. **Not a PR gate.**
- **Tier E2E**: `./scripts/e2e.sh` at the end of every `test.yml`
  job, `--full` on the macOS one (every PR; part of the required
  `test` check). Black-box run of the real `vapor` + `vapord` binaries
  in a sandbox under the repo-local `.vapor/e2e/`: the daemon
  scenarios run on macOS (FSEvents) and Linux (inotify) and skip by
  name on Windows until its watcher ships. Also part of the local
  validation loop for runtime-affecting changes (`AGENTS.md §9.8`;
  process in `docs/development/e2e-verification.md`), where
  contributors run it *without* `--full`: that flag appends the
  host-mutating `vapor service` round-trip against real `launchd`,
  which installs a real LaunchAgent and is therefore meant for
  disposable CI runners only.
- **Tier VM**: `./scripts/vm.sh`, in no workflow. It tests the
  installed Mac app in a macOS virtual machine, and a GitHub-hosted
  macOS runner is a virtual machine that cannot run one. It runs on a
  contributor's Mac and as a leg of `./scripts/release.sh`
  (`AGENTS.md §9.9`; process in
  `docs/development/vm-verification.md`). The `test` jobs still
  compile its harness, run the harness's unit tests, and on macOS
  compile and lint its driver.

`./scripts/test.sh` enforces the Tier 1 budget itself: when
`VAPOR_TEST_MAX_SECONDS` is set (the `test` workflow sets `300`), the
script fails after a green run that took longer, and the message points
at the testing-strategy doc's discipline rules rather than silently
accepting a regression. Locally the variable is unset, so a slow
machine never fails the suite.

## Platform matrix

`lint.yml`, `test.yml`, and `build.yml` fan out over `macos-latest`,
`ubuntu-latest`, and `windows-latest`. Swift runs only on the macOS leg
because Swift code is macOS-only by policy (`AGENTS.md §8`); the other
legs verify the Rust workspace. `soak.yml` fans out over `macos-latest`
and `ubuntu-latest`; `perf.yml` over `macos-latest`, where its budgets
are measured. Windows joins both once its native watcher ships: the
driver skips a cell by name on a host that cannot run it. The release
pipeline's `package` job is a matrix too, one entry per shipping
platform, each under its own protected GitHub Environment
(`release-macos` in the matrix today; `release-windows` and
`release-linux` exist with the same protection and join when those
surfaces ship), feeding one `publish` job, see
`docs/operations/release-process.md`.
