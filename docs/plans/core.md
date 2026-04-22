# Vapor core plan (portable Rust runtime)

Execution checklist: `docs/tasks/core.md`

## 0) Mission

The Rust core (`core/*`) is the portable runtime that powers every Vapor
surface. Every product capability lives here; apps under `apps/*` are UI and
OS-integration shims on top of this runtime. macOS ships first with a native
SwiftUI app, but the core is designed so Windows, Linux, and a CLI app can
adopt the same runtime without forking behavior.

## 1) Non-negotiables

1. **The Rust core is the portable runtime.** Apps under `apps/*` are UI + OS
   glue only. No business logic in Swift, no business logic in WinUI, no
   business logic in GTK. The sync engine, throttle controller, durable queue,
   lifecycle policy, crash-loop guard, and config/state layout live in Rust.
2. **Platform-specific code is welcome inside the Rust core** when it unlocks
   native performance. An abstraction trait hides the OS from the engine;
   behind the trait we use the best native API (FSEvents / ReadDirectoryChangesW /
   inotify+fanotify; launchd / Task Scheduler / systemd; Keychain / Credential
   Manager / Secret Service), not a lowest-common-denominator wrapper.
3. **Feature parity is mandatory for the invariants.** Autolaunch, crash-loop
   protection, durable queue, throttle discipline, secret storage, resource
   budgets, and ignore-rule semantics are delivered on every supported OS.
   Missing an implementation on one OS is an "implement the platform trait"
   task, never a "skip it on that OS" decision.
4. **The macOS Swift app keeps working unchanged.** Nothing about it needs to
   become cross-platform. Runtime logic that today sits in Swift (daemon
   lifecycle, crash-loop guard, service install) moves down into Rust so every
   future platform inherits it; the macOS app calls the Rust-backed layer.
5. **Performance is a per-platform commitment.** Each OS gets the idiomatic,
   native implementation of its platform trait. We do not accept portable
   implementations that cost measurable CPU, latency, or battery compared to
   the native one.

## 2) Product architecture (target state)

```
core/
  daemon/     Rust sync engine (portable; no OS-specific calls)
  providers/  cloud provider adapters (portable; network code)
  shared/     contracts, constants, types used across the workspace
  platform/   traits + per-OS native implementations (NEW)
  lifecycle/  daemon lifecycle + crash-loop guard (NEW; moved from Swift)
  cli/        the `vapor` CLI binary (NEW)

apps/
  macos/      SwiftUI/AppKit shell (unchanged)
  windows/    native Windows shell (LATER)
  linux/      native Linux shell (LATER)
```

### 2.1 Portable engine (already works)

- `core/daemon/src/{debounce, scheduler, throttle, workgate, reconcile, retry,
  event_intents, storm, executor, state_db, path_filter}.rs` — zero OS
  dependencies. Keep the current shape.
- `core/providers/src/lib.rs` — `trait Provider` + `ProviderCapabilities`. Keep
  the current shape.
- `core/shared/src/{constants, runtime_paths, logging}.rs` — small portability
  fixes listed in §4 below; structure unchanged.

### 2.2 Platform abstraction layer (`core/platform`)

A new crate (or module inside `core/shared`) that defines the traits the
daemon consumes, with one native implementation per supported OS, selected
via `#[cfg(target_os = "...")]`. Traits listed in §3.

### 2.3 Daemon lifecycle (`core/lifecycle`)

Moves from `apps/macos/Sources/VaporCore/DaemonLifecycle.swift` into Rust:

- `DaemonLifecycleManager` — orchestrates install/enable, start, stop.
- `CrashLoopGuard` — pure-logic backoff policy (5 crashes in 10 minutes →
  `CrashLoopPaused`). Direct Swift → Rust port.
- `AutoLaunchSettingStore` — reads/writes `vapor.json`'s `autoLaunch` field.
  Swift and Rust share the same file.

The macOS Swift app becomes a thin consumer of the Rust-backed lifecycle
manager (via FFI or by shelling out to the `vapor` CLI). Windows, Linux, and
CLI surfaces consume the same layer directly.

### 2.4 The `vapor` CLI (`core/cli`)

