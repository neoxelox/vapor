# CI Overview

GitHub Actions workflows are defined in `.github/workflows/`:

- `lint.yml`: runs lint and format checks via repository scripts.
- `test.yml`: runs test suites via repository scripts.

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

Both workflows run on pull requests and pushes to `main`, and are intended to mirror local script commands.

For branch protection and required status-check guidance, see `docs/ci/required-checks.md`.
