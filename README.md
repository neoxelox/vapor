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
- **CLI (`vapor`)**: ships inside the app, at `Vapor.app/Contents/Helpers/vapor`. Link it onto your `PATH` to use it from a terminal. Standalone downloads are planned.

## Features

Available now:

- 🔁 Two-way sync between a local folder and a cloud folder. Changes are picked up within seconds while you work.
- 🧠 Nothing is lost. Every change is written to disk before it moves, so a crash, a reboot, or a dropped connection resumes where it stopped.
- 🛡 Edits never silently overwrite each other. When several devices change the same file before it syncs, Vapor keeps every version.
- 🧭 Each conflicted file becomes one question that lists every version and whose edit it is. Pick the one to keep, from any device, and the others go to the trash; or settle it in the Finder.
- ❓ When the evidence is ambiguous, Vapor holds only the files in question, asks you a plain question with a short list of answers, and keeps the question until you answer it, from the terminal or the app.
- 🗑️ Nothing Vapor removes from your device is gone: it waits in a trash you can list and restore from, for as long as you choose.
- 🔌 An unplugged drive, or an empty folder where your sync folder used to be, never turns into deletions on the other side. Vapor waits for the folder, and asks before it merges anything.
- ✂️ Renaming or moving a file, on either side, moves it on the other side too. No re-upload, no re-download.
- 🔤 Two cloud files whose names differ only in case both reach a device that cannot tell them apart: the second arrives as a copy tied to its own cloud file, and neither is overwritten.
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

- 🪟 A diagnostics window in the Mac app with throttle reason, queue, open questions, and timeline, plus live pause and resume controls.
- 🚪 A first-run setup that asks which provider and which folders to use before creating anything.
- 🔔 A notification when Vapor has a question for you, conflicts included.
- 🖥️ Windows and Linux apps on the same runtime as the Mac app.
- 📥 Standalone command-line downloads for every OS, with Docker and systemd recipes.
- 🧾 Signed and notarized releases, verified on a clean machine every cycle.

## Providers

Available now:

