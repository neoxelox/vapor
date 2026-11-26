# State Schema and Migration Policy

## Scope

Durable queue/state schema rules for daemon persistence.

## Invariants

- At-least-once intent durability is non-negotiable.
- The schema version is explicit and persisted (`schema_meta`).
- A database the build cannot use is set aside, never deleted and
  never half-read.

## Before GA: no migrations

Vapor has no users yet, so a schema change ships with no migration
(`AGENTS.md` §5). The daemon handles a database of another version the
same way as a damaged one:

1. At each start the daemon opens the profile's database and runs
   SQLite's page check (`PRAGMA quick_check`).
2. A database of another schema version is renamed next to itself to
   `vapor.sqlite.schema-v<n>-<ms>`. A file that is not a readable
   SQLite database, or that fails the page check, is renamed to
   `vapor.sqlite.corrupt-<ms>`. Both are logged with the two paths.
3. A fresh database of the current version takes its place.
4. The startup whole-scope reconcile rebuilds the sync index from the
   two trees. With no index row for a path nothing is deleted on either
   side (`data-flow.md` §Local to remote), so the rebuild only transfers
   and verifies. The cost is that a device that lost its index can bring
   back a file that was deleted on the other side while the index was
   gone.
5. Files that were set aside are removed after 30 days.

A transient failure (a full disk, a busy lock, a permission error)
never sets a healthy database aside: it is an ordinary startup failure,
and the crash-loop guard retries. The CLI opens the same database to
read it and reports a version mismatch as an error; only the daemon
sets a file aside.

Migrations start at GA. From then on each schema change defines its
forward migration and its rollback before it ships, with tests for the
transform, for a restart on a mixed-version state, and for a partial or
damaged input.

## Current schema

