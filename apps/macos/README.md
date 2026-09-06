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

- Daemon lifecycle is delegated to the Rust core: the app invokes the bundled `vapor` CLI (`Contents/Helpers/vapor`), and all lifecycle policy — autolaunch persistence, crash-loop backoff and pause, supervision — lives in `core/lifecycle` behind `vapor service`.
- `VaporCore` includes `DaemonLifecycleManager` as a thin facade over the `LaunchAgentControlling` seam: it owns only serialization (one lifecycle operation at a time) and login-item registration; zero lifecycle policy lives in Swift.
- `VaporCore` includes `VaporCLIServiceController`, the default `LaunchAgentControlling` implementation: it runs `vapor service <cmd> --json` as a subprocess and decodes the stable JSON contract rendered by `core/cli/src/commands/service.rs`.
- `VaporCore` includes `DaemonHealthMonitor`, a 30-second timer (`VaporConstants.Daemon.healthTickIntervalSeconds`) that runs `vapor service check` each tick so unexpected daemon exits are detected and routed through the Rust crash-loop guard.
- `VaporCore` includes `AppLifecycleCoordinator` for app-window/menubar lifecycle actions (Dock presence, daemon stop on quit).
- `VaporCore` includes `VaporConfigurationStore` + `VaporPaths` for runtime directory resolution and `vapor.json` persistence.
- `VaporCore` includes `VaporLocalizationStore` for JSON-catalog UI copy lookup with device-language selection and English fallback.
- Locale source-of-truth catalogs live in `assets/locales/*.json` and are synced by scripts into `Sources/VaporCore/Resources/locales/*.json` before Swift build/test/package.
- The LaunchAgent plist and `launchctl` interaction are owned by the Rust `NativeServiceInstaller` (`core/platform`), reached through `vapor service`; no Swift code writes the plist.
- `AppShellViewModel` uses lifecycle defaults backed by `VaporCLIServiceController` and `SMAppService.mainApp` integration to restore Vapor at login in menubar-only mode.
- Settings/config surface includes `useGitIgnore`, `useVaporIgnore`, `localSyncDirectory`, `cloudSyncDirectory`, `preIgnoreRules`, and `postIgnoreRules`, persisted in `vapor.json`. The running daemon applies ignore toggles and rules within a few seconds; roots, provider and profile changes need a restart, which the daemon reports in status and the app offers as "Restart sync" (the LaunchAgent environment carries only `VAPOR_DIR` + `VAPOR_ENV` pass-through, with `VAPOR_*` variables remaining per-field overrides).
- Daemon startup ensures the configured local sync root exists on the device and the cloud root exists provider-side (creating either when missing) before normal sync flow.
- The 30-second health tick also reads `vapor status --json` when the daemon is running, so the Dashboard and menu bar show the daemon's real state (idle, syncing, throttled, suspended, paused, stopped, error) with its throttle reason, instead of a static label.
- Settings/config surface includes `languageCode`, which defaults UI copy to English and falls back to English again if a requested catalog is unavailable.
- Malformed `vapor.json` is preserved in place and surfaced as an actionable app diagnostic; Vapor uses in-memory defaults until the file is fixed or replaced.
- Runtime/config/log/state paths now use restrictive local permissions, and app logging shares the same centralized redaction rules used for sensitive metadata.
- Startup performs daemon lifecycle bootstrap asynchronously so app window launch stays responsive.
- Menubar provides only real lifecycle controls: `Open Vapor` restores Dock/window surface, auto-launch toggle updates persisted state, and `Quit Vapor` requests daemon stop before app termination.
- Distribution artifacts are produced by `apps/macos/scripts/package.sh` (source of truth for app packaging, signing, and optional notarization).
- `Vapor.app` bundles three executables — `Contents/MacOS/Vapor` (app), `Contents/MacOS/vapord` (daemon), and `Contents/Helpers/vapor` (CLI); `package.sh` builds, copies, signs, and asserts all three. The CLI lives in `Contents/Helpers/` because the default macOS filesystem is case-insensitive, so `vapor` cannot sit next to `Vapor`; the CLI resolves `vapord` first as a sibling, then at `../MacOS/vapord`.
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

macOS-specific implementation phases map to `docs/tasks/macos.md`. The
underlying runtime phases (core engine, platform abstractions, lifecycle
migration, CLI) live in `docs/tasks/core.md` and `docs/tasks/cli.md`.

## Testing

Logic tests only. Configuration parsing, lifecycle coordinator state
transitions, view-model state mapping, localization fallback, logger
redaction, paths, bundle layout are all covered in
`apps/macos/Tests/VaporCoreTests/` and `Tests/VaporAppTests/`. **No
SwiftUI view rendering tests, no menubar layout tests, no Dock
transition tests, no keyboard-focus tests** — UI correctness is
verified by the project owner manually. Policy: `AGENTS.md §9`; full
rationale: `docs/architecture/testing-strategy.md`.

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
