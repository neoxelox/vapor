# CI

GitHub Actions workflows and the policy around them. Use this directory
when you want to understand which checks run, where they run, what
triggers them, and which ones gate a release.

## How to use this group

- **Adding a new workflow or modifying an existing one?** Read
  `overview.md` first for the trigger policy and toolchain defaults.
- **Configuring branch protection?** Use `required-checks.md` — it lists
  exactly which checks must be required on `main` and which are
  release-only gates.
- **Running the same validation locally?** Every CI workflow runs the
  same `./scripts/*` entry points a contributor would run locally. See
  `docs/development/runbook.md` for the local command set.

## Documents

- `overview.md` — workflow catalog (`lint.yml`, `test.yml`, `build.yml`,
  `perf.yml`, `release.yml`), triggers (`pull_request`, `push` to `main`,
  `workflow_call`, `v*` tags), toolchain defaults (`macos-latest`,
  latest-stable Xcode/Swift, stable Rust), pinned CI action versions,
  dependency caching strategy.
- `required-checks.md` — branch-protection guidance (`lint`, `test`,
  and `build` required on `main`; `perf` release-only),
  workflow-to-script mapping, local-parity command set.

## Test tier model

Testing runs in three tiers. Authoritative definition:
`docs/architecture/testing-strategy.md §CI tier execution`.

- **Tier 1** — `lint.yml` + `test.yml` (runs on every PR; required
  checks on `main`). Unit + integration + platform-trait contract +
  property + snapshot + guard-rail timing tests. **Budget: under
  5 minutes per OS on CI.** If a change pushes this past the budget,
  split slow tests out to Tier 2 or make them faster.
- **Tier 2** — `perf.yml` (release gate only; no standalone triggers —
  invoked solely by `release.yml`). Performance SLO tests, long-running
  property cases (higher case counts), fuzz corpora, `loom`-backed
  concurrency tests. **Not a PR gate.**
- **Tier E2E** — `./scripts/e2e.sh --full` at the end of `test.yml`'s
  macOS job (every PR; part of the required `test` check). Black-box
  run of the real `vapor` + `vapord` binaries in a sandbox under the
  repo-local `.vapor/e2e/` — macOS only, because it drives the native
  FSEvents watcher on the shipping surface. Also part of the local
  validation loop for runtime-affecting changes (`AGENTS.md §9.8`;
  process in `docs/development/e2e-verification.md`), where
  contributors run it *without* `--full`: that flag appends the
  host-mutating `vapor service` round-trip against real `launchd`,
  which installs a real LaunchAgent and is therefore meant for
  disposable CI runners only.

A CI timing guard (tracked as `core.md` CT-2) fails the job if Tier 1
exceeds the 5-minute budget on a matrix runner. The failure message
points contributors at the testing-strategy doc's discipline rules
rather than silently accepting a regression.

## Target state (portability work)

As the `core/*` portability fixes land (see `docs/plans/core.md` §4 and
`docs/tasks/core.md` Phase C1), the Rust jobs in `lint.yml` and
`test.yml` will grow a matrix of `macos-latest`, `ubuntu-latest`,
`windows-latest`. Swift jobs stay macOS-only because Swift code is
macOS-only by policy (`AGENTS.md §8`). Per-platform release jobs
(macOS, Windows, Linux) each target their own isolated GitHub
Environment (`release-macos`, `release-windows`, `release-linux`) for
secret isolation.