- Current durable DB schema version is `10`. It has no tombstone table: a deletion is decided from `sync_index` alone.
- `queue_intents` stores pending and leased work, a lease-priority rank, attempt counts, next-available time, last error text, an optional remote path for a download whose cloud object does not live at the local path's mirror (`remote_path_text`), and an `approved` flag set on a row that carries out an answer of the user.
- `pending_decisions` stores the questions the daemon parked (`data-flow.md` §Decisions): kind, scope, optional path, question, options, JSON evidence, and the created / resolved / applied timestamps with the chosen answer. A decision is open while `resolved_at_ms` is null and applied once `applied_at_ms` is set.
- `sync_index` records, per synced path, the content hash, size, the local mtime and the remote mtime observed when the transfer completed, and the op-id of the last writer; the reconcile walk's quick check on either side reads it. Each row also holds its path with every name folded (`fold_key`, indexed), so one seek finds the rows that a path has under another spelling of its name. A row whose path an ignore rule hides is marked (`hidden_since_ms`): it still says what the last sync held, so an edit under the rule is this device's change when the rule goes, and it is no evidence for a deletion, so a file removed under the rule comes back. A transfer that writes the row, or a walk that finds the file here with no rule on it, takes the mark off. A row also holds the id that the provider has for the cloud object of the file (`remote_id`, indexed), when the provider has ids. The engine never reads an id: it compares two for equality, to know which synced file a cloud entry under another path is. The id is written when a transfer of the file ends and when a scan lists the file, it moves with the row, and an id names one row.
- `sync_folders` holds one row for each synced folder that has an id at the provider: its local path and the id. The sync index has no other row for a folder. A row is written when a scan compares a folder that is on both sides, moves with the folder, and goes with the rows under it. At a start the ids of both tables go back to the provider (`Provider::restore_remote_ids`).
- `name_carries` holds one row for each name of this device that still has to go to the cloud as one move of the cloud object: the path on this device, the path that the index rows are under, and the path that mirrors the place of the cloud object (`data-flow.md` §A name or a place that the cloud changed). The executor makes the move and removes the row.
- Two partial indexes, `idx_sync_index_conflict_copy` and `idx_queue_intents_conflict_copy`, hold only the rows whose path carries the conflict-copy marker (`~conflict-`). The unresolved-conflict count in `vapor status` reads them (the machine grammar of the name is checked in Rust on the handful of candidates), so the count costs the same on a hundred-thousand-row index as on an empty one. Indexes are created with `IF NOT EXISTS` on every open, so they need no schema version of their own.
- `name_aliases` records which cloud object a local conflict copy stands in for when two cloud names fold to one local name (case, Unicode normalization); the executor, the reconcile walk, and the changes feed resolve through it, and a delete on either side releases the row.
- Lease order is `(priority_rank, available_at_ms, id)`: reconcile control intents first (leasing one is cheap and the startup barrier depends on it), fresh file intents next ranked by the path's debounce class (key config before code before lockfile noise), and reconcile-walk backlog last; a whole-scope reconcile of a large tree can never starve a file the user just edited. Coalescing a fresh enqueue onto an existing pending backlog row promotes the row's rank.
- `failed_intents` stores durable terminal failures so auth/permanent outcomes leave the active queue without losing diagnostics.
- `state_entries` stores small daemon state values: resume markers, recovery metadata, the provider changes cursor, the adopted root identities (`root_identity.local`, `root_identity.cloud`), the acknowledged one-way mode (`one_way.acknowledged`), and the `reconcile.merge_without_deletions` flag a `reattach` or `recreate` answer sets for the next whole-scope reconcile. It also holds the work a provider still owes after a write, one entry for each record under `provider.follow_up.<hash of the record>` (`provider-onboarding.md`, the follow-up row of the contract): text that only the provider reads, written in the tick that writes the sync index row of the transfer, handed back to the provider when the profile starts, and removed when the provider reports the end of the work. A profile stores at most 10 000 of them, each at most 4 096 bytes. A database that is set aside takes its records with it, so the work of those records is not done.
- `attempt_count` semantics: incremented only by `schedule_retry` when a retryable failure is recorded; `lease_ready_batch` does NOT increment it. The cap `MAX_ATTEMPT_COUNT` is enforced at write time so callers must finalize a terminal failure rather than letting the counter overflow.
- Startup recovery must move any leased rows back to pending so interrupted work replays with at-least-once semantics. Leases older than `LEASE_TIMEOUT_MILLIS` (15 minutes) are recovered with `attempt_count` reset to `0` and a recovery diagnostic recorded; younger leases keep their `attempt_count` so retry budgets remain meaningful across short crashes.
- Retry scheduling updates `available_at_ms`, `last_error`, increments `attempt_count`, and persists the longest observed retry slowdown marker so backoff survives restarts.
- Runtime restart recovery adds a conservative whole-scope `ReconcileSubtree` intent at startup so any volatile pre-DB loss is reconstructed before ordinary replay continues. The startup reconstruction barrier auto-clears after `STARTUP_RECONSTRUCTION_BARRIER_DEADLINE_MILLIS` (60 seconds) so non-reconcile work cannot starve if the startup reconcile keeps deferring.
- Persisted timestamps are bounded by `MAX_TIMESTAMP_MILLIS` and validated on both write and read; the `u128 → i64` conversion uses checked `try_from` so a far-future wall-clock value fails the write rather than silently truncating.
- Durable diagnostic/state reads reject oversized attempt counters, out-of-range timestamps, and oversized state values, while persisted error text is redacted (covering the expanded auth/secret marker set documented in `docs/operations/runtime-logging-and-localization.md`) and length-bounded before storage.

## What a person sees

- The daemon log names a database that was set aside, with both paths:
  an ERROR line for a damaged file, an INFO line for another schema
  version (`vapor logs`).
- `vapor status` shows the whole-scope reconcile that follows.
- To look inside a copy: `sqlite3 -readonly <path>`. The `vapor-debug`
  skill has the queries.
