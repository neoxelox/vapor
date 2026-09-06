# Platform abstractions

Authoritative reference for every trait in `core/platform` and its per-OS
native implementations. This document is what a contributor adding a new
platform reads to understand exactly what they must implement.

Governing plan: `docs/plans/core.md`
Tasks: `docs/tasks/core.md` Phase C3 (traits + macOS impls), Phase C6
(Windows impls), Phase C7 (Linux impls).

Status: every trait below ships with a Rust trait definition, an
in-memory fake usable on every OS, and a native macOS implementation the
runtime consumes end to end (fs watch, service install, secret store,
metrics sampling, idle detection, filesystem capabilities, process
supervision). Linux and Windows implementations are stubs that compile
and return `Unsupported` or neutral defaults; they land with the
optional Waves 12 and 13 in `docs/tasks/README.md`. The per-trait
tables name what each native implementation reads.

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
(`Created`/`Modified`/`Removed`/`Renamed`) on a Send channel. Rename
directionality is resolved in the per-OS mapping: a rename-away maps to
`Removed` at the old path, a rename-in to `Created` at the new path, and
paired rename events are split into that pair; `Renamed` survives only
for ambiguous OS reports (consumers re-stat to disambiguate). Backend
errors reach an optional error handler instead of being dropped.

Both watch consumers go through this one trait — the daemon's local
watch (`core/daemon::fs_events` bridges the event channel through path
normalization, ignore filtering, and the recorder on a dedicated
thread) and the filesystem provider's changes feed — so there is exactly
one OS-event→kind mapping to test and fix per OS.

Hosts whose native watcher is still a stub (Linux and Windows today)
report `native_watcher_available() == false`, and the stub constructor
fails with an `Unsupported` backend error. Consumers degrade rather than
fail: the filesystem provider stops advertising its changes feed, and the
daemon runtime starts without a live watcher and surfaces an `Error` run
state whose reason says local changes are not detected (the multi-profile
runtime suspends the affected profile the same way). Reconcile is the
only source of local changes on those hosts until the native watcher
ships.

Callback discipline: the OS callback only normalizes the kind and pushes
onto the channel. No DB / hash / network work in the callback path.
Per-component symlink resolution runs on the runtime thread, not in the
callback. See `docs/architecture/data-flow.md` §"Local to remote".

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

Per-provider OAuth tokens and other credentials. `get`, `set`,
`delete`, `list`, plus `is_persistent` so a surface can tell the user
when a value will not outlive the process.

| OS | Native API | Status |
|---|---|---|
| macOS | Keychain Services (`SecItem*` through `security-framework-sys` and `core-foundation`) | Shipped. One generic-password item per secret under the `sh.arn.vapor` service, with an access list covering `vapor` and `vapord` so the daemon reads CLI-stored tokens without a prompt. Details: `docs/operations/provider-auth-operations.md`. |
| Windows | Credential Manager | Stub. `for_current_user` returns `Unsupported`. |
| Linux (desktop) | libsecret / Secret Service (D-Bus) | Stub. |
| Linux (headless) | age-encrypted file or external command shim | Planned. The fallback will be explicit, never a silent fall-through, and selected by a CLI flag. |

On an OS whose native store is a stub, the CLI falls back to the
in-memory store and prints a warning on every `auth` command.

### `PlatformMetricsSampler`

Returns a `ThrottleInputs` (`on_battery`, `low_power_mode`,
`thermal_pressure`, `system_cpu_load_percent`, `vapor_cpu_load_percent`,
`disk_pressure`, `network_error_rate_percent`, `network_throughput_kbps`,
`user_active`, `vapor_memory_bytes`, `device_memory_bytes`). CPU
percentages are shares of the whole device, so a single busy core on an
eight-core machine reads as 13%.

