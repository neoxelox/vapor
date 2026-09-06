# Vapor 💨

**`background cloud sync that won't melt your device 🔥`** - [**`vapor.arn.sh`**](https://vapor.arn.sh)

## What

Vapor is an invisible-first cloud sync app that stays out of your way. It keeps a local and a cloud folder in full bidirectional sync with best-effort real-time updates and durable eventual consistency. It works automatically in the background, from startup to shutdown, syncs opportunistically, and is tuned for speed without draining your device.

## Install

Download Vapor directly from the [GitHub Releases](https://github.com/neoxelox/vapor/releases) page.

- **macOS**: install the `Vapor` app bundle (`Vapor.zip`) from the latest release assets.
- **Windows / Linux**: planned. The CLI (`vapor`) would ship for Windows and Linux before the GUI apps do.
- **CLI (`vapor`)**: the command-line tool ships alongside every platform's installer under the same release tag.

## Features

Available now:

- 🔁 Two-way sync between a local folder and a cloud folder. Changes are picked up within seconds while you work.
- 🧠 Nothing is lost. Every change is written to disk before it moves, so a crash, a reboot, or a dropped connection resumes where it stopped.
- 🛡 Edits never silently overwrite each other. When two devices change the same file, Vapor keeps both copies.
- 🧭 Conflicts stay visible until you settle them, and one command resolves each one from any device.
- ❓ When the evidence is ambiguous, Vapor holds only the files in question, asks you a plain question with a short list of answers, and keeps the question until you answer it, from the terminal or the app.
- 🔂 Your own uploads never bounce back as new changes, so two devices cannot ping-pong a file forever.
- 🔀 Pick a direction per folder. Full two-way, or a one-way mirror for read-only backups and copies.
- 🧩 Run several sync profiles at once. One folder can flow to two clouds, or separate setups stay isolated.
- 📦 Downloads land whole or not at all, and big uploads are chunked so a pause does not restart them.
- ⏳ Retries back off on their own and respect provider rate limits, so a bad hour of connectivity fixes itself.
- 🪶 Heavy work waits while your device is busy, protecting battery and thermals.
- ⚙️ Hard caps on the share of CPU, memory, and network Vapor may use, so streaming and browsing always have room.
- 🌙 When the device sits idle, Vapor speeds up. It backs off the moment you return.
- 🌩 A burst of thousands of file changes is absorbed instead of turned into thousands of uploads.
- 🛟 A sudden mass deletion, from this device or from the cloud, is held and put to you as a question before it can reach the other side; everything else keeps syncing.
- 🧹 Ignore rules, including your existing gitignore files, keep build output and junk out of the sync.
- ♻️ Settings apply while it runs. Ceilings, ignore rules, and safeguards change without a restart, and Vapor tells you when one is needed.
- 🚀 Starts at login, restarts itself after a crash, and stops retrying when something is really broken instead of looping.
- ⏯️ Pause and resume on demand. Changes made while paused sync when you resume.
- 📈 Status with a reason, queue depth, a live activity timeline, per-file "why is this stuck", and a one-command support bundle.
- ⌨️ A full command line for scripts and servers. Everything the app does, the terminal does too.
- 🔐 Sign-in tokens live in the system keychain, logs never contain secrets, and nothing leaves your device except the files you chose to sync.
- 🔕 Lives in the menu bar. No windows unless you ask for one.

In flight and coming next:

- 🪟 A diagnostics window in the Mac app with throttle reason, queue, conflicts, and timeline, plus live pause and flush controls.
- 🔔 A notification when a conflict needs you.
- ✂️ Renames and moves without re-uploading the file.
- 🖥️ Windows and Linux apps on the same runtime as the Mac app.
- 📥 Standalone command-line downloads for every OS, with Docker and systemd recipes.
- 🧾 Signed and notarized releases, verified on a clean machine every cycle.

## Providers

Available now:

- [File System](https://github.com/neoxelox/vapor/tree/main/core/providers/src/filesystem) — any locally mounted folder (external drives, network mounts, another directory).
- [Google Drive](https://workspace.google.com/intl/es/products/drive) — OAuth sign-in via `vapor auth login gdrive`.

In flight and coming next:

- More cloud providers as demand surfaces (the provider system is pluggable; see `docs/architecture/provider-onboarding.md`).

## Benchmarks

> TBD

## Configuration

All user-facing configuration is documented here with meaning and defaults.

All persisted user configuration lives in `<vapor_dir>/vapor.json`. A running daemon picks up changes to the resource, idle-boost, safeguard, ignore and timeline keys within a few seconds; the keys that reshape the pipeline (`localSyncDirectory`, `cloudSyncDirectory`, `provider`, `syncMode`, `profiles`, `deviceId`) take effect on the next start, and `vapor status` says so until then (`vapor service restart` applies them). `vapor config set` prints which case applies. Some keys can also be set via a matching `VAPOR_*` environment variable, which takes priority over the file.

| Key                  | Type     | Default                                             | Description                                                                              |
| -------------------- | -------- | --------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| `autoLaunch`         | `Bool`   | `true`                                              | Starts Vapor automatically at login and keeps the daemon bootstrapped in the background. |
| `useGitIgnore`       | `Bool`   | `true`                                              | Applies recursive `.gitignore` rules during local filtering.                             |
| `useVaporIgnore`     | `Bool`   | `true`                                              | Applies recursive `.vaporignore` rules during local filtering.                           |
| `localSyncDirectory` | `String` | `"~/Vapor"`                                         | Sets the local sync root; Vapor creates it if it does not exist yet.                     |
| `cloudSyncDirectory` | `String` | `"/Vapor"`                                          | Sets the cloud sync root; Vapor creates it if it does not exist yet. With `provider: "filesystem"` this is a local directory path (`~` and relative paths resolve like `localSyncDirectory`), and it must not overlap the local sync root — overlapping roots refuse to sync. |
| `provider`           | `String` | `"filesystem"`                                      | Chooses the cloud backend: `filesystem` (a local folder acting as the cloud side) or `gdrive` (requires `vapor auth login gdrive`). Google Drive client credentials are supplied per deployment through the `VAPOR_GDRIVE_CLIENT_ID` / `VAPOR_GDRIVE_CLIENT_SECRET` environment variables (see `.env.example` and `docs/operations/provider-auth-operations.md`); user tokens are stored only in the platform secret store, never in `vapor.json`. |
| `syncMode`           | `String` | `"two-way"`                                         | Chooses the sync direction: `two-way` (bidirectional), `pull-only` (cloud → local, a read-only local mirror), or `push-only` (local → cloud, a read-only cloud backup). |
| `preIgnoreRules`     | `String` | Embedded `.gitignore`-like low-impact default rules | Provides the baseline ignore rules that run before discovered ignore files.              |
| `postIgnoreRules`    | `String` | Empty string                                        | Provides the final override rules that run after discovered ignore files.                |
| `languageCode`       | `String` | `"en"`                                              | Selects the UI language catalog to load.                                                 |
| `timelineLimit` | `Int`    | `1000`                                              | Caps the in-memory timeline length shown in diagnostics.                                 |
| `deviceId`           | `String` | Derived from the hostname on first run              | Stable per-device identifier used in conflict-copy names (for example `report~conflict-mac-studio-....pdf`). Written by the daemon; never regenerated silently. |
| `profiles`           | `Array`  | Absent (single implicit profile)                    | Optional named sync profiles. Each entry (`id`, `name`, `enabled`, plus optional `provider`, `localSyncDirectory`, `cloudSyncDirectory`, `syncMode`, `resourceLimits`, `idleBoost` overrides) runs as an isolated pipeline with its own durable state; unset fields inherit the top-level values. The two resource groups merge per field and only toward caution: a `resourceLimits` value can only lower the top-level one, `idleBoost.enabled: false` wins daemon-wide, `boost*Percent`, `headroomCpuPercent` and `rampDownSeconds` can only shrink, `minIdleSeconds` and `rampUpSeconds` can only grow. Fields a profile does not name keep the top-level value. |
| `resourceLimits`     | `Object` | `{ cpuPercent: 15, memoryPercent: 10, bandwidthPercent: 25 }` | Sets hard ceilings on daemon CPU (share of the whole device), device memory (resident size as a share of physical memory), and measured bandwidth. Honored by the throttle controller and auto-tuner. Profile overrides may only lower these values. The optional `maxConcurrentTransfers` (`1..16`, absent = automatic) additionally caps parallel uploads and downloads; when absent, the idle transfer width derives from the machine's core count (4–8 per direction) and always collapses while you are actively using the device. |
| `idleBoost`          | `Object` | `{ enabled: true, minIdleSeconds: 300, headroomCpuPercent: 30, boostCpuPercent: 50, boostMemoryPercent: 20, boostBandwidthPercent: 80, rampUpSeconds: 30, rampDownSeconds: 10 }` | Dynamically raises effective ceilings when the device is user-idle with measured resource headroom. Boost requires all of: throttle state `IdleDrain`, user-idle for at least `minIdleSeconds`, and non-Vapor CPU utilization at or below `headroomCpuPercent`. Each `boost*Percent` must be `>=` the matching `resourceLimits.*Percent` (lower values are treated as equal to the base ceiling). Ramp-down is deliberately faster than ramp-up so returning to your device is never met with a busy daemon. Setting `enabled: false` in any enabled profile disables boost daemon-wide. |
| `safeguards`         | `Object` | `{ massDeleteEnabled: true, massDeleteThreshold: 1000, massDeleteWindowSeconds: 60, massDeleteRatioPercent: 25 }` | Tunes the mass-deletion guard, the ransomware and bulk-mistake backstop. A burst of deletions in either direction (files vanishing from this device, or from the cloud) that reaches `massDeleteThreshold` inside the rolling window, or `massDeleteRatioPercent` of the files Vapor syncs (never fewer than 10), is held whole behind a decision before any of it lands on the other side; everything else keeps syncing. `vapor decisions list` shows the question, `vapor decisions resolve <id> --choose apply` lets the deletions through, `--choose discard` restores the files from the side that still has them. Values below the floors (`10` deletions / `5` seconds) are clamped up; `massDeleteRatioPercent: 0` turns the ratio rule off and leaves the absolute threshold; `massDeleteEnabled: false` turns the guard off, which the daemon logs as a warning. |

### Ignore rules

- Filesystem callback filtering applies before event metadata is recorded.
- Ignore rules are symmetric: a name that matches them never syncs in either direction — it is skipped by local ingest, by the remote changes feed, and by reconcile comparison on both sides — so an ignored file (for example `.DS_Store`) can never be pulled down from the cloud or produce a conflict copy.
- Rule precedence (lowest to highest): `preIgnoreRules` -> `.gitignore` (when enabled, recursive per-directory) -> `.vaporignore` (when enabled, recursive per-directory) -> `postIgnoreRules`.
- `.vaporignore` supports glob-like rules and `!` unignore rules.
- `preIgnoreRules` default content covers common low-signal, regenerable paths across programming ecosystems: dependency/module directories (`node_modules/`, `vendor/`, `Pods/`, …), build outputs (`dist/`, `build/`, `out/`, `target/`, `_build/`, …), tool caches (`__pycache__/`, `.gradle/`, `.terraform/`, `.cache/`, …), and OS/editor junk (`.DS_Store`, swap/tmp/partial files, logs). Repositories (`.git/`) and dotenv files are deliberately NOT ignored — they sync like any other content.

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
- End-to-end verification in disposable repo-local sandboxes: `./scripts/e2e.sh` (one scenario: `--only Sxx`; manual sandbox: `--sandbox`; see `docs/development/e2e-verification.md`)
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

Releases are cut from a clean `main` with `./scripts/version.sh` (`set` / `bump patch|minor|major` / `prerelease` / `release`), which validates the worktree, updates `VERSION`, syncs Cargo metadata, and creates the release commit plus matching `v$(cat VERSION)` tag; pushing them together (`git push origin main --follow-tags`) runs the gated pipeline that builds, signs, and drafts the GitHub Release. Full runbook: `docs/operations/release-process.md`.

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

Start with [CONTRIBUTING.md](CONTRIBUTING.md) for setup, the validation workflow, and what a good pull request looks like — then [AGENTS.md](AGENTS.md) for the full operating rules. Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

Found a security problem? Please report it privately — see [SECURITY.md](SECURITY.md).

## License

This project is licensed under the [GPL-3.0 License](https://opensource.org/license/gpl-3-0). Read the [LICENSE](LICENSE) file for details.
