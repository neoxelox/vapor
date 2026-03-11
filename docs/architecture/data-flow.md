# Data Flow

## Local to remote

1. FSEvents emits path metadata.
2. Daemon callback normalizes/excludes and records path metadata into bounded in-memory event and intent maps.
3. If pending-path caps are exceeded, noisy subtrees compact into a single `RECONCILE_SUBTREE` marker so storms stay bounded until later deferred reconcile stages run.
4. Debounce/coalesce loop emits stabilized intents.
5. Keyed scheduler supersedes stale intents and selects latest action.
6. Planner/hashing/uploader execute under throttle state constraints.
7. Durable queue/state records progress and retry metadata.

## Remote to local (bidirectional MVP)

1. Provider poll fetches remote changes on throttle-aware cadence.
2. Changes are mapped into durable intents with operation IDs.
3. Loop prevention filters self-originated writes.
4. Apply pipeline writes local changes and records conflict/tombstone outcomes.

## Control and observability

- App queries daemon over XPC for status, queue depths, reasons.
- App issues controls: pause/resume, flush-now, auto-launch toggle, excludes updates.
- Diagnostics expose throttle cause, retry state, and conflict outcomes.
