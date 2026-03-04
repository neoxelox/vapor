# vapor

background cloud sync that won't melt your device 🔥

`vapor` is an invisible-first macOS sync application with a SwiftUI app and a Rust daemon.
It is designed to auto-launch at login, sync opportunistically, and preserve laptop performance
over strict real-time behavior.

## Project Status

- Current stage: planning and repository foundation.
- Product direction: bidirectional eventual consistency for Google Drive in MVP.
- Primary constraint: do no harm to user workload, battery, and thermal headroom.

## Product Goals

- Keep a selected local folder (default `~/Drive/`) synced to cloud with durable intent state.
- Stay low-impact during active development and heavy system load.
- Defer expensive work under pressure while maintaining eventual consistency.
- Provide transparent state, diagnostics, and user controls from the macOS app/menubar.

## Core Architecture

- SwiftUI app
  - Onboarding, provider auth, root selection, settings, diagnostics, menubar state.
  - Auto-launch toggle and daemon control surface.
- Rust daemon (LaunchAgent)
  - FSEvents ingestion, debounce/coalescing, scheduler, throttle controller.
  - Durable queue/state, retries, deferred reconcile, provider execution.
- Provider modules
  - `provider_gdrive` first, `provider_s3`/R2 later via shared provider trait.
- XPC boundary
  - Typed status/control API between app and daemon.

## Runtime Model

- Auto-launch at login is ON by default.
- Throttle states govern all heavy work:
  - `IdleDrain`, `Light`, `Throttled`, `Suspended`.
- Eventual consistency is guaranteed by durable intent persistence and retry logic.
- Bidirectional safety includes loop prevention and deterministic conflict handling.

## Planning Docs

- Index: `docs/plans/README.md`
- Source plan (verbatim): `docs/plans/vapor-original-plan-verbatim.md`
- Derived macOS plan: `docs/plans/vapor-macos-plan.md`
- Execution task list: `docs/plans/vapor-macos-task-list.md`

## Architecture and Operations Docs

- Architecture index: `docs/architecture/README.md`
- Operations index: `docs/operations/README.md`
- Performance index: `docs/performance/README.md`

## Development and Contribution

- Contributor operating rules: `AGENTS.md`
- License: `LICENSE`

Current code bootstrap:

- Rust workspace: root `Cargo.toml` with `daemon`, `providers`, `shared`
- Swift package: `apps/macos/Package.swift` (`VaporApp`, `VaporAppCore`)

Implementation work follows the phase checklist in `docs/plans/vapor-macos-task-list.md`,
starting with repository/documentation hardening before core sync engine code.

## Developer Runbook (local)

Repository-level script entry points (used by both local development and CI):

- Build both stacks (release): `./scripts/build.sh`
- Lint both stacks: `./scripts/lint.sh`
- Format check both stacks (included in lint): `./scripts/format.sh check`
- Format apply both stacks: `./scripts/format.sh apply`
- Test both stacks: `./scripts/test.sh`

Stack-specific helpers:

- Rust lint: `./scripts/rust/lint.sh`
- Rust format: `./scripts/rust/format.sh check`
- Rust tests: `./scripts/rust/test.sh`
- Rust build: `./scripts/rust/build.sh`
- Swift lint: `./scripts/swift/lint.sh`
- Swift format: `./scripts/swift/format.sh check`
- Swift tests: `./scripts/swift/test.sh`
- Swift build: `./scripts/swift/build.sh`

Notes:

- Scripts intentionally skip missing stack artifacts during early bootstrap (for example,
  no `Cargo.toml` yet or no `apps/macos` project yet).
- If an Xcode project exists, set `VAPOR_XCODE_SCHEME` to enable `xcodebuild test`
  in `./scripts/swift/test.sh`.
- If an Xcode project exists, set `VAPOR_XCODE_SCHEME` to enable `xcodebuild build`
  in `./scripts/swift/build.sh`.

Release build policy:

- A single release mode is used and tuned for performance with safe optimizations.
- Rust release profile uses `opt-level=3`, `lto=fat`, `codegen-units=1`, `panic=abort`, and `strip=symbols`.
- Swift release build uses whole-module and cross-module optimization flags.

## Known-good local baseline (Mar 2026)

- macOS: `26.3` (Tahoe)
- Rust: `rustc 1.93.1 (01f6ddf75 2026-02-11)`
- Cargo: `cargo 1.93.1 (083ac5135 2025-12-15)`
- Swift driver: `1.127.15`
- Swift: `6.2.4 (swiftlang-6.2.4.1.4 clang-1700.6.4.2)`
- Swift target: `arm64-apple-macosx26.0`

This baseline is a known-good reference for contributors, not a hard pin. Project policy remains
"latest stable by default" unless a documented blocker requires temporary pinning.

## CI

GitHub Actions workflows are defined in `.github/workflows/`:

- `lint.yml`: runs lint and format-check via repository scripts.
- `test.yml`: runs test suites via repository scripts.

Toolchain defaults in CI:

- Runner: `macos-latest`
- Xcode/Swift: `latest-stable`
- Rust: `stable`

Pinned CI action versions:

- `actions/checkout@v6`
- `maxim-lobanov/setup-xcode@v1.6.0`
- `actions-rust-lang/setup-rust-toolchain@v1.9.0`
- `actions/cache@v5` for Rust (`cargo`) and SwiftPM caches

Dependency source defaults:

- Rust crates: `crates.io` via Cargo
- Swift packages: SwiftPM (package dependencies)

Both workflows run on pull requests and pushes to `main`, and are intended to mirror local commands.

Branch protection / required checks guidance: `docs/ci/required-checks.md`.
