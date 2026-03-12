# Vapor 💨

**`background cloud sync that won't melt your device 🔥`** - [**`vapor.arn.sh`**](https://vapor.arn.sh)

## What

Vapor is an invisible-first cloud sync app that stays out of your way. It keeps a local and a cloud folder in full bidirectional sync with best-effort real-time updates and durable eventual consistency. It works automatically in the background, from startup to shutdown, syncs opportunistically, and is tuned for speed without draining your device.

## Install

Download Vapor directly from the [GitHub Releases](https://github.com/neoxelox/vapor/releases) page and install the `Vapor` app for your platform/distribution from the latest release assets.

## Features

Available now:

- ⚡ Fast-feeling background sync designed to stay responsive without stealing your machine.
- 🪶 Low-impact by design: Vapor defers heavy work under pressure to protect battery and thermals.
- 🌩 Sudden bursts of file changes stay contained, so one big folder update doesn't snowball.
- 🍎 Menubar-first experience that stays out of your way while keeping status and controls one click away.
- 🚀 Auto-launch at login with resilient crash-loop protection for dependable day-to-day use.
- 🧹 Fine-grained ignore rules keep low-signal files out of your sync flow.

In flight and coming next:

- 🔁 Bidirectional cloud sync with durable intent replay and eventual consistency.
- 🧩 Multiple sync profiles let one folder flow to several clouds or keep separate setups neatly isolated.
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

| Key                  | Type     | Default                                             | Description                                                                              |
| -------------------- | -------- | --------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| `autoLaunch`         | `Bool`   | `true`                                              | Starts Vapor automatically at login and keeps the daemon bootstrapped in the background. |
| `useGitIgnore`       | `Bool`   | `true`                                              | Applies recursive `.gitignore` rules during local filtering.                             |
| `useVaporIgnore`     | `Bool`   | `true`                                              | Applies recursive `.vaporignore` rules during local filtering.                           |
| `localSyncDirectory` | `String` | `"~/Vapor"`                                         | Sets the local sync root; Vapor creates it if it does not exist yet.                     |
| `cloudSyncDirectory` | `String` | `"/Vapor"`                                          | Sets the cloud sync root; Vapor creates it if it does not exist yet.                     |
| `preIgnoreRules`     | `String` | Embedded `.gitignore`-like low-impact default rules | Provides the baseline ignore rules that run before discovered ignore files.              |
| `postIgnoreRules`    | `String` | Empty string                                        | Provides the final override rules that run after discovered ignore files.                |
| `languageCode`       | `String` | `"en"`                                              | Selects the UI language catalog to load.                                                 |
| `timelineEventLimit` | `Int`    | `1000`                                              | Caps the in-memory timeline length shown in diagnostics.                                 |

### Ignore rules

- Filesystem callback filtering applies before event metadata is recorded.
- Rule precedence (lowest to highest): `preIgnoreRules` -> `.gitignore` (when enabled, recursive per-directory) -> `.vaporignore` (when enabled, recursive per-directory) -> `postIgnoreRules`.
- `.vaporignore` supports glob-like rules and `!` unignore rules.
- `preIgnoreRules` default content covers common low-signal paths such as `.git/`, `node_modules/`, build outputs (`dist/`, `build/`, `out/`), caches, swap/tmp files, logs, and `.env.local`.

## Development

This project is intentionally vibe-coded while still following strict reliability, safety, and low-impact engineering rules.

Structure:

- `apps/macos`: SwiftUI app (`Vapor`) and shared app code.
- `core/daemon`: Rust daemon runtime (`vapord`).
- `core/providers`: Rust cloud provider integrations.
- `core/shared`: shared contracts/constants used across app and daemon boundaries.

See `.env.example` for the available `VAPOR_*` environment variables used by the app, scripts, CI, and packaging flow. The release version source of truth lives in `VERSION`.

### Scripts

- Build both stacks (release): `./scripts/build.sh`
- Build + package macOS app bundle: `./scripts/build.sh package`
- Clean build/dist artifacts: `./scripts/clean.sh`
- Lint both stacks: `./scripts/lint.sh`
- Format both stacks: `./scripts/format.sh`
- Format check both stacks (included in lint): `./scripts/format.sh check`
- Test both stacks: `./scripts/test.sh`
- Version helper: `./scripts/version.sh`
- Performance smoke thresholds: `./scripts/perf.sh` (`VAPOR_PERF_SMOKE_RUST_MAX_SECONDS`, `VAPOR_PERF_SMOKE_SWIFT_MAX_SECONDS`)

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

### Releases

Version bumps with `./scripts/version.sh`:

1. Commit all changes and checkout to `main` with a clean worktree.
2. Run `./scripts/format.sh`, `./scripts/lint.sh`, and `./scripts/test.sh`, then commit any fixes they produce.
3. Update `CHANGELOG.md` with a section that matches the target version and release date.
4. Run the appropriate `./scripts/version.sh ...` command to update `VERSION`, sync Cargo metadata, create the release commit, and create the matching tag.
5. Push the release commit and tag together: `git push origin "$(git branch --show-current)" --follow-tags`.
6. Wait for `lint`, `test`, `perf`, and `release` to pass on the tag.
7. Review the draft GitHub Release, verify `Vapor.zip` and `Checksums.txt`, then publish it.

`./scripts/version.sh` usage:

- Show current version: `./scripts/version.sh current`
- Set an exact stable version: `./scripts/version.sh set 0.2.0`
- Bump the stable base version: `./scripts/version.sh bump patch|minor|major`
- Set an exact prerelease: `./scripts/version.sh set 0.2.0-rc.1`
- Bump the current prerelease: `./scripts/version.sh prerelease rc|beta|alpha`
- Convert to stable release: `./scripts/version.sh release`

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
