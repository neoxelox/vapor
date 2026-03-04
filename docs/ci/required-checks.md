# CI required checks guidance

This project uses repository script entry points as the source of truth for local and CI validation.

## Required GitHub status checks

Configure branch protection for `main` to require the following workflow checks:

- `lint`
- `test`

## Workflow to script mapping

Execution environment defaults:

- GitHub runner: `macos-latest`
- Xcode/Swift toolchain: `latest-stable` via `setup-xcode`
- Rust toolchain: `stable`

Pinned CI actions:

- `actions/checkout@v6`
- `maxim-lobanov/setup-xcode@v1.6.0`
- `actions-rust-lang/setup-rust-toolchain@v1.9.0`
- `actions/cache@v5`

Dependency caches used in CI:

- Rust: `~/.cargo/bin`, `~/.cargo/registry/index`, `~/.cargo/registry/cache`, `~/.cargo/git/db`, `target`
- SwiftPM: `.build`

Dependency source defaults:

- Rust crates: `crates.io` via Cargo
- Swift packages: SwiftPM

- Lint workflow (`.github/workflows/lint.yml`)
  - `./scripts/lint.sh`
- Test workflow (`.github/workflows/test.yml`)
  - `./scripts/test.sh`

## Local parity command set

Run the same validations locally before opening a PR:

- `./scripts/lint.sh`
- `./scripts/test.sh`

`./scripts/lint.sh` includes format checks (`./scripts/format.sh check`).
