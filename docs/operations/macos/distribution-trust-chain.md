# Distribution Trust Chain — macOS

Cross-platform distribution policy lives in
`docs/operations/distribution-trust-chain.md` (which delegates to this file
and its sibling per-platform docs). This document covers only the macOS
artifacts.

## Scope

Define release trust requirements for macOS app + daemon distribution.

## Required controls

- Code signing for app bundle and daemon executable.
- Hardened runtime enabled for distributable binaries.
- Notarization for release artifacts.
- Entitlement review for least-privilege access.

## Release pipeline policy

1. Build signed artifacts for app (`Vapor`) and daemon (`vapord`).
2. Validate signatures and entitlements.
3. Submit for notarization and verify staple status.
4. Publish only notarization-passing artifacts.

Implementation requirements:

- App packaging pipeline is script-first (`apps/macos/scripts/package.sh`)
  and CI-runnable.
- Pipeline must produce `dist/Vapor.app` and zip artifacts without
  requiring Xcode archive UI flows.
- `dist/Vapor.app` must include both executables in `Contents/MacOS/`:
  - `Vapor`
  - `vapord`
- Runtime daemon launch path must be the bundled sibling binary
  (`Contents/MacOS/vapord`) only.
- Xcode project/workspace support remains optional debugging convenience
  only.

## Secrets management

Signing identities, notarization profile, and keychain access live in the
GitHub Environment `release-macos` (not repository-wide secrets). See
`AGENTS.md §7` for the general policy.

## Validation checklist

- App and daemon signatures are valid on clean host.
- LaunchAgent/login item behavior is stable across install/upgrade.
- Entitlements are reviewed for drift each release.
- Rollback artifacts are preserved and verifiable.

## Ownership and updates

- Owner: release engineering (project owner until a dedicated owner
  exists).
- Update cadence: each release cycle and any signing/notarization incident.
