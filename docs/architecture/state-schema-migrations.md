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

- Current durable DB schema version is `6`.
- `queue_intents` stores pending, leased, and held work, a lease-priority rank, attempt counts, next-available time, last error text, the decision a held row waits on (`decision_id`), an optional remote path for a download whose cloud object does not live at the local path's mirror (`remote_path_text`), and an `approved` flag set when a decision released the row.
- `pending_decisions` stores the questions the daemon parked (`data-flow.md` §Decisions): kind, scope, optional path, question, options, JSON evidence, and the created / resolved / applied timestamps with the chosen answer. A decision is open while `resolved_at_ms` is null and applied once `applied_at_ms` is set.
- `sync_index` records, per synced path, the content hash, size, the local mtime and the remote mtime observed when the transfer completed, and the op-id of the last writer; the reconcile walk's quick check on either side reads it.
- `name_aliases` records which cloud object a local conflict copy stands in for when two cloud names fold to one local name (case, Unicode normalization); the executor, the reconcile walk, and the changes feed resolve through it, and a delete on either side releases the row.
- Lease order is `(priority_rank, available_at_ms, id)`: reconcile control intents first (leasing one is cheap and the startup barrier depends on it), fresh file intents next ranked by the path's debounce class (key config before code before lockfile noise), and reconcile-walk backlog last — a whole-scope reconcile of a large tree can never starve a file the user just edited. Coalescing a fresh enqueue onto an existing pending backlog row promotes the row's rank.
- `failed_intents` stores durable terminal failures so auth/permanent outcomes leave the active queue without losing diagnostics.
- `state_entries` stores small daemon state values: resume markers, recovery metadata, the provider changes cursor, the adopted root identities (`root_identity.local`, `root_identity.cloud`), and the `reconcile.merge_without_deletions` flag a `reattach` or `recreate` answer sets for the next whole-scope reconcile.
- `attempt_count` semantics: incremented only by `schedule_retry` when a retryable failure is recorded; `lease_ready_batch` does NOT increment it. The cap `MAX_ATTEMPT_COUNT` is enforced at write time so callers must finalize a terminal failure rather than letting the counter overflow.
- Startup recovery must move any leased rows back to pending so interrupted work replays with at-least-once semantics. Leases older than `LEASE_TIMEOUT_MILLIS` (15 minutes) are recovered with `attempt_count` reset to `0` and a recovery diagnostic recorded; younger leases keep their `attempt_count` so retry budgets remain meaningful across short crashes.
- Retry scheduling updates `available_at_ms`, `last_error`, increments `attempt_count`, and persists the longest observed retry slowdown marker so backoff survives restarts.
- Runtime restart recovery adds a conservative whole-scope `ReconcileSubtree` intent at startup so any volatile pre-DB loss is reconstructed before ordinary replay continues. The startup reconstruction barrier auto-clears after `STARTUP_RECONSTRUCTION_BARRIER_DEADLINE_MILLIS` (60 seconds) so non-reconcile work cannot starve indefinitely if the startup reconcile keeps deferring under persistent non-`IdleDrain` pressure.
- Persisted timestamps are bounded by `MAX_TIMESTAMP_MILLIS` and validated on both write and read; the `u128 → i64` conversion uses checked `try_from` so a far-future wall-clock value fails the write rather than silently truncating.
- Durable diagnostic/state reads reject oversized attempt counters, out-of-range timestamps, and oversized state values, while persisted error text is redacted (covering the expanded auth/secret marker set documented in `docs/operations/runtime-logging-and-localization.md`) and length-bounded before storage.

## Migration history

- `v3 → v4`: widened the intent-kind vocabulary with the remote→local pipeline kinds (`download`, `apply_remote_delete`) by rebuilding the two intent tables (SQLite cannot alter CHECK constraints in place), preserving rows and the AUTOINCREMENT sequence. Rollback to a v3 build after remote-sourced intents were enqueued is unsupported (pre-GA policy).
- `v4 → v5`: added the `priority_rank` column and rebuilt the ready index as `(state, priority_rank, available_at_ms, id)`. Existing file rows backfill to the fresh `Other` rank; reconcile rows to the first rank. Rollback requires dropping the DB (pre-GA policy) — a v4 build's ready index no longer matches.
- `v5 → v6`: rebuilt `queue_intents` to admit the `held` state and the `decision_id`, `remote_path_text`, and `approved` columns; added `pending_decisions` and `name_aliases`; added `remote_modified_at_ms` to `sync_index` (null on old rows, so the remote quick check falls through to hashing once for them). Existing rows carry over unchanged. Rollback requires dropping the DB (pre-GA policy).
- The migrations chain: a v3 database migrates `v3 → v4 → v5 → v6` in one startup transaction.

## Compatibility and safety

- Pre-GA durable state accepts the current schema version plus the migratable ones listed above; anything older is rejected clearly instead of carrying forward compatibility shims.
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
