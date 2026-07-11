# Data Flow

## Local to remote

1. The platform fs-watch source emits path metadata (FSEvents on macOS, ReadDirectoryChangesW on Windows, inotify on Linux).
2. Daemon callback canonicalizes the watch root once at startup, then for each event performs only lexical path normalization, watch-root prefix check, and ignore-rule filtering before pushing the record onto a bounded incoming-events queue. Per-component symlink resolution is deliberately NOT done in the callback. Paired renames split into a delete for the source path and a create for the destination so both sides stay actionable. When the callback observes a change to an ignore file, it flags the shared path filter for a rebuild that the runtime thread performs between ticks — ignore-rule edits apply without a daemon restart. Events dropped at the bounded-queue boundary are counted and repaired by a deferred whole-scope reconcile scheduled on the next drain.
3. The runtime thread drains the incoming queue into the bounded in-memory event/intent maps on its next tick, where it ALSO performs full per-component symlink resolution against the real filesystem; events whose paths resolve outside the watch root (symlink escape, traversal through symlink chains) are dropped with a diagnostic and never enter the scheduler.
4. Per-directory 2s storm thresholds (200 unique paths or 600 events) compact noisy subtrees strictly *below* the watch root into deferred `RECONCILE_SUBTREE` markers so storms stop per-path fan-out early; the watch root itself compacts only via the separate 5000-pending global trigger, so a busy-but-healthy tree cannot collapse the whole scope. Continued churn defers a subtree's reconcile until quiet, bounded by `DEFERRED_RECONCILE_MAX_DELAY_MILLIS` from first detection so a permanently-busy subtree still converges.
5. A 250ms debounce/coalesce tick emits stabilized events after conservative per-path quiet windows (shorter for key configs, longer for lockfiles and other unmatched paths).
6. A keyed latest-wins scheduler keeps one intent per path, supersedes stale actions, and requeues dirty paths after in-flight work finishes.
7. A throttle controller evaluates 1s power, thermal, load, disk, network, and activity samples to select `IdleDrain`, `Light`, `Throttled`, or `Suspended`.
8. Planner, hash, upload, and reconcile stages acquire strict throttle-gated work permits before starting; reconcile only starts in `IdleDrain`, yields on slice expiry or throttle changes, and clears compacted subtree boundaries after successful quiet completion.
9. A live daemon runtime loop wires watcher ingest -> incoming queue drain + symlink resolution -> debounce -> scheduler -> durable queue -> workgate -> reconcile, so the local engine runs as one composed pipeline instead of isolated primitives. The loop is event-nudged: fs-event callbacks and IPC control requests signal a tick waker, and a fully idle daemon relaxes from the 250 ms work cadence to a 1 s cadence. Exactly one daemon may serve a `vapor_dir` (an OS advisory lock on `vapord.lock` enforces it). `Pause` semantics: ingest/debounce/durable-flush keep capturing intent state and in-flight work finishes, but no new work is released or leased until `Resume`.
10. A SQLite durable queue/state DB (WAL, `synchronous = NORMAL`) persists pending and leased intents, recovers interrupted leases on startup (resetting `attempt_count` for leases older than `LEASE_TIMEOUT_MILLIS`) and sweeps stale leases periodically in-run, coalesces scheduler flushes per `(path, kind)` against already-pending rows inside one transaction, requeues retryable failures with exponential backoff/jitter/slower rate-limit delays (with `attempt_count` incremented only by the retry path, not by leasing), durably finalizes terminal failures, and injects a whole-scope startup reconcile (bounded by `STARTUP_RECONSTRUCTION_BARRIER_DEADLINE_MILLIS` to avoid starving non-reconcile work) so volatile pre-DB intent loss is reconstructed conservatively after restart.
11. Provider selection is injected at runtime startup per profile, so daemon orchestration uses the provider trait boundary instead of hardcoding any cloud type in core engine state. `provider = "filesystem"` (default) selects the real filesystem provider; `provider = "gdrive"` selects `GoogleDriveProvider`. New backends onboard through the checklist in `provider-onboarding.md`.
12. Non-reconcile work flows through a staged executor that leases durable intents into bounded planner, hash, transfer (upload/download), and apply-delete stages under workgate/throttle caps instead of finishing one leased intent at a time. Transfers are chunked `TransferSession`s: each tick grants a bounded byte budget (shaped by the bandwidth token bucket and the auto-tuned step size), so a large file never monopolizes a tick and a `Suspended` throttle holds a transfer at its checkpoint instead of aborting it.

