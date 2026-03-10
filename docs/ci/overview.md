# CI Overview

GitHub Actions workflows are defined in `.github/workflows/`:

- `lint.yml`: runs lint and format checks via repository scripts.
- `test.yml`: runs test suites via repository scripts.
- `perf.yml`: reusable performance gate workflow invoked by the release pipeline.
- `release.yml`: runs tag-driven package and GitHub Release publication flow.

## Toolchain defaults in CI

- Runner: `macos-latest`
- Xcode and Swift: `latest-stable`
- Rust: `stable`

## Pinned CI action versions

- `actions/checkout@v6`
- `maxim-lobanov/setup-xcode@v1.6.0`
- `actions-rust-lang/setup-rust-toolchain@v1.9.0`
- `actions/cache@v5` for Rust (`cargo`) and SwiftPM caches

## Dependency source defaults

- Rust crates: `crates.io` via Cargo
- Swift packages: SwiftPM package dependencies

`lint.yml` and `test.yml` run on pull requests and pushes to `main`, and also expose `workflow_call` so release automation can reuse the same gates.

`perf.yml` has no standalone triggers; `release.yml` calls it for versioned release runs.

`release.yml` runs on pushed tags matching `v*`, validates that the tag exactly matches `VERSION` and that the tagged commit is on `main`, invokes `lint.yml`, `test.yml`, and `perf.yml` in parallel, and then runs the `release` job only after all three succeed. The publish job targets the GitHub `release` environment, and packaging still uses `./scripts/build.sh package` as the source of truth.

For branch protection and required status-check guidance, see `docs/ci/required-checks.md`.
