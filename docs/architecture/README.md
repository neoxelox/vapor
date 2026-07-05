# Architecture

Reference documentation for Vapor's system design. Use this directory when
you need to understand how the pieces fit together, why a boundary is where
it is, or what the contract between two layers looks like.

Common (cross-platform) architecture lives at the top of this directory.
Platform-specific architecture (macOS today; Windows/Linux later) lives in
per-platform subdirectories. For trait implementations shared by the engine
across platforms, start with `platform-abstractions.md`.

## How to use this group

- **New contributor?** Read `system-overview.md`, then `data-flow.md`,
  then `platform-abstractions.md`. That gives you the mental model in
  roughly 20 minutes.
- **Working on the engine?** The engine touches no OS APIs directly; the
  contracts you care about are in `platform-abstractions.md` and
  `ipc-contracts.md`.
- **Working on an app surface?** Start from the matching per-platform
  subdirectory (`macos/`, later `windows/`, `linux/`). The app is a thin
  consumer of the runtime described in the common docs.
- **Debating a schema change?** `compatibility-and-upgrades.md` +
  `state-schema-migrations.md` define the policy; `ipc-contracts.md`
  governs the app ↔ daemon wire.

## Common documents

- `system-overview.md` — component map (`core/daemon`, `core/providers`,
  `core/shared`, `core/platform`, `core/lifecycle`, `core/cli`,
  and the app surfaces under `apps/*`), boundary rules, and the planned
  implementation sequence.
- `data-flow.md` — local→remote and remote→local pipelines, throttle
  discipline, fs-watch callback rules, self-write-cache / loop-prevention
  design, multi-profile watch coordination, conflict handling,
  control/observability surface.
- `sync-modes.md` — sync directionality (`syncMode`): `two-way` (default),
  `pull-only`, and `push-only` strict-mirror one-way modes; per-profile
  resolution, interaction with the keep-both conflict policy, safety
  requirements for the destructive one-way paths, config surface, and the
  pull-only → two-way → push-only build order.
- `ipc-contracts.md` — transport-agnostic contract surface between apps
  (macOS, future Windows/Linux, `vapor` CLI) and the `vapord` daemon.
  Versioning, handshake, skew-matrix, field-omission tolerance, payload
  bounds, diagnostics surface. Transport specifics live per-platform.
- `platform-abstractions.md` — authoritative trait catalog for
  `core/platform`. `FsWatcher`, `ServiceInstaller`, `SecretStore`,
  `PlatformMetricsSampler`, `IdleNotifier`, `FilesystemCapabilities`,
  `ProcessSupervisor`; per-OS native-API mapping; parity matrix; how to
  add a new platform.
- `testing-strategy.md` — authoritative test taxonomy and discipline.
  Unit / integration / property / platform-trait contract / concurrency
  / snapshot / fuzz / guard-rail timing tests; what we deliberately do
  NOT test (UI rendering, TTY interaction, trivial restatements of
  code); per-surface scope (core heavy, apps logic-only, CLI no TTY);
  CI tier model and budget. Read before writing or reviewing a test.
- `state-schema-migrations.md` — durable queue/state schema versioning,
  at-least-once intent semantics, startup recovery, corruption detection
  rules.
- `compatibility-and-upgrades.md` — version compatibility across app,
  daemon, shared contracts, and durable schema; supported skew; upgrade
  and rollback flow.

## Per-platform subdirectories

- `macos/` — macOS-specific architecture: app lifecycle semantics
  (`Window` scene, menubar, Dock, `SMAppService`), and the macOS IPC
  transport (Unix domain socket, optional NSXPC wrapping).
- `windows/` — Windows-specific architecture (transport choice
  documented now; native trait impls land with the optional Wave 12).
- `linux/` — Linux-specific architecture (transport choice documented
  now; native trait impls land with the optional Wave 13).

Architecture docs are living references and are expected to evolve as
implementation lands.
