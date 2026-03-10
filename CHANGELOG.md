# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) with pre-GA policy details defined in `docs/operations/release-process.md`.

## [Unreleased]

### Added

- GitHub release automation policy, workflow, and operational runbooks.
- Centralized `VERSION`-driven app + daemon versioning with synced Cargo metadata and build commit provenance.

## [0.1.0] - 2026-03-08

### Added

- Initial project foundation for Vapor app and `vapord` daemon.
- Script-first build, lint, test, and packaging workflows.
- Baseline CI workflows for lint and test on `main` and pull requests.

[Unreleased]: https://github.com/neoxelox/vapor/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/neoxelox/vapor/releases/tag/v0.1.0
