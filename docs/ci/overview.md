# CI Overview

GitHub Actions workflows are defined in `.github/workflows/`:

- `lint.yml`: runs lint and format checks via repository scripts.
- `test.yml`: runs test suites via repository scripts.
- `build.yml`: runs distribution builds via repository scripts.
- `perf.yml`: reusable performance gate workflow invoked by the release pipeline.
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

## Dependency source defaults

- Rust crates: `crates.io` via Cargo
- Swift packages: SwiftPM package dependencies

`lint.yml`, `test.yml`, and `build.yml` run on pull requests and pushes to `main`, and also expose `workflow_call` so release automation can reuse the same gates. All three workflows fan out via a `strategy.matrix` over `macos-latest`, `ubuntu-latest`, and `windows-latest`; `fail-fast` is disabled so a Linux-only or Windows-only regression is visible even when macOS stays green. The macOS job runs both Rust and Swift; the Linux / Windows jobs run only the Rust workspace (the Swift wrappers detect a non-Darwin `uname -s` and exit cleanly).

`perf.yml` has no standalone triggers; `release.yml` calls it for versioned release runs.

`release.yml` runs on pushed tags matching `v*`, validates that the tag exactly matches `VERSION` and that the tagged commit is on `main`, invokes `lint.yml`, `test.yml`, and `perf.yml` in parallel, and then runs the `release` job only after all three succeed. The publish job targets the GitHub `release-macos` environment, and packaging still uses `./scripts/build.sh package` as the source of truth.

The `main` ruleset (required checks, bypass policy, how to inspect and recreate it) is documented in `docs/ci/required-checks.md`.
