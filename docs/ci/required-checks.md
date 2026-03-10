# CI required checks guidance

This project uses repository script entry points as the source of truth for local and CI validation.

See `docs/ci/overview.md` for workflow scope, triggers, and pinned action versions.

## Required GitHub status checks

Configure branch protection for `main` to require the following workflow checks:

- `lint`
- `test`

Release-only gates:

- `lint`, `test`, and `perf` are reusable workflows invoked by `release.yml` for versioned releases.
- `perf` is not a pull-request required status check.

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
  - triggers: `pull_request`, `push` to `main`, `workflow_call`
  - `./scripts/lint.sh`
- Test workflow (`.github/workflows/test.yml`)
  - triggers: `pull_request`, `push` to `main`, `workflow_call`
  - `./scripts/test.sh`
- Perf workflow (`.github/workflows/perf.yml`)
  - trigger: `workflow_call` from `.github/workflows/release.yml`
  - thresholds via env: `VAPOR_PERF_SMOKE_RUST_MAX_SECONDS` (default `600`), `VAPOR_PERF_SMOKE_SWIFT_MAX_SECONDS` (default `900`)
  - `./scripts/perf.sh`
- Release workflow (`.github/workflows/release.yml`)
  - trigger: pushed `v*` tags
  - validates tag format, `VERSION` match, and tag ancestry on `main`
  - calls `lint`, `test`, and `perf` in parallel
  - `release` job declares `needs: [preflight, lint, test, perf]`
  - `./scripts/build.sh package`
  - `gh release create/edit/upload`

## Local parity command set

Run the same validations locally before opening a PR:

- `./scripts/lint.sh`
- `./scripts/test.sh`

For performance-sensitive changes, run:

- `./scripts/perf.sh`

`./scripts/lint.sh` includes format checks (`./scripts/format.sh check`).

Release workflow is tag-driven and should not be configured as a required status check for pull requests on `main`.
