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
- `attempt_count` semantics: incremented only by `schedule_retry` when a retryable failure is recorded; `lease_ready_batch` does NOT increment it. The cap `MAX_ATTEMPT_COUNT` is enforced at write time so callers must finalize a terminal failure rather than letting the counter overflow.
- Startup recovery must move any leased rows back to pending so interrupted work replays with at-least-once semantics. Leases older than `LEASE_TIMEOUT_MILLIS` (15 minutes) are recovered with `attempt_count` reset to `0` and a recovery diagnostic recorded; younger leases keep their `attempt_count` so retry budgets remain meaningful across short crashes.
- Retry scheduling updates `available_at_ms`, `last_error`, increments `attempt_count`, and persists the longest observed retry slowdown marker so backoff survives restarts.
- Runtime restart recovery adds a conservative whole-scope `ReconcileSubtree` intent at startup so any volatile pre-DB loss is reconstructed before ordinary replay continues. The startup reconstruction barrier auto-clears after `STARTUP_RECONSTRUCTION_BARRIER_DEADLINE_MILLIS` (60 seconds) so non-reconcile work cannot starve indefinitely if the startup reconcile keeps deferring under persistent non-`IdleDrain` pressure.
- Persisted timestamps are bounded by `MAX_TIMESTAMP_MILLIS` and validated on both write and read; the `u128 → i64` conversion uses checked `try_from` so a far-future wall-clock value fails the write rather than silently truncating.
- Durable diagnostic/state reads reject oversized attempt counters, out-of-range timestamps, and oversized state values, while persisted error text is redacted (covering the expanded auth/secret marker set documented in `docs/operations/runtime-logging-and-localization.md`) and length-bounded before storage.

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
