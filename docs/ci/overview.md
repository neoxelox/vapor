# CI Overview

GitHub Actions workflows are defined in `.github/workflows/`:

- `lint.yml`: runs lint and format checks via repository scripts.
- `test.yml`: runs the Tier 1 suites via repository scripts, then the
  Tier E2E harness (`./scripts/e2e.sh`) on every OS job; the macOS job
  passes `--full` (the launchd round-trip on a disposable runner) and
  every job uploads the harness's JSON report as the
  `e2e-report-<os>` artifact. Scenarios whose needs the host cannot
  meet skip by name, so the Linux and Windows jobs run the harness
  and `S01` today and pick up the daemon scenarios once their native
  traits ship.
- `build.yml`: runs distribution builds via repository scripts.
- `perf.yml`: reusable performance gate workflow invoked by the release
  pipeline; runs one soak cell against the release profile and asserts
  the SLO checks on its report (`scripts/perf.sh`).
- `soak.yml`: Tier S, nightly and on demand, never a PR gate: a matrix
  of soak cells (mode, load, faults, throttle) on the macOS runner, each
  uploading its report, status, op log, model, and daemon log as the
  `soak-<cell>` artifact. `workflow_dispatch` takes a duration and a
  seed.
- `release.yml`: runs tag-driven package and GitHub Release publication flow.

## Toolchain defaults in CI

- Runners: `macos-latest` (full Rust + Swift), `ubuntu-latest` (Rust only),
  `windows-latest` (Rust only). The `-latest` aliases track GitHub's newest
  stable image (`AGENTS.md` §8.2); `macos-latest` is macOS 26 arm64 today. The Swift wrapper scripts skip themselves on
  non-Darwin hosts, so `apps/macos` is exercised only on macOS while
  `core/*` is verified on every supported OS.
- Xcode and Swift: `latest-stable` (macOS job only).
- Rust: `stable` (every job).

## Pinned CI action versions

Every `uses:` is pinned to a commit SHA with a trailing `# vX.Y.Z` comment,
and the repository enforces `sha_pinning_required`:

- `actions/checkout` v7.0.1
- `maxim-lobanov/setup-xcode` v1.7.0
- `actions-rust-lang/setup-rust-toolchain` v1.17.0
- `actions/cache` v5.1.0 for Rust (`cargo`) and SwiftPM caches
- `actions/upload-artifact` v4.6.2 for the Tier E2E report

## Action allowlist

The repository only runs allowlisted actions (`allowed_actions: selected`):
GitHub-owned actions (`actions/*`, `github/*`), the repository's own
reusable workflows, and these explicit third-party patterns:

- `maxim-lobanov/setup-xcode@*`
- `actions-rust-lang/setup-rust-toolchain@*`
- `Swatinem/rust-cache@*` — not referenced by any workflow directly;
  `setup-rust-toolchain` is a composite action whose `action.yml` uses
  it, and the policy is evaluated for every nested reference at
  *Set up job*, even though our call sites pass `cache: false` and the
  nested step never runs.

Marketplace "verified creators" are **not** allowed as a class; every
third-party action is listed by name.

**Add the allow rule before the workflow references the action.** An
action that is not on the list is refused by GitHub before the step
starts, with an error like *`owner/repo@sha` is not allowed because all
actions must be from a repository owned by neoxelox, a GitHub-owned
action, or match a pattern*. The workflow change cannot go green until
the setting has changed, so the order is: read the action's
`action.yml` for nested `uses:` lines, allow every pattern, SHA-pin the
`uses:` line, update the list above, then push. Removing an action
from the workflows should remove its pattern (and its nested ones)
too. Upgrading an action can change its nested references, so check
the diff of its `action.yml` on every bump.

Inspect and change the list:

```bash
gh api repos/neoxelox/vapor/actions/permissions/selected-actions
gh api -X PUT repos/neoxelox/vapor/actions/permissions/selected-actions --input - <<'EOF'
{"github_owned_allowed": true,
 "verified_allowed": false,
 "patterns_allowed": ["maxim-lobanov/setup-xcode@*",
                      "actions-rust-lang/setup-rust-toolchain@*",
                      "Swatinem/rust-cache@*"]}
EOF
```

The `PUT` replaces the whole list, so send every pattern each time.

## Dependency source defaults

- Rust crates: `crates.io` via Cargo
- Swift packages: SwiftPM package dependencies

`lint.yml`, `test.yml`, and `build.yml` run on pull requests and pushes to `main`, and also expose `workflow_call` so release automation can reuse the same gates. All three workflows fan out via a `strategy.matrix` over `macos-latest`, `ubuntu-latest`, and `windows-latest`; `fail-fast` is disabled so a Linux-only or Windows-only regression is visible even when macOS stays green. The macOS job runs both Rust and Swift; the Linux / Windows jobs run only the Rust workspace (the Swift wrappers detect a non-Darwin `uname -s` and exit cleanly).

Pull requests from forks do not start these workflows until a maintainer approves the run (repository setting *Approval for running fork pull request workflows from contributors* = **all external contributors**). Fork runs already get a read-only token and no secrets, but they still execute the PR's code on the runner, including `./scripts/e2e.sh --full`, which installs a real LaunchAgent; the approval step keeps that a deliberate act. Inspect or change it with `gh api repos/neoxelox/vapor/actions/permissions/fork-pr-contributor-approval`.

`perf.yml` has no standalone triggers; `release.yml` calls it for versioned release runs.

`release.yml` runs on pushed tags matching `v*`, validates that the tag exactly matches `VERSION` and that the tagged commit is on `main`, invokes `lint.yml`, `test.yml`, and `perf.yml` in parallel, and then runs the `release` job only after all three succeed. The publish job targets the GitHub `release-macos` environment, and packaging still uses `./scripts/build.sh package` as the source of truth.

The `main` ruleset (required checks, bypass policy, how to inspect and recreate it) is documented in `docs/ci/required-checks.md`.
