# macOS App Lifecycle

## Core architecture

- SwiftUI app
  - Onboarding, provider auth, root selection, settings, diagnostics, menubar state.
  - Auto-launch toggle and daemon control surface.
- Rust daemon (`core/daemon`, LaunchAgent)
  - FSEvents ingestion, debounce/coalescing, scheduler, throttle controller.
  - Durable queue/state, retries, deferred reconcile, provider execution.
- Provider modules (`core/providers`)
  - `provider_gdrive` first, `provider_s3` and R2 later via shared provider trait.
- XPC boundary
  - Typed status/control API between app and daemon with shared contracts in `core/shared`.

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
- Quitting from menubar performs full shutdown semantics (stop daemon, then terminate app process).