- [File System](https://github.com/neoxelox/vapor/tree/main/core/providers/src/filesystem) — any locally mounted folder (external drives, network mounts, another directory).
- [Google Drive](https://workspace.google.com/products/drive/) — sign in with `vapor auth login`, or Sign In in the app.

In flight and coming next:

- More cloud providers as demand surfaces (the provider system is pluggable; see `docs/architecture/provider-onboarding.md`).

## Benchmarks

> TBD

## Commands

`vapor` does everything the app does, from a terminal or a script. Every command answers `--help`, and `--json` prints machine-readable output where the Flags column lists it.

| Command | Flags | What it does |
| --- | --- | --- |
| `vapor status` | `--json` | Shows what sync is doing and why: run state, throttle, queue, open questions. |
| `vapor pause` | `--json` | Pauses sync. Changes are still recorded and sync on resume. |
| `vapor resume` | `--json` | Resumes sync. |
| `vapor reconcile` | `--json` | Compares both folders now and syncs the differences, without waiting for the device to go idle. |
| `vapor decisions list` | `--all`, `--json` | Lists the questions waiting for you: conflicts, and actions Vapor will not take on its own. `--all` adds the answered ones. |
| `vapor decisions show <id>` | `--profile <id>`, `--json` | Shows one question with its options and evidence. |
| `vapor decisions resolve <id>` | `--choose <option>`, `--profile <id>`, `--json` | Answers a question. For a conflict, the option is the file name of the version to keep. |
| `vapor trash list` | `--json` | Lists what Vapor removed from this device, newest first. |
| `vapor trash restore <id>` | `--profile <id>`, `--json` | Puts an item back where it was. |
| `vapor trash empty` | `--profile <id>`, `--json` | Deletes everything in the trash, for good. |
| `vapor config get <key>` | | Prints a setting, or its default when unset. |
| `vapor config set <key> <value>` | | Changes one setting and says when it takes effect. |
| `vapor auth login` | `--profile <id>`, `--browser`, `--token <token>` | Signs a profile in to its cloud. |
| `vapor auth logout` | `--profile <id>` | Removes a profile's stored sign-in. |
| `vapor auth status` | `--profile <id>`, `--json` | Shows which profiles are signed in. |
| `vapor service install` | `--supervise`, `--json` | Starts Vapor at login and starts it now. `--supervise` adds a supervisor that restarts a crashed daemon where no app runs. |
| `vapor service uninstall` | `--keep-running`, `--json` | Stops starting Vapor at login, and stops it unless `--keep-running`. |
| `vapor service start`, `stop`, `restart` | `--json` | Starts, stops, or restarts the daemon. A restart applies the settings that need one. |
| `vapor service status` | `--json` | Says whether the service is installed and running, or paused after repeated crashes. |
| `vapor service bootstrap` | `--json` | Installs and starts the service when `autoLaunch` is on. The app runs it at startup. |
| `vapor service check` | `--loop`, `--interval <seconds>`, `--json` | Restarts the daemon if it exited unexpectedly, within the crash-loop limits. |
| `vapor service acknowledge` | `--json` | Clears a crash-loop pause so the daemon may start again. |
| `vapor timeline` | `--json` | Shows recent activity. |
| `vapor diagnostics` | `--json` | Lists queued and in-flight work, with the reason each item waits. |
| `vapor logs` | `--tail <lines>` | Prints the daemon log. |
| `vapor doctor` | `--json` | Checks that this device can run Vapor and says what to fix. |
| `vapor report` | `--output <dir>`, `--json` | Writes a support report: settings, logs, and the daemon's live state. |
| `vapor run` | | Runs the daemon in this terminal, for a server or for debugging. |
| `vapor version` | | Prints the version and the git commit. |

`vapor service` takes `--user` (the default, a per-user service) before the subcommand; `--system` is reserved for a system-wide service and not available yet.

## Configuration

Settings live in `vapor.json` inside the runtime directory (`~/.vapor/vapor.json` by default). Change any of them with `vapor config set <key> <value>` or by editing the file; the app's Settings covers start at login, the ignore rules, and the language. A dotted key changes one field of a group (`vapor config set resourceLimits.cpuPercent 10`), and a list takes JSON (`vapor config set postIgnoreRules '["build/", "*.log"]'`). No environment variable overrides a setting.

The Applies column says when a change takes effect: **live** within a few seconds while the daemon runs; **restart** when the daemon restarts (`vapor service restart`; `vapor status` says one is pending); **app** when the Mac app reads it (`autoLaunch` is also acted on by `vapor service`). Edge cases, floors, and how profiles merge: `docs/product/configuration.md`.

| Key | Default | Applies | What it controls |
| --- | --- | --- | --- |
| `autoLaunch` | `true` | app | Start Vapor at login. `vapor service install` and `uninstall` set it. |
| `provider` | `"filesystem"` | restart | The cloud: `filesystem` (a local folder) or `gdrive` (Google Drive). |
| `syncMode` | `"two-way"` | restart | The direction: `two-way`, `pull-only` (a read-only local mirror), or `push-only` (a read-only cloud backup). |
| `localRoot` | `"~/Vapor"` | restart | The folder on this device. |
| `cloudRoot` | `"~/cloud/Vapor"` with `filesystem`, `"/Vapor"` otherwise | restart | The folder in the cloud. |
| `useGitignore` | `true` | live | Honour `.gitignore` files under the local root. |
| `useVaporignore` | `true` | live | Honour `.vaporignore` files under the local root. |
| `preIgnoreRules` | a built-in list | live | Ignore rules applied before the ignore files, one `.gitignore` rule per entry. |
| `postIgnoreRules` | `[]` | live | Ignore rules applied after the ignore files; `!rule` here wins over everything. |
| `timelineLimit` | `1000` | live | How many entries `vapor timeline` keeps. |
| `language` | `"en"` | app | The app's language. |
| `deviceId` | derived from the hostname | restart | This device's name in conflict copies. Written once on the first run. |
| `profiles` | none: one profile named `default` | restart | Several sync pairs at once, each with its own state. See below. |
| `resourceLimits.cpuPercent` | `15` | live | CPU ceiling, as a share of the whole device. |
| `resourceLimits.memoryPercent` | `10` | live | Memory ceiling, as a share of physical memory. |
| `resourceLimits.bandwidthPercent` | `25` | live | Network ceiling, as a share of measured bandwidth. |
| `resourceLimits.concurrentTransfers` | automatic | live | Parallel uploads and parallel downloads, `1` to `16`. |
| `idleBoost.enabled` | `true` | live | Raise the ceilings while the device is idle. |
| `idleBoost.minIdleSeconds` | `300` | live | Seconds without keyboard or pointer input before boosting. |
| `idleBoost.cpuPercent` | `50` | live | CPU ceiling while boosted. |
| `idleBoost.memoryPercent` | `20` | live | Memory ceiling while boosted. |
| `idleBoost.bandwidthPercent` | `80` | live | Network ceiling while boosted. |
| `idleBoost.headroomCpuPercent` | `30` | live | Boost only while other processes use at most this share of the CPU. |
| `idleBoost.rampUpSeconds` | `30` | live | Time to climb to the boosted ceilings. |
| `idleBoost.rampDownSeconds` | `10` | live | Time to drop back when you return. |
| `safeguards.massDelete.enabled` | `true` | live | Hold a burst of deletions and ask before it reaches the other side. |
| `safeguards.massDelete.threshold` | `1000` | live | Deletions inside the window that make a burst (at least 10). |
| `safeguards.massDelete.windowSeconds` | `60` | live | The window (at least 5). |
| `safeguards.massDelete.ratioPercent` | `25` | live | Share of the synced files that also makes a burst; `0` turns this rule off. |
| `trash.enabled` | `true` | live | Keep what Vapor removes from this device; `false` deletes for good. |
| `trash.retentionDays` | `7` | live | Days an item stays in the trash. |
| `trash.useSystem` | `false` | live | Try the system trash (the Finder's Trash) first. |

Each `profiles` entry has an `id` and optionally `name`, `enabled`, `provider`, `localRoot`, `cloudRoot`, `syncMode`, `resourceLimits`, and `idleBoost`; unset fields inherit the top-level value, and a profile's limits can only be stricter than the top-level ones.

### Environment variables

| Variable | Default | What it controls |
| --- | --- | --- |
| `VAPOR_DIR` | `~/.vapor` | The runtime directory: settings, logs, state, trash. |
| `VAPOR_ENV` | unset, which acts as `prod` | `dev` moves the default runtime directory to `./.vapor` and the log level to `debug`. The repository scripts set it. |
| `VAPOR_LOG_LEVEL` | `info` (`debug` with `VAPOR_ENV=dev`) | `debug`, `info`, `warning`, or `error`. |
| `VAPOR_GDRIVE_CLIENT_ID` | the client built in | The Google OAuth client id. Read when `vapor` and `vapord` are built (the build scripts load it from `.env`); set at run time, it overrides the built-in one. `vapor doctor` names the client in use. |
| `VAPOR_GDRIVE_CLIENT_SECRET` | the secret built in | The matching client secret, read the same two ways. |
| `VAPOR_SECRETS_COMMAND` | unset | Linux only: a program that stores Vapor's secrets (`pass`, a vault CLI), used instead of the desktop Secret Service. |
| `VAPOR_THROTTLE_INPUTS` | `host` | Test harnesses only: `static` or `file:<path>` replace the device's real load readings. Never set it on a real install. |

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
- `tools/e2e`, `tools/soak`: the end-to-end harness and the soak driver. Development tools; they never ship.
- `assets/`: brand artwork and the UI copy catalogs (`assets/locales`).

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
- Install git pre-commit hook (lint → test → build): `./scripts/hooks.sh`
- Remove the installed pre-commit hook: `./scripts/hooks.sh uninstall`
- Version helper: `./scripts/version.sh`
- Soak, hours of file churn on both sides with a no-loss check after every phase: `./scripts/soak.sh` (see `docs/development/soak-testing.md`)
- Performance gate, one soak cell with the SLO checks: `./scripts/perf.sh` (`VAPOR_PERF_SOAK_DURATION`, default `12m`; `VAPOR_PERF_SOAK_SEED`, default `11`)
- Release gate, run by hand before a version bump (format, lint, tests, and the e2e suite once per provider): `./scripts/release.sh`

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

Releases are cut from a clean `main`: run the release gate (`./scripts/release.sh`), then `./scripts/version.sh` (`set` / `bump patch|minor|major` / `prerelease` / `release`), which validates the worktree, updates `VERSION`, syncs Cargo metadata, and creates the release commit plus matching `v$(cat VERSION)` tag. Pushing them together (`git push origin main --follow-tags`) runs the gated pipeline that builds, packages, and drafts the GitHub Release. A stable tag ships only signed artifacts; a prerelease (`-alpha`, `-beta`, `-rc`) ships unsigned when no signing material is configured. Full runbook: `docs/operations/release-process.md`.

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
