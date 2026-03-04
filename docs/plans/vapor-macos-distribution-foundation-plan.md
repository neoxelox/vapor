# vapor macOS distribution foundation plan

This plan establishes a script-first packaging foundation so `Vapor` ships as a native
macOS app bundle (`Vapor.app`) while keeping Xcode optional for day-to-day debugging.

## Non-negotiables

- Target latest macOS only (`26.0`).
- Build a native SwiftUI `.app` experience (Dock, menu bar, window, no Terminal dependency).
- Distribution must be fully script/CI driven; no required Xcode Archive/Organizer flow.
- Xcode project/workspace support is optional convenience only.

## Priority 1 - SwiftUI app entry and GUI lifecycle

- Ensure executable target has a true SwiftUI app entry point (`@main struct VaporApp: App`).
- Ensure starter UI looks native by default (window structure + toolbar/menu command).
- Remove/avoid conflicting CLI-style `main.swift` or top-level executable entrypoints.
- Move startup orchestration into callable app-core functions, invoked from app lifecycle hooks.

Acceptance checks:

- `swift build --package-path apps/macos -c release` succeeds.
- App launch validation is done via `.app` bundle launch, not direct terminal execution.

## Priority 2 - Deterministic packaging pipeline (`dist/Vapor.app`)

Create `apps/macos/scripts/package-app.sh` with deterministic, ordered steps:

1. Build release binary via SwiftPM.
2. Locate expected binary output (`apps/macos/.build/release/Vapor`).
3. Create app bundle skeleton in `dist/Vapor.app/Contents/{MacOS,Resources}`.
4. Copy executable into bundle and ensure execute permissions.
5. Generate `Info.plist` with required bundle metadata.
6. Build `AppIcon.icns` from `assets` PNG using `sips` + `iconutil`.
7. Copy optional static resources from `apps/macos/Resources`.
8. Sign app (ad-hoc by default; Developer ID + hardened runtime when configured).
9. Validate plist and signature sanity (`plutil`, `codesign`, `spctl` best-effort).
10. Produce `dist/Vapor.zip` with `ditto --keepParent`.
11. Optionally notarize + staple + re-zip when notarization toggle is enabled.

Required environment inputs:

- `APP_NAME` (default `Vapor`)
- `EXECUTABLE_NAME` (default `Vapor`)
- `BUNDLE_ID` (default `so.latitude.vapor`)
- `MIN_MACOS` (default `26.0`)
- `ICON_PNG` (default `apps/macos/Assets/AppIcon.png`)
- `DIST_DIR` (default `dist`)
- `VAPOR_SIGN_IDENTITY` (optional)
- `VAPOR_ENTITLEMENTS` (optional)
- `VAPOR_NOTARIZE` (optional toggle)
- `VAPOR_NOTARY_PROFILE` (required if notarization toggle is on)

Versioning policy:

- `CFBundleShortVersionString` from latest git tag (`git describe --tags --abbrev=0`) fallback `0.1.0`.
- `CFBundleVersion` from commit count (`git rev-list --count HEAD`) or CI build number override.

Icon source note:

- Repository icon source is stored at `apps/macos/Assets/AppIcon.icon` (Icon Composer format).
- Packaging flow should consume `ICON_PNG` and fail with a clear message when missing, instructing export of a 1024x1024 PNG from the Icon Composer source.

## Priority 3 - Build-script and CI integration

- Keep existing build-only mode for fast local loops.
- Add `VAPOR_PACKAGE_APP=1` flow so release build script can invoke packaging script.
- Ensure packaging path is non-interactive and CI-friendly.
- Fail fast with clear errors if notarization is requested without required credentials/profile.

## Priority 4 - Xcode convenience (optional)

- Support opening `apps/macos/Package.swift` in Xcode for debugging.
- Optional `.xcworkspace` helper is allowed, but not required.
- Distribution source of truth remains scripts (`package-app.sh`, signing, notarization).

## Priority 5 - Developer documentation

Document in `apps/macos/README.md`:

- Local Swift build commands.
- App packaging command(s).
- Signed distribution usage (`VAPOR_SIGN_IDENTITY`).
- Notarized distribution usage (`VAPOR_NOTARIZE`, `VAPOR_NOTARY_PROFILE`).

## Final acceptance criteria

1. `Vapor` launches as a SwiftUI-native app via `open dist/Vapor.app` with no Terminal window.
2. `apps/macos/scripts/package-app.sh` produces both `dist/Vapor.app` and `dist/Vapor.zip`.
3. Scripted signing/notarization path works when corresponding env vars are provided.
4. Xcode debugging remains optional and does not gate distribution.
