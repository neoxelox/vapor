# System Overview

## Components

- `core/daemon`: Rust background engine for watch/schedule/execute/durability
  (portable; no OS-specific calls).
- `core/providers`: Rust cloud adapters behind a capability-driven trait.
- `core/shared`: contracts and versioned schema models used across the
  workspace.
- `core/platform` (planned): traits + per-OS native implementations for
  fs-watch, service install, secret store, metrics sampling, idle
  detection, filesystem capabilities, and process supervision. See
  `docs/architecture/platform-abstractions.md`.
- `core/lifecycle` (planned): daemon lifecycle manager and crash-loop
  guard, consumed by every app surface.
- `core/cli` (planned): the `vapor` CLI — headless-first control plane.
- `apps/macos`: SwiftUI app surface — UX, auth orchestration, status,
  controls. macOS-only by policy.
- `apps/windows`, `apps/linux` (planned): thin native app surfaces over the
  same `core/*` runtime.

## Boundary rules

- App process should not run heavy sync compute.
- Fs-watch callback path must stay lightweight (FSEvents on macOS,
  `ReadDirectoryChangesW` on Windows, inotify/fanotify on Linux; the same
  discipline applies regardless of OS).
- Provider-specific behavior must stay out of core engine scheduling logic.
- OS-specific behavior must stay behind `core/platform` traits, never
  sprinkled through engine code.
- User-configured resource ceilings (`resourceLimits`) are hard caps on
  daemon CPU/memory/bandwidth; `idleBoost` may dynamically raise them only
  when the device is genuinely idle with measured headroom and never
  preempts a `Suspended` throttle decision. See
  `docs/architecture/data-flow.md` for the resolution/enforcement sequence.

## Module sketch

```text
vapor/
  core/daemon         # Rust engine + queue/state + throttle + reconcile
  core/providers      # provider_filesystem (pre-GA reference), provider_gdrive (deferred), additional providers (future)
  core/shared         # IPC models, error taxonomy, policy models, constants source-of-truth
  core/platform       # (planned) per-OS native impls behind portable traits
  core/lifecycle      # (planned) daemon lifecycle + crash-loop guard
  core/cli            # (planned) vapor CLI binary
  apps/macos          # SwiftUI shell + settings + menubar + auth UI
  apps/windows        # (planned) thin native Windows shell
  apps/linux          # (planned) thin native Linux shell
  docs                # planning, architecture, operations, performance, tasks
```

## Planned implementation sequence

1. Documentation/naming hygiene + engine portability fixes so `core/*`
   compiles on macOS, Linux, Windows CI jobs.
2. `core/platform` crate: trait surfaces + macOS-native implementations
   ported from existing Swift/docs.
3. `core/lifecycle` crate: port `CrashLoopGuard` and
   `DaemonLifecycleManager` from Swift to Rust so every app surface
   inherits them.
4. `vapor` CLI (`core/cli`): reference consumer of the portable runtime;
   validates the platform traits on Linux and Windows.
5. Filesystem reference provider and bidirectional runtime shell
   (provider-neutral mechanics validated against a loopback local
   provider).
6. Conflict/tombstone safety, profile model, IPC diagnostics surface.
7. External cloud provider integration (Google Drive, deferred) and
   durability/upgrade hardening.
8. Windows and Linux app surfaces once the portable runtime is proven in
   anger through the CLI.
