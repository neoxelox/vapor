<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/github/vapor-banner-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="assets/github/vapor-banner-light.png">
    <img src="assets/github/vapor-banner-light.png" alt="Vapor" width="100%">
  </picture>
</p>

<p align="center"><strong><code>background cloud sync that won't melt your device 🔥</code></strong> - <a href="https://vapor.arn.sh"><strong><code>vapor.arn.sh</code></strong></a></p>

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
- 🗑️ Nothing Vapor removes from your device is gone: it waits in a trash you can list and restore from, for as long as you choose.
- 🔌 An unplugged drive, or an empty folder where your sync folder used to be, never turns into deletions on the other side. Vapor waits for the folder, and asks before it merges anything.
- ✂️ Renaming or moving a file, on either side, moves it on the other side too. No re-upload, no re-download.
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
- ⏯️ Pause, resume, and sync on demand. Changes made while paused sync when you resume, and `vapor reconcile` (Sync now in the app) compares both folders without waiting for an idle moment.
- 📈 Status with a reason, queue depth, a live activity timeline, per-file "why is this stuck", and a one-command support report.
- ⌨️ A full command line for scripts and servers. Everything the app does, the terminal does too, including a headless supervisor that restarts a crashed sync where no app is running.
- 🔐 Sign-in tokens live in the system keychain, logs never contain secrets, and nothing leaves your device except the files you chose to sync.
- 🔑 An expired or revoked cloud sign-in puts sync on hold instead of failing it. Your changes wait in the queue, one click in the app signs you back in, and sync picks up where it stopped.
- 🔕 Lives in the menu bar. No windows unless you ask for one.
- 🟠 The menu bar mark turns orange only when Vapor needs you: a sign-in to renew, a question to answer, a conflict to settle, a setting that wants a restart, a sync that stopped on an error.

In flight and coming next:

- 🪟 A diagnostics window in the Mac app with throttle reason, queue, conflicts, and timeline, plus live pause and resume controls.
- 🚪 A first-run setup that asks which provider and which folders to use before creating anything.
- 🔔 A notification when a conflict needs you.
- 🖥️ Windows and Linux apps on the same runtime as the Mac app.
- 📥 Standalone command-line downloads for every OS, with Docker and systemd recipes.
- 🧾 Signed and notarized releases, verified on a clean machine every cycle.

## Providers

Available now:

- [File System](https://github.com/neoxelox/vapor/tree/main/core/providers/src/filesystem) — any locally mounted folder (external drives, network mounts, another directory).
- [Google Drive](https://workspace.google.com/intl/es/products/drive) — OAuth sign-in via `vapor auth login`.

In flight and coming next:

- More cloud providers as demand surfaces (the provider system is pluggable; see `docs/architecture/provider-onboarding.md`).

## Benchmarks

> TBD

## Configuration

All persisted configuration lives in `<vapor_dir>/vapor.json`. Keys are camelCase; every value that names a choice is kebab-case. A running daemon picks up changes to the ignore, timeline, resource, idle-boost, safeguard, and trash keys within a few seconds. The keys that reshape the pipeline (`localRoot`, `cloudRoot`, `provider`, `syncMode`, `profiles`, `deviceId`) take effect on the next start, and `vapor status` says so until then (`vapor service restart` applies them). `vapor config set` prints which case applies. No environment variable overrides a key: `vapor.json` is the one place a setting lives.

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `autoLaunch` | `Bool` | `true` | Starts Vapor at login and keeps the daemon running in the background. |
| `provider` | `String` | `"filesystem"` | The cloud backend: `filesystem` (a local folder plays the cloud side) or `gdrive` (sign in with `vapor auth login`, or Sign In in the app). The build carries the Google Drive OAuth client (see `VAPOR_GDRIVE_CLIENT_ID` below and `docs/operations/provider-auth-operations.md`); tokens live only in the platform secret store, never in `vapor.json`. |
| `syncMode` | `String` | `"two-way"` | The sync direction: `two-way`, `pull-only` (cloud to local, a read-only local mirror), or `push-only` (local to cloud, a read-only cloud backup). |
| `localRoot` | `String` | `"~/Vapor"` | The folder on this device that Vapor syncs. Vapor creates it on first contact. |
| `cloudRoot` | `String` | Unset: `"~/cloud/Vapor"` with `provider: "filesystem"`, `"/Vapor"` otherwise | The folder on the provider side. Vapor creates it on first contact. With `provider: "filesystem"` this is a local path (`~` and relative paths resolve like `localRoot`), and it must not overlap `localRoot`; overlapping roots refuse to sync. `vapor config get cloudRoot` prints the default for the selected provider. |
| `useGitignore` | `Bool` | `true` | Applies `.gitignore` files found under the local root. |
| `useVaporignore` | `Bool` | `true` | Applies `.vaporignore` files found under the local root. |
| `preIgnoreRules` | `String` | Built-in rules for build output and junk | Rules in `.gitignore` format, one per line, applied before the ignore files. |
| `postIgnoreRules` | `String` | `""` | Rules in `.gitignore` format, one per line, applied after the ignore files; `!pattern` here wins over everything. |
| `timelineLimit` | `Int` | `1000` | How many entries the in-memory activity timeline keeps. |
| `languageCode` | `String` | `"en"` | The app's UI language. |
| `deviceId` | `String` | Derived from the hostname on first run | This device's name in conflict-copy names (`report~conflict-mac-studio-….pdf`). The daemon writes it once and never regenerates it. |
| `profiles` | `Array` | Absent: one implicit profile named `default` | Named sync profiles. Each entry has an `id` and optionally `name`, `enabled`, `provider`, `localRoot`, `cloudRoot`, `syncMode`, `resourceLimits`, and `idleBoost`, and runs as an isolated pipeline with its own state. Unset fields inherit the top-level value. The two resource groups merge per field and only toward caution: a profile's `resourceLimits` value can only lower the top-level one; in `idleBoost`, `enabled: false` wins daemon-wide, the three `*Percent` ceilings, `headroomCpuPercent`, and `rampDownSeconds` can only shrink, and `minIdleSeconds` and `rampUpSeconds` can only grow. |
| `resourceLimits` | `Object` | `{ cpuPercent: 15, memoryPercent: 10, bandwidthPercent: 25 }` | Hard ceilings on the daemon's CPU (share of the whole device), memory (resident size as a share of physical memory), and measured bandwidth. `concurrentTransfers` (`1` to `16`, absent means automatic) also caps parallel uploads and parallel downloads; when absent, the idle width follows the core count (4 to 8 per direction) and always collapses while you use the device. |
| `idleBoost` | `Object` | `{ enabled: true, minIdleSeconds: 300, cpuPercent: 50, memoryPercent: 20, bandwidthPercent: 80, headroomCpuPercent: 30, rampUpSeconds: 30, rampDownSeconds: 10 }` | Raises the ceilings to its own `cpuPercent`, `memoryPercent`, and `bandwidthPercent` while the device is idle: throttle state `idle-drain`, no input for `minIdleSeconds`, and other processes using at most `headroomCpuPercent` of the CPU. A boost ceiling below its `resourceLimits` counterpart counts as equal to it. Ramping down is faster than ramping up, so a returning user never meets a busy daemon. |
| `safeguards` | `Object` | `{ massDelete: { enabled: true, threshold: 1000, windowSeconds: 60, ratioPercent: 25 } }` | `massDelete` is the ransomware and bulk-mistake backstop. A burst of deletes in either direction that reaches `threshold` inside `windowSeconds`, or `ratioPercent` of the synced files (never fewer than 10), is held whole behind a `mass-delete` decision before any of it reaches the other side; everything else keeps syncing. `vapor decisions resolve <id> --choose apply` lets the deletes through, `--choose discard` restores the files from the side that still has them. Values below the floors (10 deletes, 5 seconds) are raised to them; `ratioPercent: 0` turns the ratio rule off; `enabled: false` turns the guard off, and the daemon logs a warning. |
| `trash` | `Object` | `{ enabled: true, retentionDays: 7, useSystem: false }` | Where a file Vapor removes from this device goes (a delete made in the cloud, a `pull-only` mirror removal): the managed trash under `<vapor_dir>/trash/<profile>/`, or `<volume root>/.vapor/trash/<profile>/` when the sync root is on another volume, listed and restored with `vapor trash list` and `vapor trash restore <id>`, purged after `retentionDays`. `useSystem: true` tries the system trash first (the Finder's Trash on macOS, the freedesktop trash on Linux). `enabled: false` deletes for good. |

### Environment variables

| Variable | Description |
| --- | --- |
| `VAPOR_DIR` | The runtime directory (config, logs, state). Default `~/.vapor`. |
| `VAPOR_ENV` | `dev` switches the default runtime directory to `./.vapor` and the default log level to `debug`; `prod` keeps `~/.vapor` and `info`. Unset, a truthy `CI` counts as `dev`. The repository scripts set `dev`. |
| `VAPOR_LOG_LEVEL` | `debug`, `info`, `warning`, or `error`; overrides the level `VAPOR_ENV` picks. |
| `VAPOR_GDRIVE_CLIENT_ID` | The Google OAuth client id. Read at build time, so `vapor` and `vapord` carry the client they were built with (the build scripts load it from `.env`); set at run time, it overrides the built-in client. `vapor doctor` names the client in use. |
| `VAPOR_GDRIVE_CLIENT_SECRET` | The matching client secret, read the same two ways. |
| `VAPOR_SECRETS_COMMAND` | Linux only: a program that stores Vapor's secrets (`pass`, a vault CLI), used instead of the desktop Secret Service. |
| `VAPOR_THROTTLE_INPUTS` | For test harnesses: `host` (default), `static`, or `file:<path>`. Never set it on a real install. |

The build, release, and test variables are in `.env.example`.

### Ignore rules

- Filesystem callback filtering applies before event metadata is recorded.
- Ignore rules are symmetric: a name that matches them never syncs in either direction — it is skipped by local ingest, by the remote changes feed, and by reconcile comparison on both sides — so an ignored file (for example `.DS_Store`) can never be pulled down from the cloud or produce a conflict copy.
- Rule precedence (lowest to highest): `preIgnoreRules` -> `.gitignore` (when enabled, recursive per-directory) -> `.vaporignore` (when enabled, recursive per-directory) -> `postIgnoreRules`.
- `.vaporignore` supports glob-like rules and `!` unignore rules.
- `preIgnoreRules` default content covers common low-signal, regenerable paths across programming ecosystems: dependency/module directories (`node_modules/`, `vendor/`, `Pods/`, …), build outputs (`dist/`, `build/`, `out/`, `target/`, `_build/`, …), tool caches (`__pycache__/`, `.gradle/`, `.terraform/`, `.cache/`, …), and OS/editor junk (`.DS_Store`, swap/tmp/partial files, logs, what an OS keeps at the root of a volume such as `.fseventsd/`, `.Spotlight-V100/`, `System Volume Information/`). Repositories (`.git/`) and dotenv files are deliberately NOT ignored — they sync like any other content. Independently of every rule, a directory named `.vapor` never syncs, with everything under it: it is Vapor's own runtime state or a volume's trash, wherever it sits.

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
- Sync `assets/` (locale catalogs, brand images) into every app surface: `./scripts/resources.sh`
- Install git pre-commit hook (clean → lint → test → build): `./scripts/hooks.sh`
- Remove the installed pre-commit hook: `./scripts/hooks.sh uninstall`
- Version helper: `./scripts/version.sh`
- Performance gate, one soak cell with the SLO checks: `./scripts/perf.sh` (`VAPOR_PERF_SOAK_DURATION`, default `12m`; `VAPOR_PERF_SOAK_SEED`, default `11`)

Stack-specific helpers:

- Rust lint: `./scripts/rust/lint.sh`
- Rust format: `./scripts/rust/format.sh check`
- Rust tests: `./scripts/rust/test.sh`
- Rust build: `./scripts/rust/build.sh`
- Swift lint: `./scripts/swift/lint.sh`
- Swift format: `./scripts/swift/format.sh check`
- Swift tests: `./scripts/swift/test.sh`
- Swift build: `./scripts/swift/build.sh`
- Swift resource sync: `./scripts/swift/resources.sh`
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
