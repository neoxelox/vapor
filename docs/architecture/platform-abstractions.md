# Platform abstractions

Authoritative reference for every trait in `core/platform` and its per-OS
native implementations. This document is what a contributor adding a new
platform reads to understand exactly what they must implement.

Governing plan: `docs/plans/core.md`
Tasks: `docs/tasks/core.md` Phase C3 (traits + macOS impls), Phase C6
(Windows impls), Phase C7 (Linux impls).

Wave 4 status (`core/platform` v0): every trait listed below ships with
a Rust trait definition, an in-memory fake usable on every OS, a macOS
native impl scaffolded behind the trait, and Linux / Windows native
impls stubbed to compile (returning `Unsupported` errors at runtime).
The runtime currently consumes `ProcessSupervisor` end-to-end; the
remaining traits are wired progressively as Waves 5–8 land their
respective consumers (`core/lifecycle`, IPC, the C8 runtime
expansion).

## Design rules

1. **One trait per platform-sensitive capability.** The engine code
   (`core/daemon`, `core/providers`, `core/shared`) consumes traits, never
   platform APIs directly.
2. **Native-optimal implementations.** A portable library is fine as a
   starting point, but when benchmarks show it leaving performance on the
   table we write a direct native implementation behind the same trait.
3. **In-memory fake per trait.** Every trait has a test fake so unit tests
   run on any host.
4. **Selection via `#[cfg(target_os = "...")]`.** `core/platform` does not
   use runtime dispatch; the right implementation is selected at compile
   time.
5. **No behavior drift.** Parity tests verify that every platform
   implementation obeys the same contract. Performance numbers may differ
   per OS; semantics must not.

## Trait catalog

### `FsWatcher`

Recursively watch a canonical root; emit normalized events
(`Created`/`Modified`/`Removed`/`Renamed`) on a Send channel.

Callback discipline: the OS callback only normalizes path + filters via
the ignore rules + pushes onto a bounded incoming queue. No DB / hash /
network work in the callback path. Per-component symlink resolution runs on
the runtime thread, not in the callback. See `docs/architecture/data-flow.md`
§"Local to remote".

| OS | Native API | MVP |
|---|---|---|
| macOS | FSEvents (`FSEventStreamCreate`) | `notify` (wraps FSEvents) |
| Windows | `ReadDirectoryChangesW` + IOCP | `notify` (wraps it) |
| Linux | `inotify` (user) / `fanotify` (system) | `notify` (wraps inotify) |

When to go direct (past MVP): macOS `since_when` historical events for delta
reconcile; Windows buffer sizing + rename-pair determinism on big trees;
Linux `fanotify` for whole-system scenarios with `CAP_SYS_ADMIN`.

### `ServiceInstaller`

Install / uninstall / start / stop the daemon as a platform-native
background service. This is how "autolaunch at login" ports.

| OS | Mechanism |
|---|---|
| macOS (per-user) | `~/Library/LaunchAgents/sh.arn.vapor.daemon.plist` + `launchctl bootstrap / kickstart / bootout` |
| Linux (per-user) | `~/.config/systemd/user/vapord.service` + `systemctl --user` |
| Linux (system) | `/etc/systemd/system/vapord.service` + `systemctl` |
| Windows (per-user) | Task Scheduler `AtLogOn` trigger via `ITaskService` |
| Windows (system) | SCM (`CreateServiceW`) via `windows-service` |

Each installer configures the OS restart policy so the OS respects the
backoff computed by `core/lifecycle::CrashLoopGuard`:

- launchd: `KeepAlive=false` (daemon owns restart decisions).
- systemd: `Restart=on-failure` + `RestartSec` matching the guard.
- Task Scheduler: `RestartOnFailure` matching the guard.

### `SecretStore`

Per-provider OAuth tokens and other credentials.

| OS | Native API | MVP crate |
|---|---|---|
| macOS | Keychain Services | `security-framework` or `keyring` |
| Windows | Credential Manager | `keyring` (Windows backend) |
| Linux (desktop) | libsecret / Secret Service (D-Bus) | `secret-service` |
| Linux (headless) | age-encrypted file or external command shim | `age` + custom |

