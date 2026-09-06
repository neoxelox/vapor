# System Overview

## Components

- `core/daemon`: Rust background engine for watch/schedule/execute/durability
  (portable; no OS-specific calls).
- `core/providers`: Rust cloud adapters behind a capability-driven trait.
- `core/shared`: contracts and versioned schema models used across the
  workspace.
- `core/platform`: traits + per-OS native implementations for
  fs-watch, service install, secret store, metrics sampling, idle
  detection, filesystem capabilities, and process supervision. See
  `docs/architecture/platform-abstractions.md`.
- `core/lifecycle`: daemon lifecycle manager, crash-loop guard, and
  durable lifecycle state, consumed by every app surface (the macOS app
  reaches it through the bundled `vapor` CLI).
- `core/cli`: the `vapor` CLI — headless-first control plane.
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
  core/providers      # filesystem provider (default), Google Drive provider, additional providers (future)
  core/shared         # IPC models, error taxonomy, policy models, constants source-of-truth
  core/platform       # per-OS native impls behind portable traits
  core/lifecycle      # daemon lifecycle + crash-loop guard + durable lifecycle state
  core/cli            # vapor CLI binary
  apps/macos          # SwiftUI shell + settings + menubar + auth UI
  apps/windows        # (planned) thin native Windows shell
  apps/linux          # (planned) thin native Linux shell
  docs                # planning, architecture, operations, performance, tasks
```

## Implementation history

The runtime was built in this order; every step below has landed except
the last, which is optional and depends on the project owner opening a
new surface:

1. Documentation and naming hygiene plus engine portability fixes, so
   `core/*` compiles and tests on macOS, Linux, and Windows CI jobs.
2. `core/platform`: trait surfaces with macOS-native implementations.
3. `core/lifecycle`: `CrashLoopGuard` and `DaemonLifecycleManager` ported
   from Swift to Rust so every app surface inherits them.
4. `vapor` CLI (`core/cli`): reference consumer of the portable runtime.
5. Filesystem reference provider and the bidirectional runtime shell.
6. Conflict and tombstone safety, the profile model, the IPC diagnostics
   surface, and one-way sync modes.
7. Google Drive provider, durability and upgrade hardening, live
   configuration reload.
8. Windows and Linux app surfaces (optional; `docs/tasks/README.md`
   Waves 12 to 14).
