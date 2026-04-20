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

### Ceiling transitions (idle-boost and throttle interaction)

Idle-boost and the throttle controller update asynchronously on the same 1s cadence, so a mid-ramp throttle transition must be resolved deterministically:

1. **Throttle exit from `IdleDrain`.** When throttle state transitions from `IdleDrain` to `Light`, `Throttled`, or `Suspended` while boost is active or mid-ramp, the effective ceiling snaps immediately to `min(current_ramped_value, resourceLimits.*Percent)` and boost is marked disengaged. No down-ramp occurs in this path; the cap collapses to the base ceiling in the same tick to guarantee the post-IdleDrain throttle state never runs against boosted caps.
2. **Throttle return to `IdleDrain`.** When throttle returns to `IdleDrain` after having left it, boost is not auto-resumed. The idle-boost state machine re-evaluates all gating conditions (`minIdleSeconds` since last user input, `headroom*Percent` samples, `idleBoost.enabled`) from scratch and, if they pass, starts a fresh up-ramp from `resourceLimits.*Percent`. This prevents boost from re-engaging off stale conditions after even a brief throttle excursion.
3. **Graceful boost exit (throttle stays `IdleDrain`, gating condition fails).** If boost is active and a non-throttle condition breaks (user HID input, non-Vapor utilization exceeds headroom, `idleBoost.enabled` flips to `false` via config reload), ceilings ramp down linearly over `rampDownSeconds` back to `resourceLimits.*Percent`. `rampDownSeconds` must be `<= rampUpSeconds` so activity resumption is non-invasive.
4. **Config reload mid-ramp.** Lowering `resourceLimits.*Percent` mid-ramp immediately clamps the current ramped value to the new (lower) base ceiling; raising it does not retroactively raise the ramp target, which remains the original `boost*Percent`. Changing `boost*Percent` or `rampUpSeconds` mid-ramp snaps the in-progress ramp to the new targets on the next tick (no restart, no glitch).
5. **In-flight work during snap-down.** When the effective ceiling drops below current in-flight concurrency as a result of any of the above, no new work is admitted but running work proceeds to its next slice checkpoint before yielding — same discipline as §"Invariants" above.

## Conflict handling

When local and remote versions of the same path diverge (both sides modified, or rename collides with an existing name), the default policy is "keep both; never silent overwrite." Concrete mechanics:

- **Winner and loser.** The side whose provider op-id completes first retains the canonical path. The losing side is renamed in-place to a derived "conflict copy" path.
- **Suffix template.** `{stem}~conflict-{device_id}-{timestamp_ms}{ext}` where `{stem}` and `{ext}` are the original basename split at the last `.`, `{device_id}` is a hostname-derived stable identifier persisted once at first-run under `vapor.json`, and `{timestamp_ms}` is the event time in UTC milliseconds (monotonic-within-device).
- **Device identifier.** Sourced at first-run from `gethostname()` normalized to `[a-z0-9-]` (non-matching characters stripped, length capped at 32). If empty after normalization, fall back to a generated UUIDv4 truncated to 12 characters. The resolved value is persisted in `vapor.json` as `deviceId` and never silently regenerated; changing machine hostnames does not change `deviceId` once persisted.
- **Collision-avoidance fallback.** If the derived conflict path already exists (on either side), append `-{seq}` starting at `2` and increment until free. Fallback template becomes `{stem}~conflict-{device_id}-{timestamp_ms}-{seq}{ext}`.
- **Rename-during-conflict.** If the loser is renamed by the user or remote during the conflict write, the conflict copy still lands at the resolved fallback path; the user-initiated rename becomes a separate follow-up intent through the normal scheduler.
- **Tombstone interaction.** If one side has deleted the path while the other side has a modified version, the modification wins and is written to the original canonical path (no conflict suffix); the delete is recorded as a completed tombstone. "Delete wins" is never the default — data preservation always wins over deletion.
- **Determinism.** All inputs to the suffix (device_id, timestamp_ms) must be derivable from durable state or event metadata, so the same conflict replayed on the same device produces the same conflict path across daemon restarts.

## Loop prevention (self-write cache)

Bidirectional sync must prevent the daemon from re-uploading changes it just applied from remote (and symmetrically re-downloading changes it just uploaded). The `self_write_cache` is an in-memory ring with durable-operation backing:

- **Record on every provider write.** On completion of any `upload`, `download`, `delete`, or `rename` issued by the daemon, record `(remote_path, op_id, content_hash, expiry_monotonic_ms)` into the cache.
- **Match on every inbound provider event.** Provider changes-feed events are matched against the cache before becoming intents. Primary correlator is the provider's `op_id` tag (xattr on filesystems that support it; a side-file fallback at `{path}.vapor-meta.json` when xattr is unavailable or write-failed). Content-hash is the fallback correlator when `op_id` is absent (e.g., third-party tool wrote the same bytes). A hit suppresses intent creation and records the suppression in diagnostics.
- **Eviction and bounds.** TTL and max-entries are defined in `core/shared/src/constants.rs` as a new module `self_write_cache` with `DEFAULT_TTL_MILLIS: u64 = 30_000`, `MIN_TTL_MILLIS: u64 = 5_000`, `MAX_ENTRIES: usize = 10_000`, `MIN_ENTRIES: usize = 1_000`. Eviction policy is LRU-on-insert; TTL expiry runs on the same 1s tick as throttle sampling. Memory-pressure floor: under memory pressure the cache may shorten TTL toward `MIN_TTL_MILLIS` and trim toward `MIN_ENTRIES`, but never below those floors (loop-prevention is a safety guarantee, not an opportunistic feature).
- **xattr vs side-file precedence.** Writes attempt xattr first; on `ENOTSUP`/`EACCES`/`EROFS` the fallback side-file is written atomically alongside the payload. Reads check xattr first, then the side-file; if both are present the xattr wins. The filesystem provider (Phase 3) is responsible for hiding side-files from enumeration so they do not surface as independent intents.
- **Crash safety.** The cache is purely in-memory. A daemon crash followed by restart forces a conservative whole-scope reconcile (already present in the runtime loop) which re-establishes baseline state without needing the cache to persist across restarts; cached entries for in-flight writes are re-derived from durable queue recovery.

## Multi-profile watch coordination

When multiple enabled profiles target overlapping local roots, the watcher must dedupe while keeping per-profile state isolated:

- **One watcher per distinct canonical local root.** Profile startup computes the canonical realpath of each profile's local root. Profiles sharing a canonical root share one FSEvents watcher; non-overlapping roots each get their own watcher.
- **Per-profile event fan-out.** On each raw FSEvents callback, the normalized event is matched against every enabled profile's sync-root prefix + ignore rules. Profiles that match each receive an independent copy of the event in their own bounded ingest queue, tagged with `profile_id`. Profiles that do not match do not see the event.
- **Per-profile debounce, scheduler, durable queue.** Each profile has its own debounce/coalesce tick, keyed scheduler, and durable queue tables (profile-id-keyed in SQLite). No in-memory structure is shared across profiles below the raw watcher level.
- **Shared workgate, throttle, resource ceilings.** The workgate, throttle controller, bandwidth shaper, and effective resource ceilings are daemon-level (single process serving all profiles). Profile-override MIN-lowering resolves to a single effective ceiling set that gates all profiles; per-profile work still queues behind the shared workgate under the shared caps.
- **Blast-radius containment.** A panic or error inside one profile's scheduler, reconcile, or provider execution must not kill the watcher or other profiles. Profile runtimes are spawned in tasks wrapped with a panic catcher; a caught panic marks the profile `Failed` with a durable diagnostic, suspends its queue, and leaves the watcher and other profiles running.
- **Same-path double-write safety.** If two profiles target the same provider account and the same remote subtree, they are allowed to coexist but a write from profile A and a write from profile B to the same file are treated as simultaneous writes and resolved by the conflict-handling rules above (keep both). Provider op-ids carry the originating `profile_id` so self-write-cache matches remain correctly scoped.
- **Crash recovery.** Restart reloads the enabled profile set, reconstructs watchers per distinct canonical root, and issues per-profile whole-scope reconcile intents at startup (profile-keyed) so each profile's volatile pre-DB intent loss is reconstructed independently.

## Control and observability

- App queries daemon over XPC for status, queue depths, reasons.
- App issues controls: pause/resume, flush-now, auto-launch toggle, excludes updates.
- Diagnostics expose throttle cause, retry state, conflict outcomes, current effective resource ceilings, measured utilization, and the active idle-boost state with a human-readable reason.
