# Vapor macOS app plan

Core runtime plan: `docs/plans/core.md`
CLI plan: `docs/plans/cli.md`
Execution checklist: `docs/tasks/macos.md`

## 0) Scope

This plan covers the macOS app surface only (`apps/macos`): the SwiftUI app
shell, menubar/Dock behavior, macOS-native lifecycle UX, and the macOS
distribution trust chain.

Runtime, engine, and cross-platform abstractions are owned by
`docs/plans/core.md`. The macOS app consumes the Rust-backed runtime and its
platform abstractions — it does not reimplement sync, scheduling, throttling,
or durable state in Swift.

## 1) Non-negotiables (macOS-specific)

- Native SwiftUI `.app` experience: Dock, menu bar, main window, no terminal
  dependency.
- Latest macOS target only (`26.0`).
- Distribution is script-first and CI-runnable; Xcode project/workspace is an
  optional debugging convenience, never the release source of truth.
- `Vapor.app` is the single distributable package and must embed both
  executables:
  - `Contents/MacOS/Vapor`
  - `Contents/MacOS/vapord`
- Runtime daemon launch targets the bundled sibling binary only
  (`Contents/MacOS/vapord`) — never a global install path.
- Autolaunch at login is on by default and restores Vapor as a menubar-only
  surface (no automatic main-window presentation).
- Closing the main window is a UI action only; `vapord` keeps running.

## 2) Product architecture (macOS surface)

1. **SwiftUI app (`apps/macos`)**
   - Onboarding, profile management, provider auth UI, root folder selection.
   - Settings UI (app-global + per-profile overrides).
   - Menubar status: `Idle`, `Queued`, `Syncing`, `Throttled`, `Suspended`,
     `Error`.
   - Controls: `Pause`/`Resume`, `Flush now`, diagnostics panel, timeline tab.
   - Consumes Keychain via macOS-native APIs (production `SecretStore`
     implementation in `core/platform/secrets/macos.rs`).
   - Invokes the Rust-backed lifecycle layer (`core/lifecycle`) for
     autolaunch install, daemon start/stop, and crash-loop state — either via
     FFI or by invoking the `vapor` CLI.
2. **Rust daemon (`vapord`)** — see `docs/plans/core.md`.
3. **Shared runtime/platform layer** — see `docs/plans/core.md`.

## 3) Auto-launch and lifecycle

- Default ON at install and first run.
- Implemented via `core/platform/service::macos`:
  `~/Library/LaunchAgents/sh.arn.vapor.daemon.plist` + `launchctl bootstrap /
  kickstart / bootout`, with optional `SMAppService` registration surfaced
  through the Swift app for login-item UX.
- `RunAtLoad=true`, `KeepAlive=false`. Restart decisions are owned by
  `core/lifecycle::CrashLoopGuard`; `launchd` is intentionally passive.
- Full plist policy, backoff schedule, and validation scenarios live in
  `docs/operations/macos/launchagent-policy.md`.
- Toggle semantics:
  - ON: enable launch mechanism and ensure daemon running.
  - OFF: disable launch mechanism; optionally stop daemon now.

## 4) App component model and lifecycle semantics

- **Main window** (`Window` single-instance scene): primary configuration and
  diagnostics UI; Dock-visible while open.
- **Menubar** (`MenuBarExtra`): always-on quick status and control surface
  while the app process is running; owns `Open Vapor` and `Quit Vapor`.
- **Daemon** (`vapord`): independent runtime; keeps running when the UI
  window is closed; ships at `Contents/MacOS/vapord`.

Behavior invariants:

- Auto-launch at login starts `vapord` and restores Vapor as menubar-only.
- Closing the main window closes UI + removes Dock presence; daemon and
  menubar stay alive.
- Reopening from menubar focuses the existing main window when present, or
  restores it when closed; never starts a duplicate window.
- Quitting from menubar performs full shutdown (stop daemon, then terminate
  app process). The daemon installs platform-native shutdown handlers so
  `launchctl kill TERM` exits the tick loop at the next tick boundary.
- Crash-loop protection is owned by the daemon + `core/lifecycle` (not
  `launchd`). After 5 consecutive unclean exits within 10 minutes the
  coordinator enters `CrashLoopPaused` and surfaces a reasoned menubar
  diagnostic; the user must invoke `acknowledgeCrashLoopPause` (wired through
  a menubar action) to resume.

## 5) Native app bundle and distribution foundation

Script-first packaging that produces `Vapor.app` and a zip artifact without
Xcode archive flow.

### 5.1 SwiftUI app entry

- `@main struct VaporApp: App` is the executable entry; no CLI-style
  `main.swift` conflict.
- Starter UI is native by default (window structure + toolbar/menu command).
- Startup orchestration lives in callable app-core functions invoked from app
  lifecycle hooks so the app window launches fast and daemon bootstrap stays
  asynchronous.

### 5.2 Deterministic packaging (`apps/macos/scripts/package.sh`)

Ordered steps:

