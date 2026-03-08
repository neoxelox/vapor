# apps/macos

SwiftUI macOS application surface for `vapor`.

Responsibilities:

- onboarding and settings UX
- menubar status and controls
- provider auth orchestration UI
- Keychain integration
- daemon lifecycle and auto-launch controls
- sync filtering preferences (for example `.gitignore` ingestion)
- native app lifecycle and script-first distribution packaging

Current implementation notes:

- `VaporCore` includes `DaemonLifecycleManager` for default auto-launch policy, toggle semantics, crash-loop relaunch backoff, and login-item registration.
- `VaporCore` includes `AppLifecycleCoordinator` for app-window/menubar lifecycle actions (Dock presence, daemon stop on quit).
- `VaporCore` includes `VaporConfigurationStore` + `VaporPaths` for runtime directory resolution and `vapor.json` persistence.
- `VaporCore` includes `VaporLocalizationStore` for JSON-catalog UI copy lookup with device-language selection and English fallback.
- Locale source-of-truth catalogs live in `assets/locales/*.json` and are synced by scripts into `Sources/VaporCore/Resources/locales/*.json` before Swift build/test/package.
- `VaporCore` includes a concrete `LaunchAgentController` that writes `~/Library/LaunchAgents/<label>.plist` and manages lifecycle with `launchctl`.
- `AppShellViewModel` uses lifecycle defaults backed by `LaunchAgentController` and `SMAppService.mainApp` integration to restore Vapor at login in menubar-only mode.
- Settings/config surface includes `useGitIgnore`, `useVaporIgnore`, `syncDirectories`, `preIgnoreRules`, and `postIgnoreRules`, persisted in `vapor.json` and exported to daemon launch env as `VAPOR_USE_GITIGNORE`, `VAPOR_USE_VAPORIGNORE`, `VAPOR_SYNC_DIRECTORIES`, `VAPOR_PRE_IGNORE_RULES`, and `VAPOR_POST_IGNORE_RULES`.
- Settings/config surface includes `preferredLanguageCode` to override UI language selection; when unset or unavailable, app copy falls back to device language then English.
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
  - `Open Vapor` should focus the existing main window when already open (no duplicate windows).
- Login startup:
  - Restores Vapor as menubar-only (no automatic main window presentation).
- Daemon (`vapord`):
  - Must remain running when the main window is closed.
  - Full daemon shutdown should happen only on explicit quit/stop flows, not on window close.

Implementation phases map to `docs/plans/vapor-macos-task-list.md`.

## Local dev

- Build: `swift build --package-path apps/macos`
- Test: `swift test --package-path apps/macos`
- Runtime root override: `VAPOR_DIR=/path/to/vapor swift run --package-path apps/macos Vapor`

## Logging

- Runtime root is `VAPOR_DIR` (`~/.vapor` by default, `./.vapor` under repo scripts/tests).
- App configuration file: `<vapor_dir>/vapor.json`
- Structured app logs: `<vapor_dir>/logs/vapor.logs`
- Reserved state/db location: `<vapor_dir>/state/vapor.sqlite`
- Log line format: `{timestamp} [{level}] ({component}): {message}. key=value ...`
- Runtime log level override: `VAPOR_LOG_LEVEL` (`debug`, `info`, `warning`, `error`)
- Default log level:
  - `VAPOR_ENV=dev`: `debug`
  - `VAPOR_ENV=prod` (or unset): `info`

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
2. Run packaging with a notary profile (presence of `VAPOR_NOTARY_PROFILE` enables notarization):
   - `VAPOR_SIGN_IDENTITY="Developer ID Application: ..." VAPOR_NOTARY_PROFILE="<profile>" apps/macos/scripts/package.sh`

## Xcode debugging convenience

- Open `apps/macos/Package.swift` in Xcode and run the `Vapor` executable target.
- Xcode support is optional; distribution remains script-first.
