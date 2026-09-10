# Platform abstractions

Authoritative reference for every trait in `core/platform` and its per-OS
native implementations. This document is what a contributor adding a new
platform reads to understand exactly what they must implement.

Governing plan: `docs/plans/core.md`
Tasks: `docs/tasks/core.md` Phase C3 (traits + macOS impls), Phase C6
(Windows impls), Phase C7 (Linux impls).

Status: every trait below ships with a Rust trait definition, an
in-memory fake usable on every OS, and native macOS and Linux
implementations the runtime consumes end to end (fs watch, service
install, secret store, metrics sampling, idle detection, filesystem
capabilities, process supervision, trash). The Linux ones are verified
in a container by the same Tier 1 tests and the e2e harness; Linux
becomes a shipping surface when its app and release lane land. Windows
implementations are stubs that compile and return `Unsupported` or
neutral defaults until that surface ships. The per-trait tables name
what each native implementation reads.

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

macOS and Linux share one `notify`-backed watcher
(`fs_watch/notify_backend.rs`): FSEvents on macOS, inotify on Linux.
When the kernel drops events (an inotify queue overflow, an FSEvents
"must rescan" flag) the watcher emits an `Other` event on the watch
root and the runtime answers with a whole-scope reconcile, so a burst
too large for the queue is picked up by the walk instead of being
lost. A Linux host whose `fs.inotify.max_user_watches` is too small
for the tree fails to start the watcher with a message naming that
sysctl. Hosts whose native watcher is still a stub (Windows today)
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
| Linux (per-user) | `~/.config/systemd/user/sh.arn.vapor.daemon.service` (`$XDG_CONFIG_HOME` honoured) + `systemctl --user daemon-reload / enable / start / stop / disable` |
| Linux (system) | `/etc/systemd/system/vapord.service` + `systemctl` (not implemented; the per-user unit is the product) |
| Windows (per-user) | Task Scheduler `AtLogOn` trigger via `ITaskService` |
| Windows (system) | SCM (`CreateServiceW`) via `windows-service` |

Each installer configures the OS restart policy so the OS respects the
backoff computed by `core/lifecycle::CrashLoopGuard`:

- launchd: `KeepAlive=false` (daemon owns restart decisions).
- systemd: `Restart=no` (same reason).
- Task Scheduler: `RestartOnFailure` matching the guard.

The one service that is kept alive by the OS is the headless supervisor
(`vapor service check --loop`, descriptor `keep_alive: true`):
`KeepAlive=true` on launchd, `Restart=always` with `RestartSec=5` on
systemd. It is the supervisor that applies the guard's backoff to the
daemon.

### `SecretStore`

Per-provider OAuth tokens and other credentials. `get`, `set`,
`delete`, `list`, plus `is_persistent` so a surface can tell the user
when a value will not outlive the process.

| OS | Native API | Status |
|---|---|---|
| macOS | Keychain Services (`SecItem*` through `security-framework-sys` and `core-foundation`) | Shipped. One generic-password item per secret under the `sh.arn.vapor` service, with an access list covering `vapor` and `vapord` so the daemon reads CLI-stored tokens without a prompt. Details: `docs/operations/provider-auth-operations.md`. |
| Windows | Credential Manager | Stub. `for_current_user` returns `Unsupported`. |
| Linux (desktop) | Secret Service through libsecret's `secret-tool` CLI (`lookup` / `store` / `clear` / `search`), items filed under the attribute `service = sh.arn.vapor` | Shipped. Chosen when `DBUS_SESSION_BUS_ADDRESS` is set, `secret-tool` is on `PATH`, and `VAPOR_SECRETS_COMMAND` is not. |
| Linux (headless) | External command shim named by `VAPOR_SECRETS_COMMAND` (a `pass`, vault, or `age` wrapper): `<command> get <name>` prints the secret, `<command> set <name>` reads it on stdin, `<command> delete <name>`, `<command> list` prints one name per line; exit 1 means not found | Shipped. Takes precedence over the Secret Service when set. Never a plaintext file. |

A Linux host with neither backend gets `Unsupported` from
`for_current_user`, with a message naming the variable to set; the CLI
then falls back to the in-memory store and prints that reason on every
`auth` command, as it does on Windows. `vapor doctor` names the backend
in use.

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
| Linux | Shipped. `/proc/stat` (system CPU), `/proc/self/stat` (daemon CPU), `/proc/self/statm` and `/proc/meminfo` (memory), `/sys/class/power_supply/*` (`on_battery` when no mains or USB supply is online and a battery is discharging), the hottest `/sys/class/thermal` zone against its trip points (`thermal_pressure`), and the idle notifier for `user_active`. Anything the host does not expose (a container, a VM without thermal zones) keeps the neutral default for that input. `low_power_mode`, `disk_pressure`, and the network fields keep their defaults; PSI under `/proc/pressure/` and NetworkManager's metered flag are open work. |

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
| Linux | `DISPLAY` / `WAYLAND_DISPLAY` presence | Shipped as far as it goes: a host without a graphical session is always idle (idle boost may run on a server or in a container); a desktop reports zero idle time so idle boost stays off while someone may be typing. |
| Linux (X11) | `XScreenSaverQueryInfo` | Planned. |
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
| Linux | freedesktop trash spec: `$XDG_DATA_HOME/Trash` (default `~/.local/share/Trash`) for files on the home volume, `<mount>/.Trash-<uid>` for other volumes; `info/<name>.trashinfo` written before the rename, numbered duplicates like a file manager | Shipped. A volume without a usable `.Trash-<uid>` is refused, and the managed trash takes over. |

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
