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

`lint.yml` and `test.yml` run on pull requests and pushes to `main`, and are intended to mirror local script commands.

`perf.yml` has no standalone triggers; `release.yml` calls it for versioned release runs.

`release.yml` runs on pushed tags matching `v*` (plus manual dispatch for reruns), invokes `perf.yml` / `./scripts/perf.sh` as a release gate, and then runs the `release` job only after `needs: perf` succeeds. Packaging still uses `./scripts/build.sh package` as the source of truth.

For branch protection and required status-check guidance, see `docs/ci/required-checks.md`.
