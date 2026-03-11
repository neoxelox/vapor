# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) with pre-GA policy details defined in `docs/operations/release-process.md`.

## [Unreleased]

### Added

- Vapor settings now expose editable baseline and override ignore-rule lists, persisting them for daemon filtering on the next launch.
- Daemon event/intent bookkeeping now stays bounded in memory by compacting over-cap subtrees into a single reconcile intent instead of letting callback path growth run unbounded.
- Daemon debounce/coalescing now runs on a 250ms tick with conservative quiet windows so config edits settle faster while lockfiles and other unmatched paths wait longer before stabilization.
- Daemon scheduling now keeps one latest intent per path, supersedes stale actions, and requeues dirty paths when new changes arrive during in-flight work.

### Changed

- Persisted app config now uses `autoLaunch` and `languageCode`, with English as the default UI language when no other catalog is selected.

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