1. Build release binary via SwiftPM.
2. Build `vapord` release binary via `cargo`.
3. Create `dist/Vapor.app/Contents/{MacOS,Resources}`.
4. Copy `Vapor` and `vapord` into `Contents/MacOS/`; set execute permissions;
   assert both exist and are executable.
5. Generate `Info.plist` with bundle metadata (`CFBundleShortVersionString`
   from root `VERSION`; `CFBundleVersion` from an Apple-valid mapping;
   `VaporVersion` + `VaporGitCommit` for provenance;
   `LSMinimumSystemVersion=26.0`).
6. Build `AppIcon.icns` from `assets/icon.png` (1024x1024) via `sips` +
   `iconutil`.
7. Copy optional resources from `apps/macos/Resources`.
8. Sign (ad-hoc default; Developer ID + hardened runtime when
   `VAPOR_SIGN_IDENTITY` is set; optional entitlements via
   `VAPOR_ENTITLEMENTS`).
9. Validate (`plutil`, `codesign --verify`, `spctl` best-effort).
10. Produce `dist/Vapor.zip` via `ditto --keepParent`.
11. Notarize + staple + re-zip when `VAPOR_NOTARY_PROFILE` is set.

Required environment inputs:

- `APP_NAME` (default `Vapor`)
- `EXECUTABLE_NAME` (default `Vapor`)
- `BUNDLE_ID` (default `sh.arn.vapor`)
- `MIN_MACOS` (default `26.0`)
- `ICON_PNG` (default `assets/icon.png`)
- `DIST_DIR` (default `dist`)
- `VAPOR_SIGN_IDENTITY` (optional)
- `VAPOR_ENTITLEMENTS` (optional)
- `VAPOR_NOTARY_PROFILE` (optional; presence enables notarization)

### 5.3 Distribution trust chain (macOS)

Full policy in `docs/operations/macos/distribution-trust-chain.md`.

- Code signing (Developer ID Application) for both executables.
- Hardened runtime enabled for distributable binaries.
- Notarization + staple required for release artifacts.
- Entitlement review for least-privilege access.
- GitHub Environment `release-macos` holds signing/notarization secrets.

### 5.4 Build-script and CI integration

- `./scripts/build.sh package` drives `apps/macos/scripts/package.sh`.
- Packaging path is non-interactive and CI-friendly.
- Fail-fast with clear errors when notarization is requested without
  credentials.
- Xcode debugging via `apps/macos/Package.swift` is optional and does not
  gate distribution.

## 6) Localization (macOS)

- UI copy catalogs at `assets/locales/*.json` are synced into
  `apps/macos/Sources/VaporCore/Resources/locales/*.json` before
  Swift build/test/package via `scripts/swift/sync-locales.sh`.
- `languageCode` (persisted in `vapor.json`) defaults to `en` with English
  fallback when the requested catalog is unavailable.
- Missing translation keys in non-English catalogs fall back deterministically
  to English.

## 7) Definition of done (macOS milestones)

- UI behavior validated on clean macOS host for happy + failure paths.
- Window-close vs menubar-quit vs daemon-stop semantics pass automated
  coverage (see §4).
- `dist/Vapor.app` always embeds both `Contents/MacOS/Vapor` and
  `Contents/MacOS/vapord`; runtime launch resolves the bundled sibling only.
- Signed + notarized path works end-to-end when credentials are supplied.
- `AGENTS.md` invariants for throttle discipline, durability, and low-impact
  goals are preserved — validated via the core runtime's platform-native
  sampler/installer implementations.

## 8) Doc deliverables tied to this plan

- `docs/architecture/macos/app-lifecycle.md` — macOS lifecycle semantics.
- `docs/operations/macos/launchagent-policy.md` — plist policy + crash-loop
  interaction + validation scenarios.
- `docs/operations/macos/distribution-trust-chain.md` — signing + notarization
  + entitlement review.
- `apps/macos/README.md` — local build/package/signed/notarized commands.

## 9) Items moved to the core plan

The following items used to live in the macOS plan and now belong to
`docs/plans/core.md` because they are platform-agnostic runtime or lifecycle
work:

- The full sync pipeline (FS watching, debounce, scheduler, throttle, storm
  handling, durable queue, reconcile, retry, backpressure) — all inside
  `core/daemon`.
- Bidirectional sync mechanics (self-write cache, op-id correlation,
  remote-to-local apply, provider cursor) — all inside `core/daemon` +
  `core/providers`.
- Conflict policy, tombstone semantics, deviceId derivation —
  `core/daemon` + `core/shared`.
- Multi-profile model + override resolution — `core/daemon` + `core/shared`.
- Auto-tuning, user resource budgets, idle boost — `core/daemon`.
- Provider-system extensibility — `core/providers`.
- Google Drive provider — `core/providers`.
- IPC contract schema (previously "XPC contracts") —
  `docs/architecture/ipc-contracts.md`.
- LaunchAgent install/start/stop logic — `core/platform/service::macos`.
- Crash-loop guard + daemon lifecycle orchestration — `core/lifecycle`.
