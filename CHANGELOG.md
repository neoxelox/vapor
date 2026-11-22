# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) with pre-GA policy details defined in `docs/operations/release-process.md`.

## [Unreleased]

One line for each change a user can notice. The detail of each one is
in its pull request and in the documents under `docs/`.

### Added

- Two-way sync between a folder on this device and a folder in the cloud, with two providers: any mounted folder (`filesystem`) and Google Drive (`gdrive`, `vapor auth login`).
- One-way modes for each profile: `pull-only` (a read-only local mirror) and `push-only` (a read-only cloud backup). A one-way profile syncs nothing until you confirm what the mode overwrites and removes.
- Several sync profiles at once, each with its own state. `vapor profiles list|add|remove` manages them, and the first `add` sets a fresh install up with no restart.
- Conflicts keep every version. When several devices edit a file before it syncs, one question in `vapor decisions` lists the versions and lets you pick the one to keep.
- Decisions. When an action that cannot be undone rests on evidence that reads two ways, Vapor holds only the files in question and asks: a burst of deletions, a sync folder that is missing or replaced, a file where the other side has a folder, a name that cannot sync, a one-way mode.
- A trash for everything Vapor removes on this device (`vapor trash list|restore|empty`), kept for `trash.retentionDays`.
- An unplugged drive, or an empty folder where a sync folder was, never becomes deletions on the other side. Vapor waits for the folder and asks before it merges.
- A rename or a move, on either side, moves the file on the other side with no new transfer. A rename that changes only the capitals counts.
- A deletion made while Vapor was not running reaches the other side when the other copy is unchanged, and never when it changed.
- Ceilings on CPU, memory, and bandwidth (`resourceLimits`), measured while Vapor runs, and an idle boost (`idleBoost`) that raises them while the device is idle.
- The bandwidth ceiling is a share of what your link carries. Vapor measures that from its own transfers, so a faster link syncs faster.
- Settings apply while Vapor runs: ceilings, ignore rules, safeguards, and the trash within seconds. `vapor status` says when a setting needs a restart.
- Ignore rules in both directions: `preIgnoreRules`, `.gitignore`, `.vaporignore`, `postIgnoreRules`. `vapor status` counts what they hold back, and `vapor ignored` lists each path with its rule.
- The `vapor` command line: `init`, `status`, `pause`, `resume`, `reconcile`, `profiles`, `decisions`, `ignored`, `trash`, `config`, `auth`, `service`, `timeline`, `diagnostics`, `logs`, `doctor`, `report`, `run`, `version`. Most take `--json`.
- `vapor report` writes a support report with file and folder names replaced by codes. `--keep-paths` keeps the names.
- Start at login, crash-loop protection that survives a restart, and a supervisor on every install that restarts a crashed daemon, with the app open or closed.
- `vapor init` sets Vapor up in one command, and the Mac release is one installer, `Vapor-macos-arm64.pkg`, that puts the app, the daemon, and `vapor` on the `PATH`. It replaces `Vapor.zip`, and `vapor service install --supervise` is gone: every install restarts a crashed daemon.
- A cloud sign-in that expired or was revoked puts the profile on hold. `vapor auth login`, or Sign In in the app, resumes it where it stopped.
- The Mac app shows what sync is doing and why, has Sync now and a Sign In button, and its menu bar mark turns orange when Vapor needs you.
- The daemon runs on Linux: inotify, a systemd user unit, the freedesktop trash, and the Secret Service or an external secrets command.
- On macOS, sign-in tokens live in the login keychain, and the throttle reads the real device: CPU, power, thermal state, Low Power Mode, memory, keyboard and pointer.

### Changed

- A fresh install syncs nothing and creates no folder until you set the two folders. `localRoot` and `cloudRoot` have no default; `vapor status` says `unconfigured` and names the command to run.
- Sync no longer stops on battery, while you use the device, or in Low Power Mode. Those slow it down. Only a device that is critically hot pauses it.
- A scan of both folders runs in every throttle state but `suspended`, and a full one runs once a day.
- The CPU ceiling is a measured limit: while Vapor is above it, it gives up one worker each second.
- One naming scheme: camelCase keys and kebab-case values in `vapor.json` and in every `--json` output, and single-word commands. `vapor reconcile` replaces `flush-now` and `sync-now`.
- The device name in a conflict copy is the host name plus four random characters, so two devices with one host name write different copies.
- Before GA the state database has no migrations. A database of another version is set aside, and Vapor rebuilds its index from the two folders.
- Each runtime directory has its own secrets, the daemon never opens a keychain dialog, and `vapor auth login` takes a token on stdin only (`--token -`).
- `vapor trash empty` asks first, or takes `--yes`. `vapor service` no longer takes `--user` or `--system`.
- A negation under an ignored folder follows git: it does not show the file again. A rule that ends in `/` matches folders only.
- A crash inside one profile stops that profile only. Vapor tries it again after 1, 5, and 30 minutes.
- The Mac app is built for Apple silicon. There is no Intel Mac build.
- `vapor pause`, `resume`, and `reconcile` take `--profile`, and `reconcile` tries at once the uploads that wait for room in the cloud. Profiles whose folders would lose or multiply files next to each other are refused, and a cloud folder that was swapped never removes a local file.

### Fixed

