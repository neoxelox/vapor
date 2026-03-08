# vapor

background cloud sync that won't melt your device 🔥

`vapor` is an invisible-first macOS sync application with a SwiftUI app and a Rust daemon.
It is designed to auto-launch at login, sync opportunistically, and preserve laptop performance
over strict real-time behavior.

## Project Status

- Current stage: planning and repository foundation.
- Product direction: bidirectional eventual consistency for Google Drive in MVP.
- Primary constraint: do no harm to user workload, battery, and thermal headroom.
- Pre-GA compatibility policy: backward compatibility is not guaranteed yet; config/state/schema and local interfaces may change during active development.

## Product Goals

- Keep a selected local folder (default `~/Drive/`) synced to cloud with durable intent state.
- Stay low-impact during active development and heavy system load.
- Defer expensive work under pressure while maintaining eventual consistency.
- Provide transparent state, diagnostics, and user controls from the macOS app/menubar.

## Core Architecture

- SwiftUI app
  - Onboarding, provider auth, root selection, settings, diagnostics, menubar state.
  - Auto-launch toggle and daemon control surface.
- Rust daemon (`core/daemon`, LaunchAgent)
  - FSEvents ingestion, debounce/coalescing, scheduler, throttle controller.
  - Durable queue/state, retries, deferred reconcile, provider execution.
- Provider modules (`core/providers`)
  - `provider_gdrive` first, `provider_s3`/R2 later via shared provider trait.
- XPC boundary
  - Typed status/control API between app and daemon with shared contracts in `core/shared`.

## macOS App Components and Lifecycle

- Main app window (`Window` single-instance scene)
  - Primary configuration and diagnostics UI.
  - Dock-visible while the window is open.
- Menubar component (`MenuBarExtra`)
  - Always-on quick status and control surface while app process is running.
  - Owns user-facing lifecycle actions (`Open Vapor`, `Quit Vapor`).
- Background daemon (`vapord` LaunchAgent)
  - Independent runtime for sync execution and durability.
  - Must keep running when only the UI window is closed.
  - Must be shipped inside the same `Vapor.app` bundle at `Contents/MacOS/vapord`.

Expected lifecycle behavior:

- Auto-launch at login starts `vapord` and keeps Vapor as a menubar surface without opening the main window.
- Closing the main window closes the UI and removes Dock presence.
- Closing the main window does not stop `vapord` and does not remove menubar status/control.
- Reopening from menubar focuses the existing main window when present, or restores it when closed.
- Quitting from menubar performs full shutdown semantics (stop daemon, then terminate app process).

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
- Distribution foundation plan: `docs/plans/vapor-macos-distribution-foundation-plan.md`
- Execution task list: `docs/plans/vapor-macos-task-list.md`

## Architecture and Operations Docs

- Architecture index: `docs/architecture/README.md`
- Operations index: `docs/operations/README.md`
- Performance index: `docs/performance/README.md`

## Development and Contribution

- Contributor operating rules: `AGENTS.md`
- License: `LICENSE`

Current code bootstrap:

- Rust workspace: root `Cargo.toml` with crates in `core/daemon`, `core/providers`, `core/shared`
- Daemon binary: `vapord`
- Swift package: `apps/macos/Package.swift` (`Vapor`, `VaporCore`)

Implementation work follows the phase checklist in `docs/plans/vapor-macos-task-list.md`,
starting with repository/documentation hardening before core sync engine code.

## Developer Runbook (local)

Repository-level script entry points (used by both local development and CI):

- Build both stacks (release): `./scripts/build.sh`
- Build + package macOS app bundle: `./scripts/build.sh package`
- Clean build/dist artifacts: `./scripts/clean.sh`
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
- macOS app packaging: `apps/macos/scripts/package.sh`

Notes:

- Scripts intentionally skip missing stack artifacts during early bootstrap (for example,
  no `Cargo.toml` yet or no `apps/macos` project yet).
- Scripts default `VAPOR_DIR` to repo-local `./.vapor` for local dev/test ergonomics.
- Scripts default `VAPOR_ENV` to `dev` (and `prod` for `./scripts/build.sh package`).
- Override runtime root with `VAPOR_DIR=/path/to/vapor ./scripts/test.sh` (same for build/lint/format).
- `Vapor.app` is a single package that ships both binaries: `Contents/MacOS/Vapor` and `Contents/MacOS/vapord`.
- Runtime daemon launch path is always the bundled sibling binary (`vapord`) next to the app executable.
- Swift lint/format scripts intentionally use `swift format` only.
- If an Xcode project exists, set `VAPOR_XCODE_SCHEME` to enable `xcodebuild build`
  in `./scripts/swift/build.sh`.

Release build policy:

- A single release mode is used and tuned for performance with safe optimizations.
- Rust release profile uses `opt-level=3`, `lto=fat`, `codegen-units=1`, `panic=abort`, and `strip=symbols`.
- Swift release build uses whole-module and cross-module optimization flags.

## Logging

- Vapor runtime artifacts are rooted at a single directory controlled by `VAPOR_DIR`.
- Default runtime directory:
  - app/daemon runtime: `~/.vapor`
  - local dev + tests/CI via repo scripts: `./.vapor`
