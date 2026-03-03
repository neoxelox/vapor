# vapor

background cloud sync that won't melt your device 🔥

`vapor` is an invisible-first macOS sync application with a SwiftUI app and a Rust daemon.
It is designed to auto-launch at login, sync opportunistically, and preserve laptop performance
over strict real-time behavior.

## Project Status

- Current stage: planning and repository foundation.
- Product direction: bidirectional eventual consistency for Google Drive in MVP.
- Primary constraint: do no harm to user workload, battery, and thermal headroom.

## Product Goals

- Keep a selected local folder (default `~/Drive/`) synced to cloud with durable intent state.
- Stay low-impact during active development and heavy system load.
- Defer expensive work under pressure while maintaining eventual consistency.
- Provide transparent state, diagnostics, and user controls from the macOS app/menubar.

## Core Architecture

- SwiftUI app
  - Onboarding, provider auth, root selection, settings, diagnostics, menubar state.
  - Auto-launch toggle and daemon control surface.
- Rust daemon (LaunchAgent)
  - FSEvents ingestion, debounce/coalescing, scheduler, throttle controller.
  - Durable queue/state, retries, deferred reconcile, provider execution.
- Provider modules
  - `provider_gdrive` first, `provider_s3`/R2 later via shared provider trait.
- XPC boundary
  - Typed status/control API between app and daemon.

## Runtime Model

- Auto-launch at login is ON by default.
- Throttle states govern all heavy work:
  - `IdleDrain`, `Light`, `Throttled`, `Suspended`.
- Eventual consistency is guaranteed by durable intent persistence and retry logic.
- Bidirectional safety includes loop prevention and deterministic conflict handling.

## Planning Docs

- Index: `docs/plans/README.md`
- Source plan (verbatim): `docs/plans/vapor-original-plan-verbatim.md`
- Derived macOS plan: `docs/plans/vapor-macos-plan.md`
- Execution task list: `docs/plans/vapor-macos-task-list.md`

## Development and Contribution

- Contributor operating rules: `AGENTS.md`
- License: `LICENSE`

Implementation work follows the phase checklist in `docs/plans/vapor-macos-task-list.md`,
starting with repository/documentation hardening before core sync engine code.
