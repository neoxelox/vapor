# System Overview

## Components

- `core/daemon`: Rust background engine for watch/schedule/execute/durability
  (portable; no OS-specific calls).
- `core/providers`: Rust cloud adapters behind a capability-driven trait.
- `core/shared`: the constants source of truth, the configuration
  model, the error taxonomy, runtime paths, and logging.
- `core/ipc`: the wire between the daemon and every surface: framing,
  the request and response types, the handshake, the client, and the
  server loop (`docs/architecture/ipc-contracts.md`).
- `core/platform`: traits + per-OS native implementations for
  fs-watch, service install, secret store, metrics sampling, idle
  detection, filesystem capabilities, process supervision, and the
  system trash. See `docs/architecture/platform-abstractions.md`.
- `core/lifecycle`: daemon lifecycle manager, crash-loop guard, and
  durable lifecycle state, consumed by every app surface (the macOS app
  reaches it through the bundled `vapor` CLI).
- `core/cli`: the `vapor` CLI; headless-first control plane.
- `apps/macos`: SwiftUI app surface; UX, auth orchestration, status,
  controls. macOS-only by policy.
- `apps/windows`, `apps/linux` (planned): thin native app surfaces over the
  same `core/*` runtime.
- `tools/e2e` and `tools/soak`: the end-to-end harness and the soak
  driver. Both drive the shipping binaries from outside and never ship
  (`docs/development/e2e-verification.md`,
  `docs/development/soak-testing.md`).

## Boundary rules

- App process should not run heavy sync compute.
- Fs-watch callback path must stay lightweight (FSEvents on macOS,
  `ReadDirectoryChangesW` on Windows, inotify/fanotify on Linux; the same
  discipline applies regardless of OS).
- Provider-specific behavior must stay out of core engine scheduling logic.
- OS-specific behavior must stay behind `core/platform` traits, never
  sprinkled through engine code.
- User-configured resource ceilings (`resourceLimits`) are measured
  limits on daemon CPU, memory, and bandwidth; `idleBoost` may raise
  them only when the device is idle with measured headroom and never
  preempts a `Suspended` throttle decision. A throttle state slows sync
  down; only `Suspended` stops it. See `docs/architecture/data-flow.md`
  for the resolution/enforcement sequence.

## Module sketch

```text
vapor/
  core/daemon         # Rust engine + queue/state + throttle + reconcile
  core/providers      # filesystem provider (default), Google Drive provider, additional providers (future)
  core/shared         # constants source-of-truth, config model, error taxonomy, runtime paths, logging
  core/ipc            # framing, protocol types, handshake, client, server loop
  core/platform       # per-OS native impls behind portable traits
  core/lifecycle      # daemon lifecycle + crash-loop guard + durable lifecycle state
  core/cli            # vapor CLI binary
  apps/macos          # SwiftUI shell + settings + menubar + auth UI
  apps/windows        # (planned) thin native Windows shell
  apps/linux          # (planned) thin native Linux shell
  tools/e2e           # end-to-end harness (never ships)
  tools/soak          # soak driver (never ships)
  docs                # planning, architecture, operations, performance, tasks, specs
```