- Runtime layout:
  - config: `<vapor_dir>/vapor.json`
  - logs: `<vapor_dir>/logs/vapor.logs`, `<vapor_dir>/logs/vapord.logs`
  - state/db reserved path: `<vapor_dir>/state/vapor.sqlite`
- Runtime log level override: `VAPOR_LOG_LEVEL` (`debug`, `info`, `warning`, `error`).
- Log line format: `{timestamp} [{level}] ({component}): {message}. key=value ...`
- Default log level behavior:
  - `VAPOR_ENV=dev` -> `debug`
  - `VAPOR_ENV=prod` (or unset) -> `info`

## User Configuration Reference

All user-facing configuration must be documented here with meaning and defaults.

`vapor.json` (`<vapor_dir>/vapor.json`):

- `autoLaunchEnabled` (`Bool`)
  - Default: `true`
  - Purpose: controls whether Vapor auto-launches and bootstraps `vapord` at startup/login.
- `useGitIgnore` (`Bool`)
  - Default: `true`
  - Purpose: persisted preference for `.gitignore`-aware daemon local filtering.
- `useVaporIgnore` (`Bool`)
  - Default: `true`
  - Purpose: persisted preference for `.vaporignore`-aware daemon local filtering.
- `syncDirectories` (`[String]`)
  - Default: `["~/Vapor"]`
  - Purpose: user-level list of local directories to sync. Missing or non-directory paths are skipped at daemon startup.
- `preIgnoreRules` (`String`)
  - Default: embedded `.gitignore`-like text with a curated low-impact ignore set.
  - Purpose: user-level baseline rules appended first, before discovered `.gitignore`/`.vaporignore` files.
- `postIgnoreRules` (`String`)
  - Default: empty string.
  - Purpose: user-level override rules appended last, after discovered ignore files.
- `timelineEventLimit` (`Int`)
  - Default: `1000`
  - Purpose: persisted cap for timeline/diagnostic event surfaces.

All persisted user configuration lives in `vapor.json`.

## Local Event Filtering

- Filesystem callback filtering applies before event metadata is recorded.
- Rule precedence (lowest to highest): `preIgnoreRules` -> `.gitignore` (when enabled, recursive per-directory) -> `.vaporignore` (when enabled, recursive per-directory) -> `postIgnoreRules`.
- `.vaporignore` supports glob-like rules and `!` unignore rules.
- `preIgnoreRules` default content covers common low-signal paths such as `.git/`, `node_modules/`, build outputs (`dist/`, `build/`, `out/`), caches, swap/tmp files, logs, and `.env.local`.

Runtime directory is not a `vapor.json` option and is resolved by precedence:

1. `VAPOR_DIR` environment override
2. `./.vapor` in tests/CI or when `VAPOR_ENV=dev`
3. `~/.vapor` in normal runtime

## `VAPOR_*` Environment Variable Reference

Runtime and scripts:

| Variable | Default | Purpose |
| --- | --- | --- |
| `VAPOR_DIR` | `~/.vapor` for normal runtime; repo scripts set `./.vapor` | Runtime root for `vapor.json`, logs, and durable state. |
| `VAPOR_ENV` | Unset (treated as `prod`); repo scripts default to `dev`; package flow defaults to `prod` | Runtime mode (`dev` or `prod`) controlling path fallback and default log level. |
| `VAPOR_LOG_LEVEL` | Unset (falls back to `VAPOR_ENV`) | Runtime minimum log level (`debug`, `info`, `warning`, `error`). |
| `VAPOR_USE_GITIGNORE` | `true` | Daemon local filtering toggle for `.gitignore` ingestion. |
| `VAPOR_USE_VAPORIGNORE` | `true` | Daemon local filtering toggle for `.vaporignore` ingestion. |
| `VAPOR_SYNC_DIRECTORIES` | Newline-joined value from `vapor.json.syncDirectories` | Daemon local sync directory list source. |
| `VAPOR_PRE_IGNORE_RULES` | Raw value from `vapor.json.preIgnoreRules` | Daemon user-level baseline rules source (embedded `.gitignore`-like text). |
| `VAPOR_POST_IGNORE_RULES` | Raw value from `vapor.json.postIgnoreRules` | Daemon user-level override rules source (embedded `.gitignore`-like text). |

Build/packaging:

| Variable | Default | Purpose |
| --- | --- | --- |
| `VAPOR_XCODE_SCHEME` | Unset | Required to run `xcodebuild` in `./scripts/swift/build.sh` when building from an Xcode project/workspace. |
| `VAPOR_SIGN_IDENTITY` | Empty (ad-hoc signing) | Developer ID identity used by `apps/macos/scripts/package.sh`. |
| `VAPOR_ENTITLEMENTS` | Empty | Optional entitlements plist path passed to codesign in packaging. |
| `VAPOR_NOTARY_PROFILE` | Empty | Notarytool keychain profile; when set, packaging performs notarization and stapling. |
| `VAPOR_BUILD_NUMBER` | `git rev-list --count HEAD` fallback to `1` | Overrides `CFBundleVersion` in packaged app artifacts. |

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
