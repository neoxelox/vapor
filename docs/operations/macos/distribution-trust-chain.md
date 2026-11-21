# Distribution Trust Chain: macOS

Cross-platform distribution policy lives in
`docs/operations/distribution-trust-chain.md` (which delegates to this file
and its sibling per-platform docs). This document covers only the macOS
artifacts.

## Scope

Define release trust requirements for the macOS installer and the app,
the daemon, and the CLI that it holds.

## Required controls

- Code signing for the app bundle and every bundled executable
  (app, daemon, CLI), with the Developer ID Application certificate.
- Code signing for the installer, with the Developer ID Installer
  certificate.
- Hardened runtime enabled for distributable binaries.
- Notarization for the app and for the installer.
- Entitlement review for least-privilege access.

## Release pipeline policy

1. Build signed artifacts for app (`Vapor`), daemon (`vapord`), and
   CLI (`vapor`).
2. Validate signatures and entitlements.
3. Submit for notarization and read Apple's answer. `notarytool submit
   --wait` exits 0 for a refused submission, so `package.sh` reads the
   status, and on anything but `Accepted` it prints the notarization
   log and fails.
4. Staple the ticket, then check the stapled app: `stapler validate`,
   and `spctl -a -t exec`, the question Gatekeeper asks at first
   launch. Either failure fails the package.
5. Build the installer from the stapled app, sign it, and take it
   through the same notarization and stapling. Check it with `stapler
   validate` and `spctl -a -t install`, the question Gatekeeper asks
   when the user opens it.
6. Publish only notarization-passing artifacts.

The app and the installer are signed together or not at all:
`package.sh` refuses one identity without the other. With neither, both
are unsigned, which is what a prerelease ships. The signed path has not
run yet: no signing material exists (`docs/tasks/macos.md`).

The app is built for Apple silicon. There is no Intel Mac build.

Implementation requirements:

- App packaging pipeline is script-first (`apps/macos/scripts/package.sh`)
  and CI-runnable.
- Pipeline must produce `dist/Vapor.app` and the installer
  `dist/Vapor-macos-arm64.pkg` without requiring Xcode archive UI
  flows. The installer is the release asset.
- The installer puts `Vapor.app` in `/Applications`. Its bundle is not
  relocatable, so Installer never moves the install to another copy of
  the app that the Mac holds, and it carries no version check, so the
  package a user opens is the version the user gets. Its distribution
  refuses an Intel Mac and a macOS below the minimum.
- The installer's `postinstall` script links `/usr/local/bin/vapor` to
  `Contents/Helpers/vapor` in the installed app, so the CLI is on the
  `PATH` and an update of the app updates it. It replaces a link of an
  earlier install and a link that leads nowhere; another program with
  that name stays, and the install still succeeds. The script is the
  one piece of the release that runs as root on a user's Mac: keep it
  to the link, and test a change with `scripts/tests/installer.sh`.
- `dist/Vapor.app` must include three executables:
  - `Contents/MacOS/Vapor`
  - `Contents/MacOS/vapord`
  - `Contents/Helpers/vapor` (the CLI; kept out of `Contents/MacOS/`
    because the default macOS filesystem is case-insensitive and
    `vapor` would collide with `Vapor`)
- Runtime daemon launch path must be the bundled binary
  (`Contents/MacOS/vapord`) only; the bundled CLI resolves it first as
  a sibling, then at `../MacOS/vapord`. Started through the link on the
  `PATH`, the CLI follows the link first, so it finds the daemon in the
  app and a service definition pins the binary, not the link.
- Xcode project/workspace support remains optional debugging convenience
  only.

## Secrets management

Signing identities, notarization profile, and keychain access live in the
GitHub Environment `release-macos` (not repository-wide secrets), next
to the Google Drive OAuth client (`VAPOR_GDRIVE_CLIENT_ID`,
`VAPOR_GDRIVE_CLIENT_SECRET`) that the package job compiles into
`vapor` and `vapord`. See `AGENTS.md §7` for the general policy and
`../provider-auth-operations.md` for the client.

## Validation checklist

- App, daemon, and installer signatures are valid on clean host.
- An install on a clean host leaves the app in `/Applications` and
  `vapor` on the `PATH` (`apps/macos/scripts/verify-package.sh
  --install`, which the `build` and `release` workflows run on their
  runners).
- LaunchAgent/login item behavior is stable across install/upgrade.
- Entitlements are reviewed for drift each release.
- Rollback artifacts are preserved and verifiable.

## Ownership and updates

- Owner: release engineering (project owner until a dedicated owner
  exists).
- Update cadence: each release cycle and any signing/notarization incident.
