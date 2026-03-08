# Vapor 💨

**`background cloud sync that won't melt your device 🔥`** - [**`vapor.arn.sh`**](https://vapor.arn.sh)

## What

Vapor is an invisible-first cloud sync app that stays out of your way. It keeps a local and a cloud folder in full bidirectional sync with best-effort real-time updates and durable eventual consistency. It works automatically in the background, from startup to shutdown, syncs opportunistically, and is tuned for speed without draining your device.

## Install

> TBD

## Features

Available now:

- ⚡ Fast-feeling background sync designed to stay responsive without stealing your machine.
- 🪶 Low-impact by design: Vapor defers heavy work under pressure to protect battery and thermals.
- 🍎 Menubar-first experience that stays out of your way while keeping status and controls one click away.
- 🚀 Auto-launch at login with resilient crash-loop protection for dependable day-to-day use.
- 🧹 Fine-grained ignore rules keep low-signal files out of your sync flow.

In flight and coming next:

- 🔁 Bidirectional cloud sync with durable intent replay and eventual consistency.
- 🛡 Conflict-safe behavior with deterministic outcomes (keep both copies, never silent overwrite).
- ⏸️ Pressure-aware throttle modes that adapt sync intensity to real device load.
- 📈 Clear diagnostics with status reasons, queue visibility, and live activity timeline.
- 🌩 Storm-aware scheduling and resilient recovery keep big change bursts under control.

## Cloud Providers

- [Google Drive](https://workspace.google.com/intl/es/products/drive) (current MVP target)

## Benchmarks

> TBD

## Configuration

All user-facing configuration is documented here with meaning and defaults.

All persisted user configuration lives in `<vapor_dir>/vapor.json`.

| Key | Type | Default | Purpose |
| --- | --- | --- | --- |
| `autoLaunchEnabled` | `Bool` | `true` | Controls whether Vapor auto-launches and bootstraps `vapord` at startup/login. |
| `useGitIgnore` | `Bool` | `true` | Persisted preference for `.gitignore`-aware daemon local filtering. |
| `useVaporIgnore` | `Bool` | `true` | Persisted preference for `.vaporignore`-aware daemon local filtering. |
| `localSyncDirectory` | `String` | `"~/Vapor"` | User-level local root directory to replicate to cloud; missing roots are created at startup and non-directory paths are rejected. |
| `cloudSyncDirectory` | `String` | `"/Vapor"` | User-level provider cloud root directory to replicate with local sync; Vapor ensures this remote directory exists before sync operations. |
| `preIgnoreRules` | `String` | Embedded `.gitignore`-like low-impact default rules | User-level baseline rules appended first, before discovered `.gitignore` and `.vaporignore` files. |
| `postIgnoreRules` | `String` | Empty string | User-level override rules appended last, after discovered ignore files. |
| `preferredLanguageCode` | `String?` | `null` | Optional UI language override code (for example `en`); unsupported values fall back to English. |
| `timelineEventLimit` | `Int` | `1000` | Persisted cap for timeline and diagnostic event surfaces. |

### Ignore rules

- Filesystem callback filtering applies before event metadata is recorded.
- Rule precedence (lowest to highest): `preIgnoreRules` -> `.gitignore` (when enabled, recursive per-directory) -> `.vaporignore` (when enabled, recursive per-directory) -> `postIgnoreRules`.
- `.vaporignore` supports glob-like rules and `!` unignore rules.
- `preIgnoreRules` default content covers common low-signal paths such as `.git/`, `node_modules/`, build outputs (`dist/`, `build/`, `out/`), caches, swap/tmp files, logs, and `.env.local`.

### Environment Variables

The following environment variables can be used to override settings.

| Variable | Default | Purpose |
| --- | --- | --- |
| `VAPOR_DIR` | `~/.vapor` for normal runtime; repo scripts set `./.vapor` | Runtime root for `vapor.json`, logs, and durable state. |
| `VAPOR_ENV` | Unset (treated as `prod`); repo scripts default to `dev`; package flow defaults to `prod` | Runtime mode (`dev` or `prod`) controlling path fallback and default log level. |
| `VAPOR_LOG_LEVEL` | Unset (falls back to `VAPOR_ENV`) | Runtime minimum log level (`debug`, `info`, `warning`, `error`). |
| `VAPOR_USE_GITIGNORE` | `true` | Daemon local filtering toggle for `.gitignore` ingestion. |
| `VAPOR_USE_VAPORIGNORE` | `true` | Daemon local filtering toggle for `.vaporignore` ingestion. |
| `VAPOR_LOCAL_SYNC_DIRECTORY` | Raw value from `vapor.json.localSyncDirectory` | Daemon local sync root directory source. |
| `VAPOR_CLOUD_SYNC_DIRECTORY` | Raw value from `vapor.json.cloudSyncDirectory` | Daemon cloud sync root directory source. |
| `VAPOR_PRE_IGNORE_RULES` | Raw value from `vapor.json.preIgnoreRules` | Daemon user-level baseline rules source (embedded `.gitignore`-like text). |
| `VAPOR_POST_IGNORE_RULES` | Raw value from `vapor.json.postIgnoreRules` | Daemon user-level override rules source (embedded `.gitignore`-like text). |

Build and packaging:

| Variable | Default | Purpose |
| --- | --- | --- |
| `VAPOR_XCODE_SCHEME` | Unset | Required to run `xcodebuild` in `./scripts/swift/build.sh` when building from an Xcode project/workspace. |
| `VAPOR_SIGN_IDENTITY` | Empty (ad-hoc signing) | Developer ID identity used by `apps/macos/scripts/package.sh`. |
| `VAPOR_ENTITLEMENTS` | Empty | Optional entitlements plist path passed to codesign in packaging. |
| `VAPOR_NOTARY_PROFILE` | Empty | Notarytool keychain profile; when set, packaging performs notarization and stapling. |
| `VAPOR_BUILD_NUMBER` | `git rev-list --count HEAD` fallback to `1` | Overrides `CFBundleVersion` in packaged app artifacts. |

## Development

This project is intentionally vibe-coded while still following strict reliability, safety, and low-impact engineering rules.

Structure:

- `apps/macos`: SwiftUI app (`Vapor`) and shared app code.
- `core/daemon`: Rust daemon runtime (`vapord`).
- `core/providers`: Rust cloud provider integrations.
- `core/shared`: shared contracts/constants used across app and daemon boundaries.

### Scripts

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

## Agents

Use `docs/README.md` as the entrypoint index for agent work. Quick intent mapping:

- Product direction/status/goals: `docs/product/status-and-goals.md`
- Architecture/system boundaries: `docs/architecture/README.md`
- App/menubar/daemon lifecycle semantics: `docs/architecture/macos-app-lifecycle.md`
- Runtime/logging/localization policy: `docs/operations/runtime-logging-and-localization.md`
- CI behavior and required checks: `docs/ci/required-checks.md`
- Plans and execution sequence: `docs/plans/README.md`
- Performance budgets and harness: `docs/performance/README.md`
- Local developer runbook details: `docs/development/runbook.md`
- Contributor operating rules: `AGENTS.md`

## Contribute

Feel free to contribute to this project : ) .

## License

This project is licensed under the [GPL-3.0 License](https://opensource.org/license/gpl-3-0). Read the [LICENSE](LICENSE) file for details.
