# Distribution Trust Chain Plan

## Scope

Define release trust requirements for macOS app + daemon distribution.

## Required controls

- Code signing for app bundle and daemon executable.
- Hardened runtime enabled for distributable binaries.
- Notarization for release artifacts.
- Entitlement review for least-privilege access.

## Release pipeline policy

1. Build signed artifacts for app and daemon.
2. Validate signatures and entitlements.
3. Submit for notarization and verify staple status.
4. Publish only notarization-passing artifacts.

Implementation requirements:

- App packaging pipeline is script-first (`apps/macos/scripts/package-app.sh`) and CI-runnable.
- Pipeline must produce `dist/Vapor.app` and zip artifacts without requiring Xcode Archive UI flows.
- Xcode project/workspace support remains optional debugging convenience only.

## Validation checklist

- App and daemon signatures are valid on clean host.
- LaunchAgent/login item behavior is stable across install/upgrade.
- Entitlements are reviewed for drift each release.
- Rollback artifacts are preserved and verifiable.

## Ownership and updates

- Owner: release engineering (project owner until dedicated owner exists).
- Update cadence: each release cycle and any signing/notarization incident.
