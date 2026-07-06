# Vapor 💨

**`background cloud sync that won't melt your device 🔥`** - [**`vapor.arn.sh`**](https://vapor.arn.sh)

## What

Vapor is an invisible-first cloud sync app that stays out of your way. It keeps a local and a cloud folder in full bidirectional sync with best-effort real-time updates and durable eventual consistency. It works automatically in the background, from startup to shutdown, syncs opportunistically, and is tuned for speed without draining your device.

## Install

Download Vapor directly from the [GitHub Releases](https://github.com/neoxelox/vapor/releases) page.

- **macOS**: install the `Vapor` app bundle (`Vapor.zip`) from the latest release assets.
- **Windows / Linux**: in flight. The CLI (`vapor`) will ship for Windows and Linux before the GUI apps do.
- **CLI (`vapor`)**: the command-line tool ships alongside every platform's installer under the same release tag.

## Features

Available now:

- 🔁 Bidirectional cloud sync with durable intent replay and eventual consistency.
- 🛡 Conflict-safe behavior with deterministic outcomes (keep both copies, never silent overwrite).
- 🔀 Choose each folder's sync direction — full two-way, or a one-way mirror for read-only backups and copies.
- 🧩 Multiple sync profiles let one folder flow to several clouds or keep separate setups neatly isolated.
- 🪶 Low-impact by design: Vapor defers heavy work under pressure to protect battery and thermals.
- ⏸️ Pressure-aware throttle modes that adapt sync intensity to real device load.
- ⚙️ Configurable hard caps on its share of CPU, memory, and network so streaming, browsing, and other apps always have room.
- 🌙 Smart idle boost: Vapor catches up faster when your device is genuinely idle, and yields the moment you come back.
- 🌩 Storm-aware scheduling keeps sudden bursts of file changes contained, so one big folder update doesn't snowball.
- 🛟 Mass-deletion guard pauses sync before a suspicious local wipe can replicate to the cloud.
- 📈 Clear diagnostics with status reasons, queue visibility, a live activity timeline, and a one-command support bundle.
- 🔕 Stays out of your way while keeping status and controls one click away.
- 🚀 Auto-launch at login with resilient crash-loop protection for dependable day-to-day use.
- 🧹 Fine-grained ignore rules keep low-signal files out of your sync flow.
- ⏯️ Pause and resume background work on demand; nothing is lost while paused, and Vapor picks up right where it left off.

In flight and coming next:

- ⚡ Fast-feeling background sync designed to stay responsive without stealing your machine.
- 🌍 Cross-platform parity: one portable runtime powers the macOS, Windows, and Linux apps and the CLI.

## Providers

Available now:

- [File System](https://github.com/neoxelox/vapor/tree/main/core/providers/src/filesystem) — any locally mounted folder (external drives, network mounts, another directory).
- [Google Drive](https://workspace.google.com/intl/es/products/drive) — OAuth sign-in via `vapor auth login google_drive`.

In flight and coming next:

- More cloud providers as demand surfaces (the provider system is pluggable; see `docs/architecture/provider-onboarding.md`).

## Benchmarks

> TBD

## Configuration

All user-facing configuration is documented here with meaning and defaults.

All persisted user configuration lives in `<vapor_dir>/vapor.json`; Vapor reads it at startup, so changes apply the next time it starts. Matching `VAPOR_*` environment variables, when set, take priority over the file.

| Key                  | Type     | Default                                             | Description                                                                              |
| -------------------- | -------- | --------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| `autoLaunch`         | `Bool`   | `true`                                              | Starts Vapor automatically at login and keeps the daemon bootstrapped in the background. |
| `useGitIgnore`       | `Bool`   | `true`                                              | Applies recursive `.gitignore` rules during local filtering.                             |
| `useVaporIgnore`     | `Bool`   | `true`                                              | Applies recursive `.vaporignore` rules during local filtering.                           |
| `localSyncDirectory` | `String` | `"~/Vapor"`                                         | Sets the local sync root; Vapor creates it if it does not exist yet.                     |
| `cloudSyncDirectory` | `String` | `"/Vapor"`                                          | Sets the cloud sync root; Vapor creates it if it does not exist yet.                     |
| `provider`           | `String` | `"filesystem"`                                      | Chooses the cloud backend: `filesystem` (a local folder acting as the cloud side) or `google_drive` (requires `vapor auth login google_drive`). |
| `syncMode`           | `String` | `"two-way"`                                         | Chooses the sync direction: `two-way` (bidirectional), `pull-only` (cloud → local, a read-only local mirror), or `push-only` (local → cloud, a read-only cloud backup). |
| `preIgnoreRules`     | `String` | Embedded `.gitignore`-like low-impact default rules | Provides the baseline ignore rules that run before discovered ignore files.              |
| `postIgnoreRules`    | `String` | Empty string                                        | Provides the final override rules that run after discovered ignore files.                |
| `languageCode`       | `String` | `"en"`                                              | Selects the UI language catalog to load.                                                 |
| `timelineEventLimit` | `Int`    | `1000`                                              | Caps the in-memory timeline length shown in diagnostics.                                 |
| `deviceId`           | `String` | Derived from the hostname on first run              | Stable per-device identifier used in conflict-copy names (for example `report~conflict-mac-studio-....pdf`). Written by the daemon; never regenerated silently. |
| `profiles`           | `Array`  | Absent (single implicit profile)                    | Optional named sync profiles. Each entry (`id`, `name`, `enabled`, plus optional `provider`, `localSyncDirectory`, `cloudSyncDirectory`, `syncMode`, `resourceLimits`, `idleBoost` overrides) runs as an isolated pipeline with its own durable state; unset fields inherit the top-level values. |
| `resourceLimits`     | `Object` | `{ cpuPercent: 15, memoryPercent: 10, bandwidthPercent: 25 }` | Sets hard ceilings on daemon CPU (share of one core), device memory, and measured bandwidth. Honored by the throttle controller and auto-tuner. Profile overrides may only lower these values. |
| `idleBoost`          | `Object` | See below                                           | Dynamically raises effective ceilings when the device is user-idle with measured resource headroom, ramping up slowly and down quickly. Setting `enabled: false` in any enabled profile disables boost daemon-wide. |

`idleBoost` defaults: `enabled: true`, `minIdleSeconds: 300`, `headroomCpuPercent: 30`, `boostCpuPercent: 50`, `boostMemoryPercent: 20`, `boostBandwidthPercent: 80`, `rampUpSeconds: 30`, `rampDownSeconds: 10`. Each `boost*Percent` must be `>=` the matching `resourceLimits.*Percent` (lower values are treated as equal to the base ceiling). Boost requires all of: throttle state `IdleDrain`, user-idle for at least `minIdleSeconds`, and non-Vapor CPU utilization at or below `headroomCpuPercent`. Ramp-down is deliberately faster than ramp-up so returning to your device is never met with a busy daemon.

Google Drive credentials are supplied per deployment through the `VAPOR_GDRIVE_CLIENT_ID` / `VAPOR_GDRIVE_CLIENT_SECRET` environment variables (see `.env.example` and `docs/operations/provider-auth-operations.md`); user tokens are stored only in the platform secret store, never in `vapor.json`.

### Ignore rules

- Filesystem callback filtering applies before event metadata is recorded.
- Ignore rules are symmetric: a name that matches them never syncs in either direction — it is skipped by local ingest, by the remote changes feed, and by reconcile comparison on both sides — so an ignored file (for example `.DS_Store`) can never be pulled down from the cloud or produce a conflict copy.
- Rule precedence (lowest to highest): `preIgnoreRules` -> `.gitignore` (when enabled, recursive per-directory) -> `.vaporignore` (when enabled, recursive per-directory) -> `postIgnoreRules`.
- `.vaporignore` supports glob-like rules and `!` unignore rules.
- `preIgnoreRules` default content covers common low-signal paths such as `.git/`, `node_modules/`, build outputs (`dist/`, `build/`, `out/`), caches, swap/tmp files, logs, and `.env.local`.

## Development

This project is intentionally vibe-coded while still following strict reliability, safety, and low-impact engineering rules.

Structure:

- `core/daemon`: Rust daemon runtime (`vapord`) — the portable sync engine.
- `core/providers`: Rust cloud provider integrations.
- `core/shared`: shared contracts/constants/config loader used across the workspace.
- `core/ipc`: framed JSON IPC channel between the daemon and every surface.
- `core/platform`: traits + per-OS native implementations for fs-watch, service install, secrets, metrics sampling, idle detection, filesystem capabilities, and process supervision.
- `core/lifecycle`: daemon lifecycle manager and crash-loop guard consumed by every app surface.
- `core/cli`: the `vapor` CLI — headless-first control plane usable on every supported OS.
- `apps/macos`: SwiftUI macOS app (`Vapor`).
- `apps/windows` (planned): Windows app surface consuming `core/*`.
- `apps/linux` (planned): Linux app surface consuming `core/*`.

The Rust core (`core/*`) is the single portable runtime. Every app surface is a thin UI + OS-integration shim over it.

See `.env.example` for the available `VAPOR_*` environment variables used by apps, scripts, CI, and packaging flow. The release version source of truth lives in `VERSION`.

### Scripts

- Build both stacks (release): `./scripts/build.sh`
- Build + package macOS app bundle: `./scripts/build.sh package`
- Clean build/dist artifacts: `./scripts/clean.sh`
- Lint both stacks: `./scripts/lint.sh`
- Format both stacks: `./scripts/format.sh`
- Format check both stacks (included in lint): `./scripts/format.sh check`
- Test both stacks: `./scripts/test.sh`
- End-to-end verification in a disposable repo-local sandbox: `./scripts/e2e.sh` (manual sandbox: `--sandbox`; see `docs/development/e2e-verification.md`)
- Sync locale catalogs into every app surface: `./scripts/locales.sh`
- Install git pre-commit hook (clean → lint → test → build): `./scripts/hooks.sh`
- Remove the installed pre-commit hook: `./scripts/hooks.sh uninstall`
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
- Swift locale sync: `./scripts/swift/locales.sh`
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

Use `docs/README.md` as the entrypoint index for agent work. Every
documentation group has a `README.md` that is the starting point for
that group — always enter at the group README, not at an individual
file.

- Product direction, status, goals: `docs/product/README.md`
- Architecture, system boundaries, platform abstractions, IPC contracts: `docs/architecture/README.md`
- Operations, release process, runtime logging, provider auth, incident playbooks: `docs/operations/README.md`
- Local developer runbook and toolchain baseline: `docs/development/README.md`
- CI workflows and required-check policy: `docs/ci/README.md`
- Performance SLOs and benchmark harness: `docs/performance/README.md`
- Per-surface implementation plans (core, macos, cli): `docs/plans/README.md`
- Per-surface task lists and the cross-surface roadmap ("what should be done next"): `docs/tasks/README.md`
- Contributor operating rules: `AGENTS.md`

## Contribute

Feel free to contribute to this project : ) .

## License

This project is licensed under the [GPL-3.0 License](https://opensource.org/license/gpl-3-0). Read the [LICENSE](LICENSE) file for details.
