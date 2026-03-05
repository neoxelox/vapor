# apps/macos

SwiftUI macOS application surface for `vapor`.

Responsibilities:

- onboarding and settings UX
- menubar status and controls
- provider auth orchestration UI
- Keychain integration
- daemon lifecycle and auto-launch controls
- native app lifecycle and script-first distribution packaging

Current implementation notes:

- `VaporCore` includes `DaemonLifecycleManager` for default auto-launch policy, toggle semantics, crash-loop relaunch backoff, and optional login-item registration.
- `VaporCore` includes `AppLifecycleCoordinator` for app-window/menubar lifecycle actions (Dock presence, daemon stop on quit).
- `VaporCore` includes a concrete `LaunchAgentController` that writes `~/Library/LaunchAgents/<label>.plist` and manages lifecycle with `launchctl`.
- `AppShellViewModel` uses lifecycle defaults backed by `LaunchAgentController` and can optionally enable `SMAppService` login-item integration when `VAPOR_LOGIN_ITEM_IDENTIFIER` is set.
- Startup performs daemon lifecycle bootstrap asynchronously so app window launch stays responsive.
- Menubar provides explicit lifecycle controls: `Open Vapor` restores Dock/window surface and `Quit Vapor` requests daemon stop before app termination.
- Distribution artifacts are produced by `apps/macos/scripts/package.sh` (source of truth for app packaging, signing, and optional notarization).
- Bundle identifier baseline is `sh.arn.vapor`.
- Icon source of truth is `assets/icon.png` (1024x1024).

## App component model and lifecycle semantics

- Main window and menubar are separate app surfaces with different lifecycle responsibilities.
- Main window:
  - Hosts full UI (`ContentView`, settings, diagnostics).
  - Closing window should fully close UI and remove Dock presence.
- Menubar (`MenuBarExtra`):
  - Remains available after main window closes.
  - Shows status/control actions, including reopen (`Open Vapor`) and full quit (`Quit Vapor`).
- Daemon (`vapord`):
  - Must remain running when the main window is closed.
  - Full daemon shutdown should happen only on explicit quit/stop flows, not on window close.

Implementation phases map to `docs/plans/vapor-macos-task-list.md`.

## Local dev

- Build: `swift build --package-path apps/macos`
- Test: `swift test --package-path apps/macos`

## Logging

- Structured app logs: `~/Library/Logs/Vapor/vapor.log`
- Test script logs: `.vapor/logs/vapor.logs`
- Log line format: `{timestamp} [{level}] ({component}): {message}. key=value ...`
- Runtime log level override: `VAPOR_LOG_LEVEL` (`debug`, `info`, `warning`, `error`)
- Default log level:
  - local/debug build flows: `debug`
  - package flow (`apps/macos/scripts/package.sh`): `warning`

## Package app (local)

- Direct packaging: `apps/macos/scripts/package.sh`
- Via repo build: `./scripts/build.sh package`
- Launch packaged app: `open dist/Vapor.app`

## Signed distribution

- Set signing identity: `VAPOR_SIGN_IDENTITY="Developer ID Application: ..."`
- Package: `VAPOR_SIGN_IDENTITY="Developer ID Application: ..." apps/macos/scripts/package.sh`

Optional entitlements:

- `VAPOR_ENTITLEMENTS=apps/macos/Entitlements.plist`

## Notarized distribution

1. Create a keychain profile with `xcrun notarytool store-credentials`.
2. Run packaging with notarization enabled:
   - `VAPOR_SIGN_IDENTITY="Developer ID Application: ..." VAPOR_NOTARIZE=1 VAPOR_NOTARY_PROFILE="<profile>" apps/macos/scripts/package.sh`

## Xcode debugging convenience

- Open `apps/macos/Package.swift` in Xcode and run the `Vapor` executable target.
- Xcode support is optional; distribution remains script-first.
