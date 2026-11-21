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

One file per device, and nothing else to install: it holds the app, the `vapor` command, and the daemon that syncs in the background. Pick your row and take the file from the [latest release](https://github.com/neoxelox/vapor/releases).

| Your device | File | Version | Status |
| --- | --- | --- | --- |
| Mac with Apple silicon, macOS 26 or later | [`Vapor-macos-arm64.pkg`](https://github.com/neoxelox/vapor/releases) | [![latest release](https://img.shields.io/github/v/release/neoxelox/vapor?include_prereleases&label=)](https://github.com/neoxelox/vapor/releases) | Prerelease, not signed yet |
| Windows, x64 | `Vapor-windows-x64.msi` | none yet | Planned |
| Windows, ARM64 | `Vapor-windows-arm64.msi` | none yet | Planned |
| Linux x64, Debian and Ubuntu | `Vapor-linux-x64.deb` | none yet | Planned |
| Linux ARM64, Debian and Ubuntu | `Vapor-linux-arm64.deb` | none yet | Planned |
| Linux x64, Fedora and RHEL | `Vapor-linux-x64.rpm` | none yet | Planned |
| Linux ARM64, Fedora and RHEL | `Vapor-linux-arm64.rpm` | none yet | Planned |

There is no Intel Mac build. The installer comes with the release after `0.2.0-alpha.3`; that one and the ones before it hold `Vapor.zip`: unzip it and move Vapor to Applications.

### With a double click

1. Download the file for your device.
2. Open it and follow the installer. A prerelease is not signed yet, so macOS stops it once: allow it under System Settings > Privacy & Security > Open Anyway.
3. Open a terminal and run `vapor init`. It asks which cloud, which folder on this device, and which folder in the cloud. Then sync starts, and it keeps running in the background. The app's own setup screen is on the way.

### From a terminal

```sh
sudo installer -pkg Vapor-macos-arm64.pkg -target /
vapor init
```

A script gives the answers as flags, and nothing is asked: `vapor init --provider filesystem --local ~/Vapor --cloud /Volumes/Backup/Vapor`. For Google Drive it also pipes a sign-in token with `--token -`. Run `vapor init` again whenever you like: it does only what is left, and tells you when nothing is. The detail is in [Install and first launch](docs/product/install.md).

## Features

Available now:

- 🔁 **Two-way sync.** A local folder and a cloud folder stay in step, usually within seconds.
- 🧠 **Crash-safe queue.** Every change is saved to disk before it moves, so a crash or a reboot loses nothing.
- 🛡 **Keep-both conflicts.** When two devices edit the same file, every version is kept and you pick the one to keep.
- ❓ **Decisions.** When Vapor is not sure, it holds only those files and asks you a plain question.
- 🛟 **Mass-delete guard.** A sudden burst of deletions waits for your yes before it reaches the other side.
- 🔌 **Unplug-safe.** An unplugged drive or a swapped sync folder never turns into deletions.
- 🗑️ **Trash.** Everything Vapor removes on your device waits in a trash you can restore from.
- ✂️ **Move detection.** A rename or a move syncs as a move, never as a new upload.
- 🔤 **Case-safe names.** Two files that differ only in capitals never overwrite each other.
- 🔂 **Loop prevention.** Vapor's own writes never bounce back as new changes.
- ⏮️ **Offline catch-up.** Edits, renames, and deletions made while Vapor was off sync at the next start.
- 🔀 **Sync modes.** Two-way, or a one-way mirror for backups, which runs only after you confirm it.
- 🪄 **One-command setup.** `vapor init` takes a fresh install to a running sync: cloud, sign-in, folders, start at login. Run it again and it does only what is left.
- 🧩 **Profiles.** Nothing syncs until you choose a folder pair, and several can run at once (`vapor profiles add`).
- 📦 **Resilient transfers.** Downloads land whole, big uploads resume, and retries back off and respect rate limits.
- 🪶 **Adaptive throttle.** Sync slows down on battery or while you work, and pauses only if the device overheats.
- 🌙 **Idle boost.** Sync speeds up while the device sits idle and backs off the moment you return.
- ⚙️ **Resource ceilings.** You cap the share of CPU, memory, and bandwidth, and Vapor measures itself against it.
- 🌩 **Storm control.** A burst of thousands of changes becomes one scan, not thousands of uploads.
- 🧹 **Ignore rules.** Your `.gitignore` files work as they are, and `vapor ignored` shows what they hold back.
- ♻️ **Live settings.** Ceilings, ignore rules, and safeguards apply while Vapor runs, with no restart.
- 🚀 **Always on.** It starts at login, restarts after a crash with or without the app open, and stops retrying when something is really broken.
- 🧰 **One installer.** The app, the daemon, and `vapor` on your `PATH` arrive together, and installing a newer file updates all three.
- ⏯️ **Pause, resume, Sync now.** Changes wait while paused, and Sync now compares both folders right away.
- 📈 **Explains itself.** Status with a reason, a live timeline, and a support report with your file names hidden.
- ⌨️ **Full command line.** Everything the app does, `vapor` does too, for scripts and for servers with no app.
- 🔐 **Private by design.** Tokens stay in the system keychain, logs hold no secrets, and only your files leave the device.
- 🔑 **Sign-in hold.** An expired cloud sign-in puts sync on hold, and signing in again resumes it where it stopped.
- 🔕 **Menu bar app.** No window unless you ask, and the mark turns orange only when Vapor needs you.

In flight and coming next:

- 🪟 **Diagnostics window.** Throttle reason, queue, open questions, and timeline in the Mac app.
- 🚪 **First-run setup.** The Mac app asks for the cloud and the two folders, in place of the terminal command.
- 🔔 **Notifications.** A system notification when Vapor has a question for you.
- 🖥️ **Windows and Linux apps.** The same runtime as the Mac app.
- 🐧 **Linux packages for servers.** A `.deb` and an `.rpm` that install on a server with no desktop, with Docker and systemd recipes.
- 🗂️ **Cloud folder picker.** Setup lists the folders of your cloud, so you pick one or make a new one.
- 🧾 **Signed releases.** Signed and notarized, and verified on a clean machine every cycle.

## Providers

Available now:

- [File System](https://github.com/neoxelox/vapor/tree/main/core/providers/src/filesystem): any locally mounted folder (external drives, network mounts, another directory).
- [Google Drive](https://workspace.google.com/products/drive/): sign in with `vapor auth login`, or Sign In in the app.

In flight and coming next:

- More cloud providers as demand surfaces (the provider system is pluggable; see `docs/architecture/provider-onboarding.md`).

## Commands

`vapor` does everything the app does, from a terminal or a script. Every command answers `--help`, and `--json` prints machine-readable output where the Flags column lists it.

| Command | Flags | What it does |
| --- | --- | --- |
| `vapor init` | `--provider <provider>`, `--local <path>`, `--cloud <path>`, `--mode <mode>`, `--token -`, `--auto-launch <on\|off>`, `--linger <on\|off>` (Linux), `--json` | Sets Vapor up: the cloud, the sign-in, the two folders, and the background service. A terminal asks for each answer; a script gives them as flags and has to give `--local` and `--cloud`. A later run does only what is left. Exits with 1 while a step waits for you. |
| `vapor status` | `--json` | Shows what sync is doing and why: run state, throttle, queue, open questions. |
| `vapor pause` | `--json` | Pauses sync. Changes are still recorded and sync on resume. |
| `vapor resume` | `--json` | Resumes sync. |
| `vapor reconcile` | `--json` | Compares both folders now and syncs the differences. |
| `vapor profiles list` | `--json` | Lists the sync profiles: the folders, the cloud, the direction. |
| `vapor profiles add` | `--id <id>`, `--name <name>`, `--provider <provider>`, `--local <path>`, `--cloud <path>`, `--mode <mode>`, `--json` | Adds a profile. On a terminal it asks for what you leave out; a script has to give `--local` and `--cloud`. The first one starts syncing at once. `vapor init` runs it for a fresh install, with the sign-in and the background service. |
| `vapor profiles remove <id>` | `--yes`, `--json` | Stops syncing a profile and forgets its sync state and sign-in. The files on both sides stay. Needs the daemon stopped. |
| `vapor ignored` | `--profile <id>`, `--json` | Lists what the ignore rules keep out of sync, with the rule that hides each path. |
| `vapor decisions list` | `--all`, `--json` | Lists the questions waiting for you: conflicts, and actions Vapor will not take on its own. `--all` adds the answered ones. |
| `vapor decisions show <id>` | `--profile <id>`, `--json` | Shows one question with its options and evidence. |
| `vapor decisions resolve <id>` | `--choose <option>`, `--profile <id>`, `--json` | Answers a question. For a conflict, the option is the file name of the version to keep. |
| `vapor trash list` | `--json` | Lists what Vapor removed from this device, newest first. |
| `vapor trash restore <id>` | `--profile <id>`, `--json` | Puts an item back where it was. |
| `vapor trash empty` | `--profile <id>`, `--yes`, `--json` | Deletes everything in the trash, for good. Asks first on a terminal; `--yes` skips the question. |
| `vapor config get <key>` | | Prints a setting, or its default when unset. |
| `vapor config set <key> <value>` | | Changes one setting and says when it takes effect. |
| `vapor auth login` | `--profile <id>`, `--browser`, `--token -` | Signs a profile in to its cloud. `--token -` reads a token from stdin; a token is never taken as an argument. |
| `vapor auth logout` | `--profile <id>` | Removes a profile's stored sign-in. With no profile yet, it removes the sign-in that `vapor init` kept. |
| `vapor auth status` | `--profile <id>`, `--json` | Shows which profiles are signed in. |
| `vapor service install` | `--linger <on\|off>` (Linux), `--json` | Starts Vapor at login and starts it now, with the supervisor that restarts a crashed daemon. On a Linux host with no desktop it turns lingering on, so Vapor keeps running after you log out; `--linger off` leaves that alone. |
| `vapor service uninstall` | `--keep-running`, `--json` | Stops starting Vapor at login, and stops it unless `--keep-running`. |
| `vapor service start`, `stop`, `restart` | `--json` | Starts, stops, or restarts the daemon. A restart applies the settings that need one. |
| `vapor service status` | `--json` | Says whether the service is installed and running, or paused after repeated crashes. Changes nothing. |
| `vapor service bootstrap` | `--json` | Installs and starts the service when `autoLaunch` is on. The app runs it at startup. |
| `vapor service check` | `--loop`, `--interval <seconds>`, `--json` | Restarts the daemon if it exited unexpectedly, within the crash-loop limits. The supervisor runs it for you. |
| `vapor service acknowledge` | `--json` | Clears a crash-loop pause so the daemon may start again. |
| `vapor timeline` | `--json` | Shows recent activity. |
| `vapor diagnostics` | `--json` | Lists queued and in-flight work, with the reason each item waits. |
| `vapor logs` | `--tail <lines>` | Prints the daemon log. |
| `vapor doctor` | `--json` | Checks that this device can run Vapor and says what to fix. |
| `vapor report` | `--output <dir>`, `--keep-paths`, `--json` | Writes a support report: settings, logs, and the daemon's live state, with file and folder names replaced by codes. `--keep-paths` keeps the names, for a report that stays with you. |
| `vapor run` | | Runs the daemon in this terminal, for a server or for debugging. |
| `vapor version` | | Prints the version and the git commit. |

## Configuration

Settings live in `vapor.json` inside the runtime directory (`~/.vapor/vapor.json` by default). Change any of them with `vapor config set <key> <value>` or by editing the file; the app's Settings covers start at login, the ignore rules, and the language. A dotted key changes one field of a group (`vapor config set resourceLimits.cpuPercent 10`), and a list takes JSON (`vapor config set postIgnoreRules '["build/", "*.log"]'`). No environment variable overrides a setting.

The Applies column says when a change takes effect: **live** within a few seconds while the daemon runs; **restart** when the daemon restarts (`vapor service restart`; `vapor status` says one is pending); **app** when the Mac app reads it (`autoLaunch` is also acted on by `vapor service`). Edge cases, floors, and how profiles merge: `docs/product/configuration.md`.

| Key | Default | Applies | What it controls |
| --- | --- | --- | --- |
| `autoLaunch` | `true` | app | Start Vapor at login. `vapor service install` and `uninstall` set it. |
| `provider` | `"filesystem"` | restart | The cloud: `filesystem` (a local folder) or `gdrive` (Google Drive). |
| `syncMode` | `"two-way"` | restart | The direction: `two-way`, `pull-only` (a read-only local mirror), or `push-only` (a read-only cloud backup). A one-way mode waits for your confirmation (`vapor decisions`). |
| `localRoot` | none: you choose it | restart | The folder on this device. Nothing syncs until both folders are set. |
| `cloudRoot` | none: you choose it | restart | The folder in the cloud. |
| `useGitignore` | `true` | live | Honour `.gitignore` files under the local root. [Ignore rules](docs/product/ignore-rules.md). |
| `useVaporignore` | `true` | live | Honour `.vaporignore` files under the local root. [Ignore rules](docs/product/ignore-rules.md). |
| `preIgnoreRules` | a built-in list | live | Rules applied before the ignore files, one `.gitignore` rule per entry. [Ignore rules](docs/product/ignore-rules.md). |
| `postIgnoreRules` | `[]` | live | Rules applied after the ignore files; `!rule` here wins over everything. [Ignore rules](docs/product/ignore-rules.md). |
| `timelineLimit` | `1000` | live | How many entries `vapor timeline` keeps, at most `10000`. |
| `language` | `"en"` | app | The app's language. |
| `deviceId` | the host name plus four random characters | restart | This device's name in conflict copies. Written once on the first run. |
| `profiles` | none: one profile named `default` | restart | Several sync pairs at once, each with its own state. See below. |
| `resourceLimits.cpuPercent` | `15` | live | CPU ceiling, as a share of the whole device. |
| `resourceLimits.memoryPercent` | `10` | live | Memory ceiling, as a share of physical memory. |
| `resourceLimits.bandwidthPercent` | `25` | live | Network ceiling, as a share of what the link carries, which Vapor measures. |
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

Each `profiles` entry has an `id` and optionally `name`, `enabled`, `provider`, `localRoot`, `cloudRoot`, `syncMode`, `resourceLimits`, and `idleBoost`; unset fields inherit the top-level value, and a profile's limits can only be stricter than the top-level ones. `vapor profiles add` writes an entry for you; on a terminal it suggests `~/Vapor` and `/Vapor` for the two folders. The first profile starts without a restart.

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

## Development

This project is intentionally vibe-coded while still following strict reliability, safety, and low-impact engineering rules.

Structure:

- `core/daemon`: Rust daemon runtime (`vapord`); the portable sync engine.
- `core/providers`: Rust cloud provider integrations.
- `core/shared`: shared contracts/constants/config loader used across the workspace.
- `core/ipc`: framed JSON IPC channel between the daemon and every surface.
- `core/platform`: traits + per-OS native implementations for fs-watch, service install, secrets, metrics sampling, idle detection, filesystem capabilities, process supervision, and the system trash.
- `core/lifecycle`: daemon lifecycle manager and crash-loop guard consumed by every app surface.
- `core/cli`: the `vapor` CLI; headless-first control plane usable on every supported OS.
- `apps/macos`: SwiftUI macOS app (`Vapor`).
- `apps/windows` (planned): Windows app surface consuming `core/*`.
- `apps/linux` (planned): Linux app surface consuming `core/*`.
- `tools/e2e`, `tools/soak`: the end-to-end harness and the soak driver. Development tools; they never ship.
- `assets/`: brand artwork and the UI copy catalogs (`assets/locales`).

The Rust core (`core/*`) is the single portable runtime. Every app surface is a thin UI + OS-integration shim over it.

See `.env.example` for the available `VAPOR_*` environment variables used by apps, scripts, CI, and packaging flow. The release version source of truth lives in `VERSION`.

### Scripts

Every workflow goes through a script under `scripts/`, never a raw `cargo` or `swift` command. The ones you run on every change, in this order:

- Format both stacks: `./scripts/format.sh`
- Lint both stacks: `./scripts/lint.sh` (needs `cargo install cargo-deny --locked` and a connection: it checks the dependencies against the RustSec advisories)
- Test both stacks: `./scripts/test.sh`
- End-to-end verification in disposable repo-local sandboxes, for a change a user could observe: `./scripts/e2e.sh` (one scenario: `--only Sxx`; manual sandbox: `--sandbox`)

The full list (build, package, clean, the pre-commit hook, the soak, the performance gate, the release gate, the version helper, the per-stack helpers) is in `docs/development/runbook.md`.

### Releases

Releases are cut from a clean `main`: run the release gate (`./scripts/release.sh`), then `./scripts/version.sh` (`set` / `bump patch|minor|major` / `prerelease` / `release`), which refuses a version that does not sort above the current one, validates the worktree, updates `VERSION`, syncs Cargo metadata, and creates the release commit plus matching `v$(cat VERSION)` tag. Pushing them together (`git push origin main --follow-tags`) runs the gated pipeline that builds, packages, and drafts the GitHub Release. A stable tag ships only signed artifacts; a prerelease (`-alpha`, `-beta`, `-rc`) ships unsigned when no signing material is configured. Full runbook: `docs/operations/release-process.md`.

## Agents

Use `docs/README.md` as the entrypoint index for agent work. Every documentation group has a `README.md` that is the starting point for that group: always enter at the group README, not at an individual file.

- Product direction, status, goals: `docs/product/README.md`
- Architecture, system boundaries, platform abstractions, IPC contracts: `docs/architecture/README.md`
- Operations, release process, runtime logging, provider auth, incident playbooks: `docs/operations/README.md`
- Local developer runbook and toolchain baseline: `docs/development/README.md`
- CI workflows and required-check policy: `docs/ci/README.md`
- Performance SLOs and benchmark harness: `docs/performance/README.md`
- Per-surface implementation plans (core, macos, cli): `docs/plans/README.md`
- Per-surface task lists and the cross-surface roadmap ("what should be done next"): `docs/tasks/README.md`
- Feature specs, with the numbered decisions behind each feature: `docs/specs/README.md`
- Contributor operating rules: `AGENTS.md`

## Contribute

Feel free to contribute to this project : ) .

Start with [CONTRIBUTING.md](CONTRIBUTING.md) for setup, the validation workflow, and what a good pull request looks like, then [AGENTS.md](AGENTS.md) for the full operating rules. Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

Found a security problem? Please report it privately, see [SECURITY.md](SECURITY.md).

## License

This project is licensed under the [GPL-3.0 License](https://opensource.org/license/gpl-3-0). Read the [LICENSE](LICENSE) file for details.