- A rename that changes only the case of a name no longer removes the file from the cloud.
- After a conflict with a slow download, the version that won is no longer deleted from the cloud.
- A file removed and put back within seconds stays in the cloud, and a file put back with an old date is not deleted again.
- A file that looks deleted because its sync folder was swapped is not removed from the cloud.
- A transfer that waits for bandwidth no longer uses a full CPU core.
- A folder that had a burst of changes keeps syncing on battery and while you type.
- A broken `vapor.json` no longer makes the daemon sync a default folder.
- Google Drive: an upload over 5 MiB records its checksum, so the next edit of that file can upload (built from the API documents, not yet checked against a live account). Two files with one name no longer take turns. A full Drive holds uploads and lets downloads go on.
- After an update of a prerelease, the daemon asks for a new sign-in. It no longer waits on a keychain dialog that nobody sees.
- In `push-only`, a name that the cloud respells gets this device's spelling back.
- A file that left both folders while Vapor was not running, and that you put back later, is no longer moved to the trash.
- A folder moved in with a `.gitignore` of its own no longer uploads the files that those rules hide.
- A file named `meta.json` that Vapor removed can be listed and restored from the trash.
- A sync folder that is read-only syncs. Before, it was read as a replaced folder after the first check.
- Google Drive: two uploads at once into a new folder no longer make that folder twice, and a file whose name differs only in capitals, or a Google Doc of the same name, is no longer taken for the file at a path (built from the API documents, not yet checked against a live account).
- A command run in a test or development directory can no longer read or remove the sign-in of the real install.
- A long changelog section no longer fails a release: the release body is cut at GitHub's limit and links the changelog.
- The many sync, command-line, app, and IPC fixes of the earlier reviews in this cycle. The git history of this file has each one.

### Docs

- `docs/specs/2026-11-14-full-review.md` records the decisions of the full review and the choices that wait for the owner.
- The task lists hold open work only, and each performance threshold says whether a tool measures it.

### Tooling / CI

- Lint checks the dependencies (`cargo deny`: advisories, licenses, sources). Its first run found advisories in rustls, rustls-webpki, webpki-roots, and the SQLite that rusqlite bundles; all four are updated.
- The Swift lint can fail now, the Tier 1 time budget does not count the compiler, and lint refuses an em dash and a locale catalog that does not parse.
- `scripts/version.sh` refuses a version that does not sort above the current one. The release gate checks the format and changes no file. The assets of a published release are not replaced.
- Tier E2E (`./scripts/e2e.sh`) drives the real binaries in sandboxes on every pull request. Tier S (`./scripts/soak.sh`) is hours of seeded file churn with a no-loss check, as a release gate.
- The release pipeline packages each platform under its own protected environment. Actions are pinned and allowlisted, and `main` is protected by a ruleset.
- The pre-commit hook runs lint and Tier 1.
- Tier VM (`./scripts/vm.sh`) installs the packaged app in a macOS virtual machine and acts on it as a user does, with screenshots of each scenario. The controls in the windows of the Mac app have accessibility identifiers, which the tier finds them by.
- A manual Tier VM guest can run next to the app scenarios (`--guest --second`) and can have the Internet to test a real cloud provider (`--guest --internet`). `tools/vm/workloads/` holds the workloads of a heavy-user session, and `url` is updated for RUSTSEC-2026-0592.

## [0.2.0-alpha.3] - 2026-03-10

### Changed

- Simplified GitHub Actions checkout credential handling so release preflight uses the default authenticated checkout session for tag ancestry fetches.
- Added contributor policy requiring concise `CHANGELOG.md` `Unreleased` notes before non-trivial commits.

## [0.2.0-alpha.2] - 2026-03-10

### Changed

- Hardened `lint`, `test`, `perf`, and `release` GitHub Actions workflows with explicit least-privilege permissions.
- GitHub Releases are now documented as the direct installation source for Vapor app builds.

### Fixed

- Release preflight now uses the default authenticated checkout session for its `main` ancestry fetch so tag-triggered releases can complete reliably.
- CI signing and notarization now run behind the protected GitHub `release` environment, verify the imported `Developer ID Application` identity, and pass the temporary keychain into `notarytool` explicitly.
- `./scripts/version.sh` now refreshes `Cargo.lock` through Cargo and keeps workspace package versions aligned with `VERSION` during release preparation.

## [0.2.0-alpha.1] - 2026-03-10

### Added

- Scoped local/cloud sync directory handling, including safe local root creation and strict sync-root boundaries.
- Gitignore-style filtering with recursive `.gitignore` and `.vaporignore` support plus pre/post user ignore rule layers.
- Locale catalogs with deterministic English fallback for app UI text.
- GitHub release automation with lint/test/perf-gated packaging, draft GitHub Releases, checksums, and release runbooks.
- Centralized `VERSION`-driven app + daemon versioning with build commit provenance surfaced in diagnostics and `vapord --version`.

### Changed

- Shared runtime/config constants are centralized across Swift and Rust so script, app, and daemon defaults stay aligned.
- Release preparation now goes through `./scripts/version.sh`, which creates the release commit and matching tag together.

## [0.1.0] - 2026-03-08

### Added

- Initial project foundation for Vapor app and `vapord` daemon.
- Script-first build, lint, test, and packaging workflows.
- Baseline CI workflows for lint and test on `main` and pull requests.

[Unreleased]: https://github.com/neoxelox/vapor/compare/v0.2.0-alpha.3...HEAD
[0.2.0-alpha.3]: https://github.com/neoxelox/vapor/releases/tag/v0.2.0-alpha.3
[0.2.0-alpha.2]: https://github.com/neoxelox/vapor/releases/tag/v0.2.0-alpha.2
[0.2.0-alpha.1]: https://github.com/neoxelox/vapor/releases/tag/v0.2.0-alpha.1
[0.1.0]: https://github.com/neoxelox/vapor/releases/tag/v0.1.0
