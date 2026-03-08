# Product Status and Goals

## Project status

- Current stage: planning and repository foundation.
- Product direction: bidirectional eventual consistency for Google Drive in MVP.
- Primary constraint: do no harm to user workload, battery, and thermal headroom.
- Pre-GA compatibility policy: backward compatibility is not guaranteed yet; config/state/schema and local interfaces may change during active development.

## Product goals

- Keep one selected local folder (default `~/Vapor`) bidirectionally synced with one selected cloud folder (default `/Vapor`) with durable intent state.
- Scope sync strictly to that configured folder pair; Vapor is not intended to be full-device backup.
- Stay low-impact during active development and heavy system load.
- Defer expensive work under pressure while maintaining eventual consistency.
- Provide transparent state, diagnostics, and user controls from the macOS app and menubar.

## Runtime model

- Auto-launch at login is ON by default.
- Throttle states govern all heavy work: `IdleDrain`, `Light`, `Throttled`, `Suspended`.
- Eventual consistency is guaranteed by durable intent persistence and retry logic.
- Bidirectional safety includes loop prevention and deterministic conflict handling.