A headless-first binary that exposes every runtime capability. It ships on
every supported OS, proves the platform abstractions are correct, and is the
universal control plane other apps can shell out to. Command surface in §7.

## 3) Platform traits

All traits live under `core/platform/src/*`. Each has one native implementation
per OS. Tests use an in-memory fake.

### 3.1 `FsWatcher`

Start/stop a recursive watch on a canonical root; deliver
`Created`/`Modified`/`Removed`/`Renamed` events via a Send channel; preserve
the lightweight-callback discipline (no DB/hash/network work in the OS
callback, per `docs/architecture/data-flow.md`).

- macOS — FSEvents (via `notify` initially; direct `FSEventStreamCreate` if
  benchmarks demand finer tuning of latency/coalescing or `since_when`
  historical event delivery for delta reconcile).
- Windows — `ReadDirectoryChangesW` with IOCP (via `notify` initially; direct
  `windows` crate if buffer sizing or rename pairing needs tuning).
- Linux — `inotify` for user-level trees; `fanotify` for system-wide/headless
  scenarios with `CAP_SYS_ADMIN`. `vapor doctor` detects
  `/proc/sys/fs/inotify/max_user_watches` exhaustion and prints a remediation
  hint.

### 3.2 `ServiceInstaller`

Install/uninstall/start/stop the daemon as a platform-native background
service. This is how Vapor's "autolaunch at login" feature ports. Every OS has
a first-class native mechanism:

| OS | Mechanism |
|---|---|
| macOS (per-user) | `~/Library/LaunchAgents/sh.arn.vapor.daemon.plist` + `launchctl bootstrap/kickstart/bootout`; `SMAppService` registration mirrored via a small FFI helper when invoked from the Swift app. |
| Linux (per-user) | `~/.config/systemd/user/vapord.service` + `systemctl --user enable --now vapord`. Optional `loginctl enable-linger <user>` for sync while logged out. |
| Linux (system-wide) | `/etc/systemd/system/vapord.service` + `systemctl enable --now vapord`. Opt-in via `vapor service install --system`. |
| Windows (per-user) | Task Scheduler with an `AtLogOn` trigger and `RestartOnFailure` via `ITaskService`. No admin prompt needed, matching the macOS LaunchAgent UX. |
| Windows (system-wide) | Windows Service via the `windows-service` crate + SCM (`CreateServiceW`). Opt-in via `vapor service install --system`. |

Crash-loop protection stays in `core/lifecycle::CrashLoopGuard`; each platform
installer configures the OS restart policy (`KeepAlive=false` on launchd,
`Restart=on-failure` + `RestartSec` on systemd, `RestartOnFailure` on Task
Scheduler) so the OS respects the backoff the guard computes.

### 3.3 `SecretStore`

Per-provider OAuth tokens and other credentials. No generic encrypted file by
default — every desktop OS has a native store.

| OS | Native API |
|---|---|
| macOS | Keychain Services (`security-framework`). |
| Windows | Credential Manager (`windows` crate or `keyring`). |
| Linux (desktop) | Secret Service / libsecret via D-Bus (`secret-service`). |
| Linux (headless / server) | Two supported fallbacks: (a) age-encrypted file at `<vapor_dir>/secrets.age` keyed by a machine-bound passphrase or `systemd-creds`; (b) external command shim (HashiCorp Vault, pass, aws-vault). |

The `keyring` crate unifies the first three with a single API and is the
default; drop to direct APIs only when access-group semantics are required.

### 3.4 `PlatformMetricsSampler`

Populates `ThrottleInputs` every tick. The engine already defines the
portable value shape in `core/daemon/src/throttle.rs`; this trait is the
per-OS source. Prefer notification-based signals over polling where available.

- macOS — `host_statistics64` + `task_info`; `IOPSCopyPowerSourcesInfo`;
  `NSProcessInfo.isLowPowerModeEnabled`; `NSProcessInfo.thermalState`;
  `nw_path_monitor` for expensive-link detection.
- Windows — `GetSystemTimes` + `GetProcessTimes` or PDH; `GetSystemPowerStatus`;
  `CallNtPowerInformation(SystemPowerInformation)`;
  `NotifyNetworkConnectivityHintChange` for metered-connection awareness
  (metered ⇒ auto-throttle bandwidth).
