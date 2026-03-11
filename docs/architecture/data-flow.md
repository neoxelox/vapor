# Data Flow

## Local to remote

1. FSEvents emits path metadata.
2. Daemon callback normalizes/excludes and records path metadata into bounded in-memory event and intent maps.
3. If pending-path caps are exceeded, noisy subtrees compact into a single `RECONCILE_SUBTREE` marker so storms stay bounded until later deferred reconcile stages run.
4. A 250ms debounce/coalesce tick emits stabilized events after conservative per-path quiet windows (shorter for key configs, longer for lockfiles and other unmatched paths).
5. A keyed latest-wins scheduler keeps one intent per path, supersedes stale actions, and requeues dirty paths after in-flight work finishes.
6. Planner/hashing/uploader execute under throttle state constraints.
7. Durable queue/state records progress and retry metadata.

Current caveat: `RECONCILE_SUBTREE` markers now bridge into the scheduler, but the later storm/deferred-reconcile stages still need to own clearing compacted subtree boundaries and reconciling any already-scheduled descendant work.

## Remote to local (bidirectional MVP)

1. Provider poll fetches remote changes on throttle-aware cadence.
2. Changes are mapped into durable intents with operation IDs.
3. Loop prevention filters self-originated writes.
4. Apply pipeline writes local changes and records conflict/tombstone outcomes.

## Control and observability

- App queries daemon over XPC for status, queue depths, reasons.
- App issues controls: pause/resume, flush-now, auto-launch toggle, excludes updates.
- Diagnostics expose throttle cause, retry state, and conflict outcomes.
