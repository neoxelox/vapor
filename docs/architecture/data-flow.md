# Data Flow

## Local to remote

1. FSEvents emits path metadata.
2. Daemon callback normalizes/excludes and records path metadata into bounded in-memory event and intent maps.
3. Per-directory 2s storm thresholds (200 unique paths or 600 events) plus a 5000-pending global trigger compact noisy subtrees into deferred `RECONCILE_SUBTREE` markers so storms stop per-path fan-out early.
4. A 250ms debounce/coalesce tick emits stabilized events after conservative per-path quiet windows (shorter for key configs, longer for lockfiles and other unmatched paths).
5. A keyed latest-wins scheduler keeps one intent per path, supersedes stale actions, and requeues dirty paths after in-flight work finishes.
6. A throttle controller evaluates 1s power, thermal, load, disk, network, and activity samples to select `IdleDrain`, `Light`, `Throttled`, or `Suspended`.
7. Planner, hash, upload, and reconcile stages acquire strict throttle-gated work permits before starting; reconcile only starts in `IdleDrain`, yields on slice expiry or throttle changes, and clears compacted subtree boundaries after successful quiet completion.
8. A SQLite durable queue/state DB persists pending and leased intents, recovers interrupted leases on startup, requeues retryable failures with exponential backoff/jitter/slower rate-limit delays, and durably finalizes terminal failures.

Current caveat: the reconcile controller is now idle-biased and interruptible, but future provider/local scan milestones still need to plug real subtree walking/apply work into that control loop.

## Remote to local (bidirectional MVP)

1. Provider poll fetches remote changes on throttle-aware cadence.
2. Changes are mapped into durable intents with operation IDs.
3. Loop prevention filters self-originated writes.
4. Apply pipeline writes local changes and records conflict/tombstone outcomes.

## Control and observability

- App queries daemon over XPC for status, queue depths, reasons.
- App issues controls: pause/resume, flush-now, auto-launch toggle, excludes updates.
- Diagnostics expose throttle cause, retry state, and conflict outcomes.