| OS | Status |
|---|---|
| macOS | Shipped. `host_statistics64` (system CPU), `getrusage` (daemon CPU), `IOPSGetTimeRemainingEstimate` (battery), `NSProcessInfo.thermalState` and `isLowPowerModeEnabled` through the Objective-C runtime, `proc_pidinfo` and `hw.memsize` (memory), and the HID idle clock for `user_active` (input within the last 30 s). One read per throttle interval; calls inside the interval return the cached reading. `disk_pressure` and the two network fields keep their defaults: macOS has no public disk-pressure signal and link capacity is not measured yet. |
| Windows | Planned. `GetSystemTimes` + `GetProcessTimes`; `GetSystemPowerStatus`; `CallNtPowerInformation`; `NotifyNetworkConnectivityHintChange` (metered means auto-throttle). Today the native sampler returns the static defaults and `has_native_sampling()` is `false`. |
| Linux | Planned. `/proc/stat`, `/proc/self/stat`; `/sys/class/power_supply/*`; PSI under `/proc/pressure/`; `/proc/net/dev`; NetworkManager `NM-metered` when present. Static defaults today. |

`StaticPlatformMetricsSampler` (config-driven) is the headless/CLI
fallback and the test fake. `VAPOR_THROTTLE_INPUTS=static` makes the
daemon use it (with zero idle time) on any host; `scripts/e2e.sh` sets
it so a run is not shaped by whoever is typing on the machine.
`VAPOR_THROTTLE_INPUTS=file:<path>` selects the daemon's
`FileMetricsSampler` and `FileIdleNotifier`, which re-read a JSON
document (`{"inputs": <ThrottleInputs>, "idle_seconds": N}`) on every
sample and fall back to the static defaults while the file is missing
or half-written; the soak driver uses it to walk the daemon through
every throttle state.

### `IdleNotifier`

User-idle duration, polled on the throttle cadence.

| OS | Source | Status |
|---|---|---|
| macOS | `CGEventSourceSecondsSinceLastEventType` on the HID system state | Shipped. Without a window-server session (SSH, CI agents) the notifier reports the headless always-idle reading, decided once at construction. |
| Windows | `GetLastInputInfo` | Planned. Reports zero idle time today so idle boost stays off. |
| Linux (X11) | `XScreenSaverQueryInfo` | Planned. Zero idle time today. |
| Linux (Wayland) | `org.freedesktop.ScreenSaver` / `ext-idle-notify-v1` | Planned. |
| Headless | Always-idle | CLI and server default. |

A CLI flag to force always-idle or never-idle is planned and not
implemented.

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

### `TrashBin`

The user's own trash, for a file Vapor removes on this device. The
daemon's managed trash under `vapor_dir/trash/<profile>/`
(`core/daemon/src/trash.rs`) is the safety net that works on every OS;
this trait is the opt-in (`trash.useSystemTrash`) discoverable
alternative, and the managed trash catches whatever the native bin
refuses (another volume, no session), so a discard never degrades to an
unlink.

| OS | Mechanism | Status |
|---|---|---|
| macOS | Rename into `~/.Trash`, Finder-style numbered duplicates | Shipped. Cross-volume moves are refused rather than copied. |
| Windows | `SHFileOperation` / `IFileOperation` with `FOF_ALLOWUNDO` (Recycle Bin) | Planned. Refuses with `Unsupported` today. |
| Linux | freedesktop trash spec (`~/.local/share/Trash`, per-volume `.Trash-<uid>`) | Planned. Refuses with `Unsupported` today. |

The fake (`InMemoryTrashBin`) moves into a directory the test owns and
can be told to refuse, which is how the fallback is tested.

## Parity matrix

See `docs/plans/core.md §9` for the capability-vs-OS matrix. Every trait
must deliver the same contract on every supported OS before that OS ships.

## Adding a new platform

1. Add the OS target to the Cargo workspace's CI matrix.
2. Implement every trait in `core/platform/<trait>/<os>.rs`.
3. Run the contract tests next to each trait (`#[cfg(test)]` modules
   under `core/platform/src/<trait>/`) against the new native impl; the
   fake and every native impl share one test body per trait.
4. Add a platform doc subdirectory under `docs/architecture/<os>/`,
   `docs/operations/<os>/`, and the matching plan + tasks file
   (`docs/plans/<os>.md`, `docs/tasks/<os>.md`).
5. Update this document's parity tables and the `docs/plans/core.md` §9
   matrix.
