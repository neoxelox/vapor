# macOS App Lifecycle

## Core architecture

- SwiftUI app
  - Onboarding, provider auth, root selection, settings, diagnostics, menubar state.
  - Auto-launch toggle and daemon control surface.
- Rust daemon (`core/daemon`, LaunchAgent)
  - FSEvents ingestion, debounce/coalescing, scheduler, throttle controller.
  - Durable queue/state, retries, deferred reconcile, provider execution.
- Provider modules (`core/providers`)
  - Pre-GA default is `FilesystemStubProvider` (inert; reports no remote-changes-feed and no server-side-rename).
  - `provider_filesystem` (loopback local) is the Phase 3 reference provider used to validate every provider-neutral mechanic before any external provider ships.
  - `provider_gdrive` is deferred to Phase 9 and integrates on top of the runtime already validated against the filesystem provider.
  - Additional adapters (for example iCloud, S3, R2, Proton Drive) come through Phase 8 extensibility hardening and are not part of the first release.
- XPC boundary
  - Typed status/control API between app and daemon with shared contracts in `core/shared`. Version skew, payload bounds, and field-omission tolerance live in `docs/architecture/xpc-contracts.md`.

## Runtime components

- Main app window (`Window` single-instance scene)
  - Primary configuration and diagnostics UI.
  - Dock-visible while the window is open.
- Menubar component (`MenuBarExtra`)
  - Always-on quick status and control surface while app process is running.
  - Owns user-facing lifecycle actions (`Open Vapor`, `Quit Vapor`).
- Background daemon (`vapord` LaunchAgent)
  - Independent runtime for sync execution and durability.
  - Keeps running when only the UI window is closed.
  - Ships inside the same `Vapor.app` bundle at `Contents/MacOS/vapord`.

## Expected lifecycle behavior

- Auto-launch at login starts `vapord` and keeps Vapor as a menubar surface without opening the main window.
- Closing the main window closes the UI and removes Dock presence.
- Closing the main window does not stop `vapord` and does not remove menubar status/control.
- Reopening from menubar focuses the existing main window when present, or restores it when closed.
- Quitting from menubar performs full shutdown semantics (stop daemon, then terminate app process). The daemon installs SIGTERM/SIGINT handlers so `launchctl kill TERM` (or Ctrl-C in dev) exits its tick loop cleanly at the next tick boundary.
- Crash-loop protection is owned by the daemon and the app's lifecycle coordinator (not `launchd`). After 5 consecutive unclean exits within 10 minutes the coordinator enters a `CrashLoopPaused` state, stops attempting auto-restart, and surfaces a reasoned diagnostic to the menubar; the user must invoke `acknowledgeCrashLoopPause` (wired through a menubar action) before automatic restarts resume. See `docs/operations/launchagent-policy.md` for the full backoff schedule, plist policy, and validation scenarios.
