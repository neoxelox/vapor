# macOS App Lifecycle

## Core architecture

- SwiftUI app (`apps/macos`)
  - Onboarding, provider auth, root selection, settings, diagnostics,
    menubar state.
  - Auto-launch toggle and daemon control surface (delegates to
    `core/lifecycle` by invoking the bundled `vapor` CLI at
    `Contents/Helpers/vapor` as a subprocess — `vapor service …
    --json`; Swift keeps no lifecycle policy).
- Rust daemon `vapord` (`core/daemon`, registered as a per-user LaunchAgent)
  - Fs-watch ingestion, debounce/coalescing, scheduler, throttle controller.
  - Durable queue/state, retries, deferred reconcile, provider execution.
- Platform layer (`core/platform`)
  - Native macOS implementations for `FsWatcher` (FSEvents),
    `ServiceInstaller` (launchd + SMAppService), `SecretStore` (Keychain),
    `PlatformMetricsSampler` (IOKit / NSProcessInfo), `IdleNotifier`
    (CGEventSource), `FilesystemCapabilities` (xattr), `ProcessSupervisor`
    (SIGTERM/SIGINT).
- Provider modules (`core/providers`)
  - Pre-GA default is `FilesystemStubProvider` (inert; reports no
    remote-changes-feed and no server-side-rename).
  - `provider_filesystem` (loopback local) is the Phase C8 reference
    provider used to validate every provider-neutral mechanic before any
    external provider ships.
  - `provider_gdrive` integrates on top of the runtime already validated
    against the filesystem provider.
  - Additional adapters (for example iCloud, S3, R2, Proton Drive) come
    through extensibility hardening and are not part of the first release.
- IPC boundary
  - Transport-agnostic status/control API between app and daemon with
    shared contracts in `docs/architecture/ipc-contracts.md`. Macintosh
    transport is a Unix domain socket by default (see
    `docs/architecture/macos/ipc-transport.md`).

## Runtime components

- **Main app window** (`Window` single-instance scene)
  - Primary configuration and diagnostics UI.
  - Dock-visible while the window is open.
- **Menubar component** (`MenuBarExtra`)
  - Always-on quick status and control surface while the app process is
    running.
  - Owns user-facing lifecycle actions (`Open Vapor`, `Quit Vapor`).
- **Background daemon** (`vapord` LaunchAgent)
  - Independent runtime for sync execution and durability.
  - Keeps running when only the UI window is closed.
  - Ships inside the same `Vapor.app` bundle at `Contents/MacOS/vapord`.

## Expected lifecycle behavior

- Auto-launch at login starts `vapord` and keeps Vapor as a menubar surface
  without opening the main window.
- Closing the main window closes the UI and removes Dock presence.
- Closing the main window does not stop `vapord` and does not remove
  menubar status/control.
- Reopening from menubar focuses the existing main window when present, or
  restores it when closed.
- Quitting from menubar performs full shutdown semantics (stop daemon,
  then terminate app process). The daemon installs SIGTERM/SIGINT handlers
  via `core/platform/process::macos` so `launchctl kill TERM` (or Ctrl-C
  in dev) exits its tick loop cleanly at the next tick boundary.
- Crash-loop protection is owned by `core/lifecycle::CrashLoopGuard`
  (not `launchd`, and not Swift — the former Swift guard was deleted;
  the app only relays outcomes). Crash counters, backoff, and pause
  persist in `<vapor_dir>/state/lifecycle.json`, so they survive
  process restarts and are shared across surfaces. The app's
  `DaemonHealthMonitor` runs `vapor service check` every 30 seconds
  (`VaporConstants.Daemon.healthTickIntervalSeconds`) so unexpected
  daemon exits are detected and routed through the guard. After 5
  consecutive unclean exits within 10 minutes the guard enters a
  durable `CrashLoopPaused` state, stops attempting auto-restart, and
  the app surfaces a reasoned diagnostic to the menubar; the user must
  acknowledge (the menubar action invokes `vapor service acknowledge`)
  before automatic restarts resume. Pause and crash counters are
  reported by `vapor service status [--json]`. See
  `docs/operations/macos/launchagent-policy.md` for the full backoff
  schedule, plist policy, and validation scenarios.
