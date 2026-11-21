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
4. **The macOS Swift app stays a macOS app.** Nothing about it needs to
   become cross-platform. Runtime logic (daemon lifecycle, crash-loop
   guard, service install) lives in Rust so every platform inherits it;
   the macOS app calls the Rust-backed layer.
5. **Performance is a per-platform commitment.** Each OS gets the idiomatic,
   native implementation of its platform trait. We do not accept portable
   implementations that cost measurable CPU, latency, or battery compared to
   the native one.
6. **Testing is the feedback loop.** Vapor is coded autonomously. The suite is
   what the agent trusts. Core runtime work ships with heavy, scoped testing;
   app surfaces ship with logic tests only (no UI rendering or TTY interaction
   tests: those are verified manually). Discipline rules and the full
   taxonomy live in `docs/architecture/testing-strategy.md`; the policy
   contract lives in `AGENTS.md §9`. Tier 1 (`./scripts/test.sh`) must stay
   under 5 minutes per OS on CI.

## 2) Product architecture

```
core/
  daemon/     Rust sync engine (portable; no OS-specific calls)
  providers/  cloud provider adapters (portable; network code)
  shared/     constants, configuration model, error taxonomy, runtime paths
  ipc/        the wire between the daemon and every surface
  platform/   traits + per-OS native implementations
  lifecycle/  daemon lifecycle + crash-loop guard
  cli/        the `vapor` CLI binary

apps/
  macos/      SwiftUI/AppKit shell
  windows/    native Windows shell (LATER)
  linux/      native Linux shell (LATER)
```

How the pieces work today is in `docs/architecture/system-overview.md`
and `docs/architecture/data-flow.md`. This plan holds the intent.

### 2.1 Portable engine

`core/daemon`, `core/providers`, and `core/shared` call no OS API. They
stay that way.

### 2.2 Platform abstraction layer (`core/platform`)

The crate that defines the traits the daemon consumes, with one native
implementation per supported OS selected via `#[cfg(target_os = "...")]`.
Traits listed in §3.

### 2.3 Daemon lifecycle (`core/lifecycle`)

`DaemonLifecycleManager`, `CrashLoopGuard`, and `AutoLaunchSettingStore`
are Rust. Every surface consumes them: the macOS app through the
bundled `vapor` CLI (`vapor service ... --json`), the CLI and future
apps directly. Crash-loop bookkeeping persists in
`<vapor_dir>/state/lifecycle.json`.

### 2.4 The `vapor` CLI (`core/cli`)

A headless-first binary that exposes every runtime capability. It ships on
every supported OS, proves the platform abstractions are correct, and is the
universal control plane other apps can shell out to. Command surface in §7.

### 2.5 Sync modes (directionality)

The engine supports three sync directions, selected by the `syncMode` setting
and owned entirely by the core runtime (apps only display it):

- `two-way` (default): bidirectional local ⇄ cloud with the keep-both
  conflict policy.
- `pull-only`: one-way cloud → local. Cloud is authoritative; local is a
  read-only replica (strict mirror). Great for keeping several read-only
  local copies/backups of a cloud folder.
- `push-only`: one-way local → cloud. Local is authoritative; the cloud is a
  read-only backup (strict mirror).

`syncMode` is a **per-profile, categorical** setting: the top-level value is
the default and each profile may override it outright (not MIN-lowering), so
one device can run several `pull-only` mirror profiles beside a `two-way`
profile. One-way modes are **strict mirror**: the subordinate side is driven
to exactly match the source, which can overwrite/delete user data, so they
are opt-in per profile, and the daemon holds a one-way profile until its
data-loss warning is acknowledged. Vapor's never-lose-data guarantee is
scoped to `two-way`; one-way modes deliberately trade it for a faithful
mirror with no conflict copy: the declared source of truth wins. The local
trash keeps what a mirror removes on this device; an overwrite, and a
removal in a cloud without a trash, have no undo. Keep-both stays the
`two-way` default.

Full design: `docs/architecture/sync-modes.md`. Pipeline mechanics:
`data-flow.md`.

## 3) Platform traits

All traits live under `core/platform/src/*`: `FsWatcher`,
`ServiceInstaller`, `SecretStore`, `PlatformMetricsSampler`,
`IdleNotifier`, `FilesystemCapabilities`, `ProcessSupervisor`, and
`TrashBin`. Each has one native implementation per OS and an in-memory
fake for tests.

The contract of each trait, the native API behind it on each OS, and
what is shipped, stubbed, or planned are in
`docs/architecture/platform-abstractions.md`. The principles:

- The engine consumes the trait and never the OS API.
- Behind the trait, the best native API of the OS, not a
  lowest-common-denominator wrapper.
- The OS never restarts the daemon on its own: `core/lifecycle` owns
  restart decisions, so the service definition says "no restart".
- A secret is never in a plaintext file, on any OS.
- A stub on an OS that ships no surface must compile.

## 4) Portability

`core/*` builds and passes its tests on macOS, Linux, and Windows in
CI. A change that adds a Unix-only assumption to engine code is a
regression: paths go through `vapor_shared::paths`, home lookup through
`runtime_paths`, and OS calls through `core/platform`.

## 5) IPC between apps and daemon

`core/ipc` implements the channel. `docs/architecture/ipc-contracts.md`
holds the versioning, handshake, and skew-matrix discipline; the transport
is swapped per OS:

- macOS: Unix domain socket at `<vapor_dir>/vapord.sock` (default) with
  optional NSXPC wrapping if sandboxing capability delegation is ever needed.
- Linux: Unix domain socket at `<vapor_dir>/vapord.sock`.
- Windows: a named pipe whose name carries the user and the runtime
  directory (`docs/architecture/windows/ipc-transport.md`).