Current caveat: real system-driven throttle metrics sampling (power/thermal/HID) still uses conservative placeholders on some hosts until the remaining native `MetricsSampler`/`IdleNotifier` bridges land; the pipeline itself (watch → debounce → durable queue → staged execution → reconcile walk) runs real work end to end.

## Local safeguards (optional advanced protections)

- **Active-coding heuristic (C8-55).** Stabilized code/config-class events feed a rolling 60s window; at or above the threshold the runtime ORs `user_active = true` into the throttle inputs, so a compile-edit loop throttles sync even on hosts without a permissioned HID-idle signal. Strictly additive — it can only raise throttle caution.
- **Priority classes + flush boost (C8-56).** Within one durable flush batch, key-config and code paths enqueue ahead of lockfile noise (reusing the debounce classification as the priority signal). An explicit `vapor flush` activates a bounded 30s boost window: deferred reconciles release immediately (bypassing not-before times and the idle gate) and the remote feed polls on the next tick. Execution still answers to the throttle ladder, so flush accelerates scheduling, never resource impact.
- **Mass-change / ransomware guard (C8-57).** 200+ local deletions inside 60s (post-echo-suppression, so the engine's own applied deletes never count) pause the daemon in the same tick, raise a `guard` timeline alert, and set an actionable status reason. Ingest keeps capturing intent durably while paused. `vapor resume` is the explicit human reset and re-arms the guard with an empty window.

## Remote to local (bidirectional MVP)

1. Provider poll fetches remote changes on throttle-aware cadence.
2. Changes are mapped into durable intents with operation IDs. A change
   whose local-equivalent path matches the ignore rules is dropped here
   (counted as `ignored_changes`): ignore filtering is symmetric, so an
   ignored name never syncs in either direction. The reconcile
   comparison walk applies the same rules to both the local and the
   remote side of every directory pair — an ignored name (`.DS_Store`,
   `node_modules/`) is never descended into, never uploaded, never
   downloaded, and can never manufacture a keep-both conflict copy.
3. Loop prevention filters self-originated writes.
4. Apply pipeline writes local changes and records conflict/tombstone outcomes.

## Sync modes (directionality)

`syncMode` selects which direction changes may flow. Full design (semantics,
safety, config surface, rollout order) lives in `sync-modes.md`; the pipeline
touch-points are:

- **`two-way`** (default) — both the local→remote and remote→local pipelines
  above run; divergence resolves by the keep-both conflict policy below.
- **`pull-only`** (cloud → local, strict mirror) — the local→remote pipeline
  is gated off: local watcher events do not produce upload/remote-delete/
  remote-rename intents. The remote→local apply and reconcile actively make
  local match cloud, including reverting local edits to the cloud canonical
  and removing local-only files. Cloud is the source of truth; no keep-both.
- **`push-only`** (local → cloud, strict mirror) — the remote→local pipeline
  is gated off: nothing is written, deleted, or reverted locally. The
  local→remote apply and reconcile make cloud match local, including
  overwriting divergent remote files and removing cloud-only files. Local is
  the source of truth; no keep-both.

The mode is a property of the sync scope / profile, so every operation
(upload, download, delete, rename, revert) flows through the same gate — no
operation bypasses it. `self_write_cache` is orthogonal and stays active in
every mode. One-way modes are strict-mirror and can delete/overwrite the
subordinate side, so they are opt-in per profile with the safety
requirements in `sync-modes.md §Safety`. `syncMode` is per-profile and
categorical (the top-level value is the default; each profile may override
outright — not MIN-lowering), so one device can run several `pull-only`
mirror profiles alongside a `two-way` profile.

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

## Directory and symlink semantics

This section owns the "what syncs" contract: Vapor syncs file content,
and everything else on a filesystem is handled deliberately. The table
summarizes the contract; the prose below carries the engine-level
mechanics and the "why":

| Item | How Vapor treats it |
| ---- | ------------------- |
| Regular files | Fully synced, byte-for-byte, in both directions (following the configured sync direction). |
| Folders | Containers, not synced objects: each side creates them automatically as the files inside them sync, and deleting a folder propagates. An empty folder does not appear on the other side, and renaming a folder re-syncs its contents under the new name. |
| Ignored files | Anything matching the ignore rules never syncs in either direction — it can't be pulled down from the cloud, and it never causes a conflict. |
| Conflicting edits | When both sides change the same file, both versions are kept — the other device's version stays next to the local one as a `~conflict-` copy until resolved. Nothing is ever silently overwritten. |
| Symlinks | Never followed and never synced: a link could pull content from outside the sync root into scope, and cloud providers can't represent them faithfully. |
| Hard links | Synced as an ordinary independent file; the link relationship is not preserved on the other side. |
| Special files (pipes, sockets, devices) | Ignored entirely — they carry no transferable content and never block the files around them. |
| Metadata (permissions, extended attributes, timestamps) | Not synced; content only. Vapor's own bookkeeping tags (op-id xattrs / side-files) stay invisible and never appear in listings. |

**The engine is file-only by decision: directories are implicit
containers, not synced objects.** They materialize on the other side only
through the files inside them: uploads create the remote parent chain,
downloads create local parent directories, and the executor no-ops a
directory upload intent outright ("directories materialize through their
children"). The reconcile walk descends into directories but never emits
an intent for the directory itself. Consequences: an **empty folder does
not sync** in either direction, and a **folder rename/move propagates as
a recursive delete plus child-by-child re-upload** rather than one
rename (data-safe, just not cheap). Directory *deletions* do propagate
(both directions, including strict-mirror removals). First-class folder
sync was evaluated and rejected (project-owner decision, 2026-07-07):
keeping every synced object content-shaped is what keeps the conflict,
echo-suppression, and transfer machinery simple — folders cannot
"conflict", and parent materialization is idempotent by construction.

**Special files (FIFOs, sockets, device nodes) are inert.** The executor
refuses them at planning and the reconcile walk skips them — this must
stay an *explicit* guard, not an accident of ordering: hashing a FIFO
blocks until a writer appears, and without the guard the intent sat in
the durable queue forever as a permanently-`WaitingForHash` row. Hard
links are indistinguishable from regular files at the path level and
sync as independent files (the link relationship is not preserved).

**Symlinks are outside the sync contract entirely.** They are never
followed, never uploaded, and never created locally: the reconcile walk
and the conflict scan skip them, the executor no-ops upload intents for
them, and the filesystem provider treats symlinks inside the cloud root
as invisible while its scope enforcement rejects any path that resolves
through a symlink to *outside* the configured root (classic
path-traversal risk). Event paths are symlink-resolved per component on
the runtime thread, and events that resolve outside the watch root are
dropped with a logged warning. This is deliberate: following symlinks
would let one link pull an arbitrary external tree (or a cycle) into
sync scope, cloud providers have no faithful symlink representation, and
Windows symlink creation requires elevated privileges — so the safe,
portable contract is "symlinks are invisible to sync."

## Conflict handling

This section describes the **`two-way`** policy. In the one-way `pull-only`
and `push-only` modes there is a declared source of truth, so divergence is
resolved in favor of the authoritative side with no conflict copy (see
`sync-modes.md`). Keep-both remains the default because `two-way` is the
default mode.

This section owns conflict *creation*. Everything after a copy exists —
notification, listing, and resolution through `vapor conflicts` and the
app surfaces — lives in `conflict-resolution.md`.

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
- **Local watcher echoes verify current content, never the tag alone.** The local-side twin of the cache (suppressing watcher echoes of download-applies) requires the file's *current* size and content hash to match what the daemon wrote: the op-id tag survives later writes, so a tag-only match would keep suppressing genuine user edits for the record's whole TTL after a download.
- **Eviction and bounds.** TTL and max-entries are defined in `core/shared/src/constants.rs` as a new module `self_write_cache` with `DEFAULT_TTL_MILLIS: u64 = 30_000`, `MIN_TTL_MILLIS: u64 = 5_000`, `MAX_ENTRIES: usize = 10_000`, `MIN_ENTRIES: usize = 1_000`. Eviction policy is LRU-on-insert; TTL expiry runs on the same 1s tick as throttle sampling. Memory-pressure floor: under memory pressure the cache may shorten TTL toward `MIN_TTL_MILLIS` and trim toward `MIN_ENTRIES`, but never below those floors (loop-prevention is a safety guarantee, not an opportunistic feature).
- **xattr vs side-file precedence.** Writes attempt xattr first; on `ENOTSUP`/`EACCES`/`EROFS` the fallback side-file is written atomically alongside the payload. Reads check xattr first, then the side-file; if both are present the xattr wins. The filesystem provider (Phase 3) is responsible for hiding side-files from enumeration so they do not surface as independent intents.
- **Crash safety.** The cache is purely in-memory. A daemon crash followed by restart forces a conservative whole-scope reconcile (already present in the runtime loop) which re-establishes baseline state without needing the cache to persist across restarts; cached entries for in-flight writes are re-derived from durable queue recovery.

## Multi-profile watch coordination

When multiple enabled profiles target overlapping local roots, the watcher must dedupe while keeping per-profile state isolated:

- **One watcher per distinct canonical local root.** Profile startup computes the canonical realpath of each profile's local root. Profiles sharing a canonical root share one fs-watch watcher; non-overlapping roots each get their own watcher.
- **Per-profile event fan-out.** On each raw fs-watch callback, the normalized event is matched against every enabled profile's sync-root prefix + ignore rules. Profiles that match each receive an independent copy of the event in their own bounded ingest queue, tagged with `profile_id`. Profiles that do not match do not see the event.
- **Per-profile debounce, scheduler, durable queue.** Each profile has its own debounce/coalesce tick, keyed scheduler, and durable queue. Implementation note: isolation is per-profile SQLite *files* (`state/profiles/<id>/vapor.sqlite`), not profile-keyed tables in one file — a corrupt or quarantined profile DB then cannot take out its siblings. The implicit `default` profile keeps the legacy `state/vapor.sqlite` path so single-scope setups upgrade in place. No in-memory structure is shared across profiles below the raw watcher level.
- **Shared workgate, throttle, resource ceilings.** The workgate, throttle controller, bandwidth shaper, and effective resource ceilings are daemon-level (single process serving all profiles). Profile-override MIN-lowering resolves to a single effective ceiling set that gates all profiles; per-profile work still queues behind the shared workgate under the shared caps.
- **Blast-radius containment.** A panic or error inside one profile's scheduler, reconcile, or provider execution must not kill the watcher or other profiles. Profile runtimes are spawned in tasks wrapped with a panic catcher; a caught panic marks the profile `Failed` with a durable diagnostic, suspends its queue, and leaves the watcher and other profiles running.
- **Same-path double-write safety.** If two profiles target the same provider account and the same remote subtree, they are allowed to coexist but a write from profile A and a write from profile B to the same file are treated as simultaneous writes and resolved by the conflict-handling rules above (keep both). Provider op-ids carry the originating `profile_id` so self-write-cache matches remain correctly scoped.
- **Crash recovery.** Restart reloads the enabled profile set, reconstructs watchers per distinct canonical root, and issues per-profile whole-scope reconcile intents at startup (profile-keyed) so each profile's volatile pre-DB intent loss is reconstructed independently.

## Control and observability

- App queries daemon over IPC for status, queue depths, reasons (see `docs/architecture/ipc-contracts.md`).
- App issues controls: pause/resume, flush-now, auto-launch toggle, excludes updates.
- Diagnostics expose throttle cause, retry state, conflict outcomes, current effective resource ceilings, measured utilization, and the active idle-boost state with a human-readable reason.
