# core

Rust workspace crates for the portable sync runtime, shared contracts,
platform abstractions, daemon lifecycle policy, and the `vapor` CLI.

The `core/*` tree is the portable runtime that powers every Vapor app
surface (`apps/macos` today; `apps/windows`, `apps/linux` later). Apps are
UI + OS-integration shims on top of this runtime; no business logic lives
in them.

## Current contents

- `core/daemon` — `vapor-daemon` crate and `vapord` binary (sync engine).
- `core/providers` — provider trait/capabilities and cloud provider
  integrations.
- `core/shared` — shared models/contracts, constants, and reusable Rust
  utilities (including logging).

## Testing

Every crate under `core/` is heavily tested. The test suite is the
autonomous coding agent's feedback loop, so it must stay fast
(Tier 1 under 5 minutes per OS on CI), deterministic (no sleeps, no
network, no real `~/.vapor`), and honest (cover real behavior, not
trivial restatements). Full policy in `AGENTS.md §9` and
`docs/architecture/testing-strategy.md`.

## Planned additions

Delivered incrementally per `docs/plans/core.md`:

- `core/platform` — trait surfaces + per-OS native implementations for
  fs-watch, service install, secret store, metrics sampling, idle
  detection, filesystem capabilities, and process supervision.
- `core/lifecycle` — daemon lifecycle manager and crash-loop guard
  (currently lives in Swift; moves here so every app surface inherits it).
- `core/cli` — the `vapor` CLI, the reference consumer of the portable
  runtime and the universal control plane for every app surface.
