# CI

GitHub Actions workflows and the policy around them. Use this directory
when you want to understand which checks run, where they run, what
triggers them, and which ones gate a release.

## How to use this group

- **Adding a new workflow or modifying an existing one?** Read
  `overview.md` first for the trigger policy and toolchain defaults.
- **Touching branch protection or renaming a matrix leg?** Use
  `required-checks.md` — it records the live `main` ruleset (the nine
  required checks, bypass policy, and the command to recreate it) and
  which gates are release-only.
- **Running the same validation locally?** Every CI workflow runs the
  same `./scripts/*` entry points a contributor would run locally. See
  `docs/development/runbook.md` for the local command set.

## Documents

- `overview.md` — workflow catalog (`lint.yml`, `test.yml`, `build.yml`,
  `perf.yml`, `release.yml`), triggers (`pull_request`, `push` to `main`,
  `workflow_call`, `v*` tags), toolchain defaults (`macos-latest`,
  latest-stable Xcode/Swift, stable Rust), pinned CI action versions,
  dependency caching strategy.
- `required-checks.md` — the `main` ruleset (`lint`, `test`, and
  `build` × macOS/Linux/Windows required on `main`; `perf`
  release-only), how to inspect and recreate it, workflow-to-script
  mapping, local-parity command set.

## Test tier model

Testing runs in three tiers. Authoritative definition:
`docs/architecture/testing-strategy.md §CI tier execution`.

- **Tier 1** — `lint.yml` + `test.yml` + `build.yml` (runs on every PR;
  required checks on `main` via the `main` ruleset). Unit + integration + platform-trait contract +
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

## Platform matrix

`lint.yml`, `test.yml`, and `build.yml` fan out over `macos-latest`,
`ubuntu-latest`, and `windows-latest`. Swift runs only on the macOS leg
because Swift code is macOS-only by policy (`AGENTS.md §8`); the other
legs verify the Rust workspace. Per-platform release jobs each target
their own protected GitHub Environment (`release-macos` today,
`release-windows` / `release-linux` when those surfaces ship) — see
`docs/operations/release-process.md`.