The headless-Linux fallback is explicit — no silent fall-through.
`--secrets-backend=keyring|file|command` on the `vapor` CLI selects.

### `PlatformMetricsSampler`

Returns a `ThrottleInputs` (`on_battery`, `low_power_mode`,
`thermal_pressure`, `system_cpu_load_percent`, `vapor_cpu_load_percent`,
`disk_pressure`, `network_error_rate_percent`, `network_throughput_kbps`,
`user_active`).

| OS | Notes |
|---|---|
| macOS | `host_statistics64` + `task_info`; `IOPSCopyPowerSourcesInfo`; `NSProcessInfo.isLowPowerModeEnabled`; `NSProcessInfo.thermalState`; `nw_path_monitor` for expensive links. |
| Windows | `GetSystemTimes` + `GetProcessTimes` (or PDH); `GetSystemPowerStatus`; `CallNtPowerInformation(SystemPowerInformation)`; `NotifyNetworkConnectivityHintChange` (metered ⇒ auto-throttle). |
| Linux | `/proc/stat`, `/proc/self/stat`; `/sys/class/power_supply/*`; `/proc/pressure/{cpu,io,memory}` (PSI); `/proc/net/dev`; NetworkManager D-Bus `NM-metered` when present. |

`StaticMetricsSampler` (config-driven) is the headless/CLI fallback and the
test fake.

### `IdleNotifier`

User-idle duration, event-driven where possible.

| OS | Source |
|---|---|
| macOS | `CGEventSourceSecondsSinceLastEventType` |
| Windows | `GetLastInputInfo` polled on the 1s throttle cadence |
| Linux (X11) | `XScreenSaverQueryInfo` |
| Linux (Wayland) | `org.freedesktop.ScreenSaver` / `ext-idle-notify-v1` |
| Headless | Always-idle — CLI/server default |

`--user-activity=always|never|auto` on the `vapor` CLI overrides the
auto-detect.

### `FilesystemCapabilities`

Metadata tagging + case-sensitivity discovery.

| OS | Metadata store | Case-sensitivity default |
|---|---|---|
| macOS | xattr | case-insensitive (APFS/HFS+ default) |
| Linux | xattr (ext4/xfs/btrfs); side-file fallback elsewhere | case-sensitive |
| Windows | NTFS ADS (`CreateFileW` with `filename:streamname`); side-file on ReFS/FAT | case-insensitive by default (per-dir case-sensitivity possible on Win10+) |

Loop-prevention and conflict-path derivation consult this trait so
comparisons respect local filesystem semantics.

Tag API (Wave 8): `read_tag` / `write_tag` / `remove_tag` carry the
engine's op-id (`sh.arn.vapor.op-id`) on files. The Unix native impl
uses the `xattr` crate; `supports_xattr()` reports whether the store is
real, and `OpIdTagStore` in `core/providers` layers the atomic
`{path}.vapor-meta.json` side-file fallback (xattr wins on read when
both exist; side-files are hidden from provider enumeration).

### `ProcessSupervisor`

Graceful shutdown handler registration.

| OS | Sources |
|---|---|
| macOS / Linux | `SIGTERM` (from launchd / systemd), `SIGINT` (from terminal) via `signal-hook` |
| Windows | `SetConsoleCtrlHandler` (interactive); `SERVICE_STOP` via `windows-service` (service runs); `WM_ENDSESSION` (logout) |

The unified handler sets the existing `SHUTDOWN_REQUESTED` atomic in
`core/daemon/src/runtime.rs`. The tick loop observes it and exits cleanly
at the next tick boundary.

## Parity matrix

See `docs/plans/core.md §9` for the capability-vs-OS matrix. Every trait
must deliver the same contract on every supported OS before that OS ships.

## Adding a new platform

1. Add the OS target to the Cargo workspace's CI matrix.
2. Implement every trait in `core/platform/<trait>/<os>.rs`.
3. Add parity tests under `core/platform/tests/` that run against every
   platform impl.
4. Add a platform doc subdirectory under `docs/architecture/<os>/`,
   `docs/operations/<os>/`, and the matching plan + tasks file
   (`docs/plans/<os>.md`, `docs/tasks/<os>.md`).
5. Update this document's parity tables and the `docs/plans/core.md` §9
   matrix.
