# Data Flow

## Local to remote

1. FSEvents emits path metadata.
2. Daemon callback canonicalizes the watch root, lexically normalizes event paths, rejects traversal/symlink escape cases outside the sync root, and then records filtered path metadata into bounded in-memory event and intent maps.
3. Per-directory 2s storm thresholds (200 unique paths or 600 events) plus a 5000-pending global trigger compact noisy subtrees into deferred `RECONCILE_SUBTREE` markers so storms stop per-path fan-out early.
4. A 250ms debounce/coalesce tick emits stabilized events after conservative per-path quiet windows (shorter for key configs, longer for lockfiles and other unmatched paths).
5. A keyed latest-wins scheduler keeps one intent per path, supersedes stale actions, and requeues dirty paths after in-flight work finishes.
6. A throttle controller evaluates 1s power, thermal, load, disk, network, and activity samples to select `IdleDrain`, `Light`, `Throttled`, or `Suspended`.
7. Planner, hash, upload, and reconcile stages acquire strict throttle-gated work permits before starting; reconcile only starts in `IdleDrain`, yields on slice expiry or throttle changes, and clears compacted subtree boundaries after successful quiet completion.
8. A live daemon runtime loop now wires watcher ingest -> debounce -> scheduler -> durable queue -> workgate -> reconcile, so the local engine runs as one composed pipeline instead of isolated primitives.
9. A SQLite durable queue/state DB persists pending and leased intents, recovers interrupted leases on startup, requeues retryable failures with exponential backoff/jitter/slower rate-limit delays, durably finalizes terminal failures, and injects a whole-scope startup reconcile so volatile pre-DB intent loss is reconstructed conservatively after restart.
10. Provider selection is now injected at runtime startup, so daemon orchestration uses the provider trait boundary instead of hardcoding the Google Drive type in core engine state.
11. Non-reconcile work now flows through a staged executor that leases durable intents into bounded planner, hash, and upload stages under workgate/throttle caps instead of finishing one leased intent at a time.

Current caveat: the reconcile controller is now idle-biased and interruptible, and the runtime loop is composed, but real system-driven throttle sampling and real subtree walking/apply work still need to replace the current placeholders in later hardening milestones.

## Remote to local (bidirectional MVP)

1. Provider poll fetches remote changes on throttle-aware cadence.
2. Changes are mapped into durable intents with operation IDs.
3. Loop prevention filters self-originated writes.
4. Apply pipeline writes local changes and records conflict/tombstone outcomes.

## User resource budgets

User-configurable daemon-process ceilings (`resourceLimits.cpuPercent`, `memoryPercent`, `bandwidthPercent`) and an optional dynamic headroom expansion (`idleBoost`) layer on top of the internal throttle controller and the auto-tuner.

Resolution and enforcement sequence on every tick:

1. Resolve effective ceilings by taking the MIN of the global values and each enabled profile's override; any enabled profile with `idleBoost.enabled = false` disables boost daemon-wide. Profile overrides can only *lower* effective ceilings relative to global values.
2. Sample device-level CPU, memory, and network utilization plus user-idle duration at the same 1s cadence as the throttle controller.
3. Run the idle-boost state machine. Boost engages only when: throttle state is `IdleDrain`, user has been idle for at least `minIdleSeconds`, and non-Vapor utilization is at or below each `headroom*Percent`. Effective ceilings linearly ramp from `resourceLimits.*Percent` toward `boost*Percent` over `rampUpSeconds`; any condition break ramps back down over `rampDownSeconds` (bounded to `<= rampUpSeconds` so activity resumption is non-invasive).
4. Publish effective ceilings and reason codes to: the workgate (CPU ceiling scales planner/hash/upload/download concurrency caps), the provider-neutral bandwidth shaper in `core/providers` (bandwidth ceiling sets a bytes/sec token bucket shared across upload and download, capped at `bandwidthPercent` of measured link capacity so non-Vapor traffic always retains at least `100 - bandwidthPercent` of the link by construction), and memory-reactive paths (storm compaction thresholds, `self_write_cache` TTL, timeline buffer trim).
5. Auto-tuning decisions (60-120s cadence) are constrained to stay inside current effective ceilings and must absorb ceiling changes within one cycle without oscillation.

Invariants:

- Ceilings are hard caps. The throttle controller and auto-tuner must never drive the daemon above them.
- User ceilings never relax the throttle controller — a `Suspended` decision always wins, even under full idle boost.
- When effective CPU ceiling drops below current in-flight concurrency, no new work is admitted but running work proceeds to its next slice checkpoint before yielding; this matches the existing interruptible-reconcile discipline.
- `self_write_cache` TTL and diagnostics buffer length are bounded by documented floors even under memory pressure so loop-prevention and observability guarantees are preserved.

## Control and observability

- App queries daemon over XPC for status, queue depths, reasons.
- App issues controls: pause/resume, flush-now, auto-launch toggle, excludes updates.
- Diagnostics expose throttle cause, retry state, conflict outcomes, current effective resource ceilings, measured utilization, and the active idle-boost state with a human-readable reason.
