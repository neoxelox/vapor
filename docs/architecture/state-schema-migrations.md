# State Schema and Migration Policy

## Scope

Durable queue/state schema rules for daemon persistence.

## Invariants

- At-least-once intent durability is non-negotiable.
- Schema version is explicit and persisted.
- Migrations are tested before release.

## Migration strategy

- Use monotonic integer schema versions.
- Support forward migration on startup.
- Define rollback behavior for each schema transition.

## Current schema

- Current durable DB schema version is `2`.
- `queue_intents` stores pending vs leased work, attempt counts, next-available time, and last error text.
- `failed_intents` stores durable terminal failures so auth/permanent outcomes leave the active queue without losing diagnostics.
- `state_entries` stores small daemon state values (for example resume markers or recovery metadata).
- Startup recovery must move any leased rows back to pending so interrupted work replays with at-least-once semantics.
- Retry scheduling updates `available_at_ms`, `last_error`, and the durable retry slowdown marker so backoff survives restarts.
- Runtime restart recovery currently adds a conservative whole-scope `ReconcileSubtree` intent at startup so any volatile pre-DB loss is reconstructed before ordinary replay continues.
- Durable diagnostic/state reads now reject oversized attempt counters, out-of-range timestamps, and oversized state values, while persisted error text is redacted and length-bounded before storage.

## Compatibility and safety

- Pre-GA durable state treats the current schema version as the only supported version.
- Older local schemas are rejected clearly instead of carrying forward compatibility shims.
- Partial migration failures must not corrupt existing persisted data.
- Backup or snapshot strategy required for high-risk migrations.

## Corruption recovery

- Detect corruption early with integrity checks.
- Enter recoverable degraded mode with user-visible diagnostics.
- Provide documented operator recovery steps and verification.

## Validation requirements

- Unit tests for migration transforms.
- Integration tests for restart with mixed-version states.
- Negative tests for partial/corrupt migration inputs.
