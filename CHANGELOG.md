# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) with pre-GA policy details defined in `docs/operations/release-process.md`.

## [Unreleased]

### Added

- Vapor settings now expose editable baseline and override ignore-rule lists, persisting them for daemon filtering on the next launch.
- Daemon event/intent bookkeeping now stays bounded in memory by compacting over-cap subtrees into a single reconcile intent instead of letting callback path growth run unbounded.
- Daemon debounce/coalescing now runs on a 250ms tick with conservative quiet windows so config edits settle faster while lockfiles and other unmatched paths wait longer before stabilization.
- Daemon scheduling now keeps one latest intent per path, supersedes stale actions, and requeues dirty paths when new changes arrive during in-flight work.
- Daemon throttle control now evaluates 1s power, thermal, load, disk, network, and activity samples to choose `IdleDrain`, `Light`, `Throttled`, or `Suspended` with deterministic worker caps.
- Daemon planner, hash, upload, and reconcile stages now acquire strict throttle-gated work permits so new work cannot exceed the active state's caps.
- Daemon startup now initializes a SQLite durable queue/state DB, recovers leased intents after restart, and persists queue/state metadata with explicit schema versioning.
- Daemon retry scheduling now applies exponential backoff with deterministic jitter, persists the longest rate-limit slowdown window across restarts, and durably finalizes terminal failures.
- Daemon storm detection now converts noisy subtrees into deferred reconcile markers once per-directory or global burst thresholds trip, keeping callback-side path growth bounded earlier.
- Daemon tests now include micro-regression guards for filesystem callback bursts, debounce/coalescing ticks, and scheduler superseding hot paths.
- Daemon reconcile control now starts only in `IdleDrain`, yields on slice expiry or throttle changes, and clears compacted subtree boundaries after successful quiet completion.
- Daemon tests now stress large per-subtree, global-cap, and multi-subtree storm scenarios so bounded memory/backpressure behavior stays covered under heavy pending-intent bursts.
- Daemon startup now composes a real runtime loop that advances watcher ingest, debounce, scheduler draining, durable queueing, throttle-gated work, and idle-biased reconcile progression on each tick.
- Daemon restart recovery now inserts a prioritized whole-scope reconcile and re-prioritizes any existing root reconcile so volatile pre-DB intent loss is reconstructed conservatively before older durable work resumes.
- Daemon callback scope checks now canonicalize the watch root and reject traversal or symlink-escape paths before they can enter bounded ingest state.

### Changed

- Persisted app config now uses `autoLaunch` and `languageCode`, with English as the default UI language when no other catalog is selected.
- App startup now preserves malformed `vapor.json` files in place, surfaces the load failure in the UI, and avoids silently overwriting broken config with defaults.
- Ignore toggles and saved ignore rules now refresh the in-memory daemon launch configuration immediately so future lifecycle actions stay aligned with persisted settings.
- Placeholder sync-state cycling controls have been removed from the app and menubar until real daemon-backed pause/flush actions exist.
- Runtime path handling now normalizes `VAPOR_DIR`, applies restrictive local permissions to config/log/state artifacts, and redacts sensitive log metadata without panicking on log-file open failure.
- Durable daemon state now redacts and bounds persisted error text, rejects oversized counters or state values, and guards against out-of-range persisted timestamps.
- Pre-GA daemon state now rejects older on-disk schemas instead of carrying forward compatibility shims, and removes an obsolete deferred-reconcile helper API.
- Daemon startup now injects the selected provider through the provider trait boundary instead of hardcoding the Google Drive type inside core daemon orchestration.
- Daemon runtime now advances durable non-reconcile work through bounded planner, hash, and upload stages under throttle/workgate caps instead of processing one leased intent at a time.
- Roadmap now introduces a Phase 3 local filesystem reference provider that exercises every provider-neutral bidirectional mechanic against a loopback backing store, and defers Google Drive integration to Phase 9 so later runtime, safety, profile, XPC, auto-tuning, and provider-extensibility work stabilizes against the reference provider first.
- Roadmap now specifies user-configurable `resourceLimits` (CPU/memory/bandwidth hard ceilings) and `idleBoost` (dynamic headroom expansion when the device is genuinely idle) as a layer over the internal throttle controller, with profile overrides resolving by MIN-lowering, enforcement at the workgate/bandwidth-shaper/memory-compaction layers, and diagnostics surfacing effective ceilings and boost reason codes.
- Design spec now pins deterministic idle-boost / throttle transition behavior (snap-down on `IdleDrain` exit, fresh up-ramp on return, no auto-resume from stale conditions, in-flight work yields at next slice checkpoint), a concrete bidirectional conflict-suffix template (`{stem}~conflict-{deviceId}-{timestampMs}{ext}` with collision-avoidance fallback and "data preservation wins over deletion"), self-write cache constants and xattr/side-file precedence with memory-pressure floors, multi-profile watcher coordination (one watcher per canonical realpath, per-profile queues, shared workgate, panic-contained per-profile runtimes), XPC version-skew rules (`|N - M| <= 1`, typed default-on-unknown-field, payload-size bounds), LaunchAgent plist policy (`KeepAlive=false` with in-process crash-loop backoff) and validation scenarios, SLO applicability under lowered user ceilings, a Phase 2.5 simulator removal checklist blocking Phase 4, a happy-path bidirectional race smoke test in Phase 3, and per-intent "why stuck" diagnostics.

### Fixed

- Packaged `Vapor.app` builds now include the SwiftPM localization resource bundle and no longer crash on launch while the app shell resolves UI copy catalogs.
- Packaging now fails fast if either bundled executable is missing, and runtime daemon resolution stays pinned to the bundled `Contents/MacOS/vapord` sibling binary.
- GitHub Actions macOS workflows now run on `macos-26`, matching Vapor's macOS 26-only app target so SwiftUI app tests load against a supported runtime.

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
