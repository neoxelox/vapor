# CI Overview

GitHub Actions workflows are defined in `.github/workflows/`:

- `lint.yml`: runs lint and format checks via repository scripts.
- `test.yml`: runs the Tier 1 suites via repository scripts, then the
  Tier E2E harness (`./scripts/e2e.sh`) on every OS job; the macOS job
  passes `--full` (the launchd round-trip on a disposable runner) and
  every job uploads the harness's JSON report as the
  `e2e-report-<os>` artifact. Scenarios whose needs the host cannot
  meet skip by name: the Linux job runs the daemon scenarios (not the
  launchd round-trip or the disk-image ones), the Windows job runs the
  harness and `S01` and picks up the daemon scenarios once its native
  traits ship.
- `build.yml`: runs distribution builds via repository scripts.
- `perf.yml`: Tier 2, a release gate and on demand, never a PR gate:
  one soak cell against the release profile with the SLO checks
  asserted on its report (`scripts/perf.sh`), on a runner matrix that
  holds `macos-latest` today. `release.yml` calls it through
  `workflow_call`; `workflow_dispatch` takes a duration and a seed for
  a run by hand, with the release numbers as defaults.
- `soak.yml`: Tier S, a release gate and on demand, never a PR gate: a
  matrix of soak cells (mode, load, faults, throttle) over
  `macos-latest` and `ubuntu-latest`, each uploading its report,
  status, op log, model, and daemon log as the `soak-<os>-<cell>`
  artifact. The disk-full cell is macOS-only (its image comes from
  `hdiutil`). `release.yml` calls it through `workflow_call`;
  `workflow_dispatch` takes a duration and a seed for a run by hand.
- `release.yml`: the tag-driven pipeline: preflight, the four gates,
  one `package (<platform>)` job per shipping platform under that
  platform's environment, then one `publish` job that assembles the
  GitHub Release.

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
- `actions/upload-artifact` v4.6.2 for the Tier E2E report, the soak
  and perf reports, and the release packages
- `actions/download-artifact` v8.0.1 for the release packages in the
  `publish` job

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

`perf.yml` and `soak.yml` have no schedule and no PR trigger: `release.yml` calls both through `workflow_call` for versioned release runs, and `workflow_dispatch` runs either by hand with a duration and a seed.

`release.yml` runs on pushed tags matching `v*`. Preflight validates that the tag exactly matches `VERSION`, that the tagged commit is on `main`, and that `CHANGELOG.md` has the tag's section; then `lint.yml`, `test.yml`, `perf.yml`, and `soak.yml` run in parallel. The soak matrix is the long leg: nine cells of 45 minutes each, side by side. After all four gates pass, `package (<platform>)` runs once per shipping platform, on that platform's runner and under its protected environment (`release-macos` in the matrix today; `release-windows` and `release-linux` exist with the same protection and join the matrix when those surfaces ship), and uploads its assets as the `release-<platform>` artifact; `./scripts/build.sh package` stays the source of truth. The `publish` job then downloads every platform's assets, writes one `Checksums.txt` over all of them, and creates or updates the draft GitHub Release. Preflight and `publish` run on `ubuntu-latest`: nothing in them is platform work.

The `main` ruleset (required checks, bypass policy, how to inspect and recreate it) is documented in `docs/ci/required-checks.md`.