- Linux — `/proc/stat`, `/proc/self/stat`; `/sys/class/power_supply/*`;
  `/proc/pressure/{cpu,io,memory}` (PSI — the best pressure signal available);
  `/proc/net/dev`; NetworkManager D-Bus `NM-metered` when present.

### 3.5 `IdleNotifier`

User-idle duration. Event-driven where possible, polled otherwise. Separate
from `PlatformMetricsSampler` so headless hosts can trivially stub it to
"always idle" (CLI/server scenario).

- macOS — `CGEventSourceSecondsSinceLastEventType`.
- Windows — `GetLastInputInfo` (polled on the 1s throttle cadence).
- Linux — X11 `XScreenSaverQueryInfo`; Wayland `org.freedesktop.ScreenSaver` or
  `ext-idle-notify-v1`; headless hosts ⇒ always-idle.

CLI / server operators can force a specific mode via
`--user-activity=always|never|auto`.

### 3.6 `FilesystemCapabilities`

Metadata tagging (op-id for self-write-cache) and case-sensitivity discovery:

- macOS — native xattr (`getxattr`/`setxattr`).
- Linux — native xattr on ext4/xfs/btrfs; side-file fallback on FS without
  xattr support (FAT, some network mounts).
- Windows — NTFS alternate data streams (ADS) via `CreateFileW` with
  `filename:streamname`; side-file fallback on ReFS/FAT.
- Case-sensitivity surfaced via `case_sensitive_by_default() -> bool` so loop
  prevention and conflict-path derivation respect local semantics.

### 3.7 `ProcessSupervisor`

Graceful shutdown:

- macOS/Linux — `SIGTERM` from launchd/systemd, `SIGINT` from terminal; install
  via `signal-hook` into the existing `SHUTDOWN_REQUESTED` atomic in
  `core/daemon/src/runtime.rs`.
- Windows — `SetConsoleCtrlHandler` for interactive runs; `SERVICE_STOP` from
  SCM for service runs (`windows-service` crate); `WM_ENDSESSION` for logout.

## 4) Small engine fixes required for multi-OS build

These are strictly inside existing files, not new abstraction surface, but
they must land before `core/daemon` compiles on Windows or Linux.

- **`core/shared/src/runtime_paths.rs`** — Unix-only `DirBuilderExt` /
  `OpenOptionsExt` / `PermissionsExt` calls. Gate with `#[cfg(unix)]`; on
  Windows translate to equivalent DACL or accept inherited permissions for
  MVP. Replace `HOME`-only lookup with `directories`-style resolution
  (`USERPROFILE` on Windows, `$XDG_*` on Linux when present).
- **`core/daemon/src/sync_directories.rs`** — same `HOME` gap; same fix.
- **`core/daemon/src/state_db.rs`** — Unix `OsStrExt`/`OsStringExt` for path ↔
  bytes. Replace with UTF-8 storage (`Path::to_str()` + `PathBuf::from(&str)`)
  or the `os_str_bytes` crate. Pre-GA schema bump is fine per `AGENTS.md §1.1`.