Protocol: Vapor's own tagged JSON envelopes (`{kind, payload}` requests,
`{outcome, value}` responses) in length-prefixed frames; not JSON-RPC.
Debuggable with `nc` / pipe tools, easy from every language, zero-ceremony
in Rust. Migrate to gRPC/`tonic` over the same transports only if typed
multi-language clients become a requirement.

## 6) Distribution trust chain (per platform)

Each platform owns its own trust chain. Every platform gets an isolated
GitHub Environment holding its secrets.

Each platform ships install packages named `Vapor-<os>-<arch>.<ext>`.
A package holds the app, the `vapor` CLI, and `vapord`, and its
installer puts `vapor` on the `PATH`; a signature is inside the file it
signs (`AGENTS.md` §7).

- macOS: `Vapor-macos-arm64.pkg`. Developer ID Application cert for the
  app, Developer ID Installer cert for the package, and a `notarytool`
  keychain profile. Environment: `release-macos`. Delegated spec in
  `docs/operations/macos/distribution-trust-chain.md`.
- Windows: one installer for x64 and one for ARM64. EV code-signing
  certificate (Azure Key Vault or USB HSM) + WiX or MSIX packaging +
  `signtool`; the format follows the UI toolkit of the app.
  Environment: `release-windows`.
- Linux: a `.deb` and an `.rpm` for x64 and for ARM64, GPG-signed
  inside the package. They install on a host with no desktop, and may
  ship before the app window exists. Flathub/Snap later, as demand
  surfaces. Environment: `release-linux`.
- CLI: no artifact of its own. It ships inside each package.

## 7) The `vapor` CLI

Binary name: **`vapor`** (the daemon binary stays `vapord`). The CLI is the
reference consumer of the portable runtime and the universal control plane.

Command surface:

The command list and its naming rules live in `docs/plans/cli.md` §3;
`vapor --help` is the reference.

Design rules:

- Every command that reports state has a `--json` output for scripting.
- No interactive-TTY requirements; runs in CI, Docker, SSH.
- Respects the same `VAPOR_DIR`, `VAPOR_ENV`, `vapor.json` as every GUI app.
- When no daemon is running, queries exit non-zero with a clear message; they
  never hang.

## 8) App-shell technology guidance (informational)

You are free to pick per app, but these are the recommended defaults:

- macOS: SwiftUI + AppKit + SMAppService. Already shipped. Keep.
- Windows: **Tauri 2.0** for fastest parity (embeds Rust runtime directly,
  MSI via WiX, single-file exe). WinUI 3 + WindowsAppSDK if you want the most
  native Fluent feel; WPF + .NET if the team has XAML muscle memory.
- Linux: **Tauri 2.0** for parity; GTK4 + libadwaita via `gtk4-rs` for a
  pure-Rust GNOME-native feel; Qt 6 via `cxx-qt` for KDE-first polish.
- CLI: pure Rust: `clap` for args, `indicatif` for progress, `crossterm` for
  terminal UI, JSON mode for scripting.

A reasonable default path is **Tauri 2.0 on Windows + Linux while macOS stays
SwiftUI**: one codebase for the non-mac surfaces, direct Rust runtime
embedding (no IPC needed inside the UI process itself), revisit per-platform
native if Tauri doesn't pull its weight.

## 9) Feature parity

Nothing ships on an OS with a hole in an invariant: autolaunch,
crash-loop protection, the durable queue, throttle discipline, secret
storage, and resource budgets come through the matching `core/platform`
trait. An OS that ships no surface may keep stubs. Where each trait
stands on each OS: the parity matrix in
`docs/architecture/platform-abstractions.md`.

## 10) Priorities

**Focus:** a very good, performant, polished core runtime, `vapor` CLI,
and macOS app. Windows and Linux are kept *possible* via the platform
trait layer but are not committed deliverables: they start when the
project owner decides to ship a non-macOS surface. The order of the
open work is in `docs/tasks/README.md`.

## 11) Definition of done (applies to every milestone)

"Every supported OS" below means the OSes currently shipping a surface
(macOS is the primary shipping OS; Windows/Linux only count once their
optional waves land).

- Behavior validated on every shipping OS: happy path and failure path.
- Crash/restart recovery preserved on every shipping OS.
- Throttle/backpressure invariants upheld on every shipping OS.
- Autolaunch + lifecycle behavior verified on every shipping OS
  (including the CLI where it ships).
- Diagnostics expose `current state + reason` identically on every
  shipping OS.
- Platform trait gets an in-memory fake for cross-OS unit tests. Every
  trait has a real native implementation on macOS; Windows/Linux impls
  may be `unimplemented!()` stubs unless their optional wave has
  landed, but the stub state must compile and be tracked in
  `docs/tasks/core.md`.
- Docs/contracts updated in the same change set.

## 12) What must not change

- The macOS Swift app. No cross-platform refactor of Swift code, ever.
  Logic that other platforms need lives in Rust, and the Swift app
  calls the Rust-backed version.
- The sync engine invariants in `AGENTS.md §1, §3, §4, §5`: throttle
  discipline, durable queue, bidirectional safety, "never lose intent state",
  recover after crash. These are what Vapor *is*.
- The provider trait boundary (`core/providers/src/lib.rs`): already
  correctly generic.
- `VAPOR_DIR`, `VAPOR_ENV`, `vapor.json` schema, and the locale catalogs.
  Every platform reads the same config surface.
- Script-first validation discipline: contributors still run
  `./scripts/lint.sh`, `./scripts/test.sh`. The scripts learn per-OS routing
  internally; the entry points stay.
