# core

Rust workspace crates for the portable sync runtime, shared contracts,
platform abstractions, daemon lifecycle policy, and the `vapor` CLI.

The `core/*` tree is the portable runtime that powers every Vapor app
surface (`apps/macos` today; `apps/windows`, `apps/linux` later). Apps are
UI + OS-integration shims on top of this runtime; no business logic lives
in them.

## Crates

- `core/daemon` — `vapor-daemon` crate and `vapord` binary: fs-watch
  ingest, debounce, scheduler, throttle, durable queue, staged executor,
  reconcile walk, remote poll, multi-profile runtime, IPC service.
- `core/providers` — the `Provider` trait and capabilities, the
  filesystem and Google Drive providers, the contract test suite.
- `core/shared` — constants (source of truth), configuration model,
  error taxonomy, runtime paths, logging with redaction.
- `core/ipc` — framed JSON transport between every surface and the
  daemon (Unix domain socket today).
- `core/platform` — per-OS native implementations behind portable
  traits (fs watch, service install, secret store, metrics, idle,
  filesystem capabilities, process supervision).
- `core/lifecycle` — crash-loop guard, autolaunch setting store, and the
  daemon lifecycle manager every surface drives through the CLI.
- `core/cli` — the `vapor` binary, the universal control plane.

## Testing

Every crate under `core/` is heavily tested. The test suite is the
autonomous coding agent's feedback loop, so it must stay fast
(Tier 1 under 5 minutes per OS on CI), deterministic (no sleeps, no
network, no real `~/.vapor`), and honest (cover real behavior, not
trivial restatements). Full policy in `AGENTS.md §9` and
`docs/architecture/testing-strategy.md`.