- **`core/daemon/src/fs_events.rs`** — `normalize_absolute_path` rejects
  `Component::Prefix`, which excludes every Windows drive-letter path. Extend
  to preserve prefixes; optionally strip `\\?\` UNC via `dunce` for user-
  facing paths.
- **`core/daemon/Cargo.toml`** — move `libc` under
  `[target.'cfg(unix)'.dependencies]` once §3.7 handles signals via a
  portable crate.
- **Defaults in `core/shared/src/constants.rs`** — keep `"~/Vapor"` and
  `"/Vapor"`; verify tilde expansion honors `USERPROFILE` on Windows inside
  `sync_directories.rs`.

## 5) IPC between apps and daemon

No IPC code exists yet. The existing `docs/architecture/ipc-contracts.md`
(formerly `xpc-contracts.md`) keeps its versioning, handshake, and skew-matrix
discipline; the transport is swapped per OS:

- macOS — Unix domain socket at `<vapor_dir>/vapord.sock` (default) with
  optional NSXPC wrapping if sandboxing capability delegation is ever needed.
- Linux — Unix domain socket at `<vapor_dir>/vapord.sock`.
- Windows — named pipe `\\.\pipe\vapord-<user-sid>`.

Protocol: **JSON-RPC 2.0** over the transport, length-prefixed frames.
Debuggable with `nc` / pipe tools, easy from every language, zero-ceremony in
Rust. Migrate to gRPC/`tonic` over the same transports only if typed
multi-language clients become a requirement.

## 6) Distribution trust chain (per platform)

Each platform owns its own trust chain. Every platform gets an isolated
GitHub Environment holding its secrets.

- macOS — Developer ID Application cert + `notarytool` keychain profile.
  Environment: `release-macos`. Delegated spec in
  `docs/operations/macos/distribution-trust-chain.md`.
- Windows — EV code-signing certificate (Azure Key Vault or USB HSM) + WiX or
  MSIX packaging + `signtool`. Environment: `release-windows`.
- Linux — GPG-signed AppImage first; `.deb`/`.rpm` as demand surfaces;
  Flathub/Snap later. Environment: `release-linux`.
- CLI — static-ish Rust binaries per target triple, zstd-compressed,
  checksummed, published to GitHub Releases alongside the platform installers.

## 7) The `vapor` CLI

Binary name: **`vapor`** (the daemon binary stays `vapord`). The CLI is the
reference consumer of the portable runtime and the universal control plane.

Command surface:

```
vapor run [--foreground]                # run the daemon in-process
vapor service install [--user|--system] # §3.2 ServiceInstaller
vapor service uninstall
vapor service start|stop|restart|status
vapor config get|set <key> [value]      # edits vapor.json
vapor auth login <provider>             # OAuth PKCE via localhost loopback
vapor auth logout <provider>
vapor status [--json]                   # queue depth, throttle state, reason
vapor pause|resume
vapor flush-now                         # force-flush pending intents
vapor reconcile                         # request whole-scope reconcile
vapor timeline [--tail] [--json]        # diagnostics timeline
vapor logs [--tail] [--level=debug]
vapor doctor                            # checks inotify watches, permissions, etc.
vapor version
```

Design rules:

- Every command has a `--json` output for scripting.
- No interactive-TTY requirements; runs in CI, Docker, SSH.
- Respects the same `VAPOR_DIR`, `VAPOR_ENV`, `vapor.json` as every GUI app.
- When no daemon is running, queries exit non-zero with a clear message; they
  never hang.

## 8) App-shell technology guidance (informational)

You are free to pick per app, but these are the recommended defaults:

- macOS — SwiftUI + AppKit + SMAppService. Already shipped. Keep.
- Windows — **Tauri 2.0** for fastest parity (embeds Rust runtime directly,
  MSI via WiX, single-file exe). WinUI 3 + WindowsAppSDK if you want the most
  native Fluent feel; WPF + .NET if the team has XAML muscle memory.
- Linux — **Tauri 2.0** for parity; GTK4 + libadwaita via `gtk4-rs` for a
  pure-Rust GNOME-native feel; Qt 6 via `cxx-qt` for KDE-first polish.
- CLI — pure Rust: `clap` for args, `indicatif` for progress, `crossterm` for
  terminal UI, JSON mode for scripting.

A reasonable default path is **Tauri 2.0 on Windows + Linux while macOS stays
SwiftUI**: one codebase for the non-mac surfaces, direct Rust runtime
embedding (no IPC needed inside the UI process itself), revisit per-platform
native if Tauri doesn't pull its weight.

## 9) Feature parity matrix

This is the acceptance definition for "Vapor ships on platform X".

| Capability | macOS | Windows | Linux | CLI | Lives in |
|---|---|---|---|---|---|
| Invisible background daemon | ✅ | must | must | must (`vapor run`) | `core/daemon` |
| Autolaunch on login/boot | ✅ LaunchAgent | Task Scheduler / SCM | systemd --user / system | `vapor service install` | `core/platform/service` |
| Crash-loop protection | ✅ Swift | must | must | must | `core/lifecycle` |
| Menubar/tray status | ✅ SwiftUI | tray (WinUI/Tauri/WPF) | tray (GTK/Qt/Tauri) | — | `apps/<os>` |
| FS watch (native-optimal) | FSEvents | ReadDirectoryChangesW + IOCP | inotify / fanotify | via host impl | `core/platform/fs_watch` |
| Durable queue/state | ✅ SQLite | same | same | same | `core/daemon/state_db` |
| Throttle controller | ✅ | same | same | same | `core/daemon/throttle` |
| Metrics sampler | needs impl | needs impl | needs impl | via host impl or static | `core/platform/metrics` |
| User-idle detector | needs impl | needs impl | needs impl | headless ⇒ always-idle | `core/platform/idle` |
| Secret store | Keychain | Credential Manager | Secret Service + file fallback | via host impl | `core/platform/secrets` |
| xattr / metadata tags | xattr | NTFS ADS | xattr | via host impl | `core/platform/fs_caps` |
| Signed + verified distribution | Developer ID + notary | EV cert + signtool | GPG AppImage | binary + checksum | `apps/<os>/scripts`, `core/cli/scripts` |
| Diagnostics / logs / timeline | ✅ | must | must | must | `core/daemon` |

Everything marked "must" is a platform-port deliverable; nothing is allowed
to ship with a hole on one OS because it was "too macOS-y".

## 10) Execution sequence

1. **Docs and naming hygiene.** Rename "XPC contracts" →
   "IPC contracts"; "FSEvents callback" → "fs-watch callback"; reframe
   `AGENTS.md` product intent; split macOS-specific doc files into
   `docs/{architecture,operations}/macos/*`. Zero code risk.
2. **Engine portability fixes (§4).** Make `core/daemon` + `core/shared`
   compile on macOS/Linux/Windows. Add Linux + Windows Rust jobs to the CI
   matrix (lint + test; no perf yet).
3. **Create `core/platform` crate** with trait skeletons (§3) and macOS
   implementations ported from existing Swift/docs. No new app surfaces yet.
4. **Move lifecycle into `core/lifecycle`** (§2.3). Swift app starts
   consuming the Rust-backed lifecycle via FFI or via the prototype `vapor`
   CLI. macOS behavior unchanged end-to-end.
5. **Ship the `vapor` CLI.** First non-macOS surface. Validates that
   `core/platform` + `core/lifecycle` actually work by running the real
   daemon on Linux + Windows with full autolaunch semantics.
6. **Land Linux and Windows platform implementations** for every remaining
   trait in `core/platform`. Real per-OS CI jobs run real tests.
7. **Decide app tech per OS** (see §8). Start `apps/windows` and
   `apps/linux`. Each is a thin shell over `core` + the IPC channel.
8. **Ship v0.3 as the multi-platform MVP.** macOS app + `vapor` CLI + Linux
   app + Windows app, all sharing one `core` runtime.

## 11) Definition of done (applies to every milestone)

- Behavior validated on every supported OS — happy path and failure path.
- Crash/restart recovery preserved on every OS.
- Throttle/backpressure invariants upheld on every OS.
- Autolaunch + lifecycle behavior verified on every OS (including the CLI).
- Diagnostics expose `current state + reason` identically on every OS.
- Platform trait gets an in-memory fake for cross-OS unit tests and a real
  native impl tested in each OS-specific CI job.
- Docs/contracts updated in the same change set.

## 12) What must not change

- The macOS Swift app. No cross-platform refactor of Swift code, ever. The
  only effect of this plan on Swift is that some logic moves *out* of Swift
  into Rust (crash-loop guard, lifecycle orchestration) so the other
  platforms get it; the Swift app then calls the Rust-backed version.
- The sync engine invariants in `AGENTS.md §1, §3, §4, §5` — throttle
  discipline, durable queue, bidirectional safety, "never lose intent state",
  recover after crash. These are what Vapor *is*.
- The provider trait boundary (`core/providers/src/lib.rs`) — already
  correctly generic.
- `VAPOR_DIR`, `VAPOR_ENV`, `vapor.json` schema, and the locale catalogs.
  Every platform reads the same config surface.
- Script-first validation discipline — contributors still run
  `./scripts/lint.sh`, `./scripts/test.sh`. The scripts learn per-OS routing
  internally; the entry points stay.
