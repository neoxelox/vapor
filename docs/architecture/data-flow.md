# Data Flow

## Local to remote

1. The platform fs-watch source emits path metadata (FSEvents on macOS, ReadDirectoryChangesW on Windows, inotify on Linux).
2. Daemon callback canonicalizes the watch root once at startup, then for each event performs only lexical path normalization, watch-root prefix check, and ignore-rule filtering before pushing the record onto a bounded incoming-events queue. Per-component symlink resolution is deliberately NOT done in the callback. Paired renames split into a delete for the source path and a create for the destination so both sides stay actionable. When the callback observes a change to an ignore file, it flags the shared path filter for a rebuild. The rebuild runs on a worker, the runtime swaps the new rules in between ticks, and a whole-scope reconcile follows, so a rule change applies without a daemon restart and to the files that already exist. A change to the rule keys of `vapor.json` takes the same path. Events dropped at the bounded-queue boundary are counted and repaired by a deferred whole-scope reconcile scheduled on the next drain.
3. The runtime thread drains the incoming queue into the bounded in-memory event/intent maps on its next tick, where it ALSO performs full per-component symlink resolution against the real filesystem; events whose paths resolve outside the watch root (symlink escape, traversal through symlink chains) are dropped with a diagnostic and never enter the scheduler.
4. Per-directory 2s storm thresholds (200 unique paths or 600 events) compact noisy subtrees strictly *below* the watch root into deferred `RECONCILE_SUBTREE` markers so storms stop per-path fan-out early; the watch root itself compacts only via the separate 5000-pending global trigger, so a busy-but-healthy tree cannot collapse the whole scope. A compaction takes its burst with it: the windows of the folders above forget those paths, so one save in a folder next to it does not compact their common parent. The reconcile of a compacted folder starts a fixed time after the storm in it ends: 1.5 seconds in `Idle` (`DEFERRED_RECONCILE_IDLE_QUIET_RELEASE_MILLIS`), 30 seconds in the other states. The storm is over when the folder takes in fewer than `STORM_ABSORBED_BUSY_EVENTS` events in a window, so ordinary work in it (a save a second) does not push the reconcile back. A storm that goes on is scanned all the same: 30 seconds after it was first seen in `Idle`, and `DEFERRED_RECONCILE_MAX_DELAY_MILLIS` after in the other states. The compaction ends when its reconcile is released, not when the walk is done: the walk lists the folder after that moment, so it sees every change the compaction took in, and a later change travels as its own event again. While a folder waits for its scan, `vapor status` reports `reconcile.state: waiting` with the number of changes it holds and the folder.
5. A 250ms debounce/coalesce tick emits stabilized events after per-path quiet windows (shorter for key configs, longer for lockfiles). A file whose type has no class waits one second when its writes came in one go (a copy, a save): how a file was written says more about whether it is complete than its name does. Only a file whose writes are spread over more than a second, which is what a file that still grows looks like, waits the longer 4 seconds.
6. A keyed latest-wins scheduler keeps one intent per path, supersedes stale actions, and requeues dirty paths after in-flight work finishes.
7. A throttle controller evaluates 1s power, thermal, load, disk, network, and activity samples to select `Idle`, `Light`, `Throttled`, or `Suspended`.
8. Planner, hash, upload, and reconcile stages acquire strict throttle-gated work permits before starting. A reconcile scan starts in every state but `Suspended`, at the caps of that state. A whole-scope reconcile that is requested while one walks (a rule that changed, dropped events, `vapor reconcile`) gets a walk of its own after it, because the walk under way already passed some folders. A scan spends a planner slot and a read token, yields on slice expiry or throttle changes, and clears compacted subtree boundaries after successful quiet completion. The listing of a cloud folder runs on a thread of its own, and the walk asks for the listings of the next folders ahead: 8 at one time in `Idle` (`RECONCILE_LISTINGS_IN_FLIGHT`), 4 in `Light`, 1 in `Throttled`. The folders are still compared one by one, in the order of the walk, each with the time its listing was asked for. A walk whose slice ends while it waits for a listing is due again at once, so the answer of the cloud is what moves it on and no timer stands between two folders. A compacted subtree is released at the end of its deferral in every state but `Suspended`, so a burst on a device on battery still syncs. In `Idle` it is released as soon as the burst is over, and its scan is due at that moment. A whole-scope reconcile runs at startup, once each 24 hours (`WHOLE_SCOPE_RECONCILE_INTERVAL_SECONDS`), and on the user's request: `vapor reconcile` (the app's Sync now) queues one at once and opens the reconcile boost window. While a queued scan waits, `vapor status` reports `reconcile.state: waiting` with the reason, so a machine in use never looks stuck without saying why.
9. A live daemon runtime loop wires watcher ingest -> incoming queue drain + symlink resolution -> debounce -> scheduler -> durable queue -> workgate -> reconcile, so the local engine runs as one composed pipeline instead of isolated primitives. The loop is event-nudged: fs-event callbacks and IPC control requests signal a tick waker, and a fully idle daemon relaxes from the 250 ms work cadence to a 1 s cadence. Exactly one daemon may serve a `vapor_dir` (an OS advisory lock on `vapord.lock` enforces it). On a shutdown signal the loop stabilizes every local event still inside its debounce window and writes the resulting intents to the durable queue before exiting (`intents_flushed` in the shutdown log line), so a change reported moments before SIGTERM does not depend on the next startup reconcile to be found. `Pause` semantics: ingest/debounce/durable-flush keep capturing intent state and in-flight work finishes, but no new work is released or leased until `Resume`.
10. A SQLite durable queue/state DB (WAL, `synchronous = NORMAL`) persists pending and leased intents, recovers interrupted leases on startup (resetting `attempt_count` for leases older than `LEASE_TIMEOUT_MILLIS`) and sweeps stale leases periodically in-run, coalesces scheduler flushes per `(path, kind)` against already-pending rows inside one transaction, requeues retryable failures with exponential backoff/jitter/slower rate-limit delays (with `attempt_count` incremented only by the retry path, not by leasing), durably finalizes terminal failures, and injects a whole-scope startup reconcile (bounded by `STARTUP_RECONSTRUCTION_BARRIER_DEADLINE_MILLIS` to avoid starving non-reconcile work) so volatile pre-DB intent loss is reconstructed conservatively after restart. Every write of one tick is one transaction, and so one write to the log of the database: under a burst of small files a write for each statement cost a fifth of the tick thread, and on macOS each one waits behind the flush of the drive that the transfers of the moment ask for. A daemon that is killed inside a tick loses the writes of that tick and nothing else; its intents are leased or pending again at the next start. A tick that fails or panics keeps what it wrote before. A command that writes to the same database (an answer to a decision) asks for the lock each millisecond, so it gets its turn between two ticks of a busy daemon within a third of a second.
11. Provider selection is injected at runtime startup per profile, so daemon orchestration uses the provider trait boundary instead of hardcoding any cloud type in core engine state. `provider = "filesystem"` (default) selects the real filesystem provider; `provider = "gdrive"` selects `GoogleDriveProvider`. New backends onboard through the checklist in `provider-onboarding.md`.
12. Non-reconcile work flows through a staged executor that leases durable intents into bounded planner, hash, transfer (upload/download), and apply-delete stages under workgate/throttle caps instead of finishing one leased intent at a time. Every blocking provider call (remote stats and hash probes during planning, `begin_upload`/`begin_download`, transfer-session steps, remote deletes) runs on a small provider-job worker pool, never on the tick thread. Hashing a local file runs on the same workers, and so does every other read of a local file's content: the probe of a cloud deletion reads the local files whose size is the indexed size and whose modification time is not, and a download job reads the local file its payload displaces when the size of that file could match. The tick thread uses such a hash only for a file that still has the size and the modification time the worker saw; a file with no usable hash (larger than one hash step, changed since, unreadable) counts as changed, which keeps it. The reads of a deletion's probe stop when the throttle puts hashing on hold, and a deletion whose check stopped part of the way is tried again; it is not decided on the files nobody read. A file that a transfer writes is durable before it gets its name: its bytes are flushed to the drive first (`vapor_platform::fs_ops::make_durable`). On macOS the costly part of that flush is for the whole drive, so the files that are written at the same moment share one flush; each still waits for a flush that started after its own bytes reached the drive. The same holds for a discard: when the volume of a file refuses a trash folder, a worker copies the file to the trash of the runtime directory and removes the original, and the tick thread only renames. The tick loop dispatches jobs and harvests their outcomes, so provider RTT and a large file stall neither fs-event draining nor debounce nor IPC status, and the upload/download concurrency caps buy real parallel transfers. The calls outside the executor follow the same rule with one thread per call: the changes poll, each directory listing of the reconcile walk, the walk's comparison of a file a crash left set aside with the file at its name, and the cloud-root retry start on one tick and are harvested on a later one (the walk resumes at the directory whose listing landed). An expired changes cursor is dropped, the whole-scope reconcile that repairs the gap is queued, and the next poll is the baseline. No reconcile walk starts while the feed waits for a cursor (the first poll after a start, the baseline after an expiry or after a root that came back), so each listing of a walk is newer than the cursor and a change is in the walk or in the feed. The wait ends with the answer of that poll, whatever it is, and it does not apply to a provider with no feed. When the answer is a failure (the device is offline at the start), a walk may start with no cursor; the baseline that answers later queues one more whole-scope reconcile, which finds a change made between that walk and the baseline. A daemon profile's first cloud-root check runs on a worker at its first tick, and the run state is `starting` until it answers, so IPC answers at once. Transfers remain chunked `TransferSession`s, and each provider owns its chunk size: workers re-check the throttle gates and draw a byte grant from the bandwidth token bucket between steps, so a `Suspended` throttle hands the session back at its checkpoint instead of aborting it, and an empty bucket holds the session until the bucket's wait is over. A step that moves more than its grant (a provider's chunk has a least size, 256 KiB for a Google Drive resumable upload) leaves the bucket in debt for the difference, and the next grant waits until the refill has paid it. Cheap stage transitions chain within one tick (lease → plan, permit-acquire → first hash chunk, hash-complete → transfer dispatch), so a small file no longer pays a fixed tick of latency per pipeline stage; durable leasing orders by priority class (`state-schema-migrations.md` §Current schema) so reconcile backlog never starves fresh edits.
13. A file present on one side only is not always a file to transfer. When the sync index has a row for it and the surviving copy is exactly what was last synced (the rsync quick check on that side, or the provider's hash), the other side deleted it while no daemon was watching, and the walk propagates that deletion: a `Delete` for a file gone from this device, an `ApplyRemoteDelete` (into the trash) for a file gone from the cloud. A surviving copy that changed since the last sync is kept and transferred instead, and a pair the index cannot vouch for is transferred. The same evidence rule holds for a deletion that the watcher reports: with no index row for the path, the planner keeps the cloud object and downloads it again, because Vapor never deletes a cloud object that it did not compare, and a file that is back when the deletion is planned drops the deletion. A modification time never decides that a deletion wins; the sync index is the only evidence. Both deletion kinds still pass the executor's own guard and the mass-delete guard, so a wiped side becomes a question, never a mirror. An `apply` answer to that question lets the held deletions pass the guard and nothing more: a cloud file with no index row (another device added it to a folder that was deleted here) still stays. Only an answer that names the path itself (the side of a `type-mismatch` to remove) stands in for the row. A whole-scope walk after a `reattach` or `recreate` answer runs with `reconcile.merge_without_deletions` set and transfers every one-sided file. The walk also removes the index rows of names that neither side holds any more (a file that left both sides while no daemon watched, the old spelling of a name that both sides respelled), unless the row was written after the cloud listing was asked for. Left in place, such a row would later vouch for a deletion that never happened: the file put back on this device would read as deleted in the cloud and go to the trash.
14. A rename is recognised from the sync index, not from the watcher, which reports it as a delete and a create (a `Rename` intent for the destination on macOS). At the upload gate, a new path whose hash and size match an index row whose local file is gone becomes one `Provider::move_object` of that cloud object, and the index row moves with it; the stale delete of the old path then converges as a no-op. In the download planner, a new cloud object with the size and remote mtime of an index row whose local file is present and untouched is confirmed by hash (the backend's, or one probe) and applied as a local rename with no transfer, but only when the cloud has no object under the old name any more: a copy that kept its time (`cp -p`, a Finder duplicate) has the same size, time, and content, and it is downloaded as a second file. So that the create half is seen first, the deletion of a synced file waits one settle window while the create may still come, at most `MOVE_SETTLE_MAX_DEFERRALS` times: on its first planning while changes of the same side are not in the queue yet (local events inside their quiet window, or a feed that has more to hand over), and on every planning while a transfer that could be the new name is queued. The two halves of a rename are seen at the same moment, so when neither holds, nothing can turn the deletion into a move, and it proceeds at once. An upload whose bytes the cloud already holds completes without transferring. A change in only the spelling of a name, on a volume that folds case, is a rename too and reaches the other side as a move (§Directory and symlink semantics).
15. The reconcile walk compares each file pair with the rsync quick check on **both** sides: a pair is converged when the sizes match, the local size and mtime are what the sync index recorded at the last transfer, and the remote size and mtime are what the index recorded from the provider at that transfer. A pair the index has no row for, or that fails either check, is verified once through the upload planner in two-way mode, which knows the last synced hash: identical content converges silently and records the row; a local copy still equal to the last synced hash means the change is remote-only and becomes a download; a remote copy still equal to it means the change is local-only and becomes a guarded overwrite; both moved is a keep-both conflict. The op-id tag on the remote is never taken as proof on its own that the remote is unchanged, because an in-place write keeps the tag on a filesystem; the remote quick check has to agree. The provider reports the remote mtime with every completed transfer (`TransferOutcome::remote_modified_at`).

On macOS the throttle inputs are read from the host every second: system and daemon CPU load, power source, thermal state, Low Power Mode, resident and physical memory, and keyboard/pointer presence. On Linux they come from `/proc` and `/sys`: CPU load and memory, whether a battery is discharging, the hottest thermal zone against its trip points, and whether a graphical session exists at all (a headless host counts as idle). Disk pressure has no source on either OS yet and keeps its neutral default, as does Low Power Mode on Linux. The speed of the network interface is read on macOS only; it is the first estimate of the link capacity (§User resource budgets). Windows is not a shipping surface; its sampler returns static defaults and the daemon logs a warning at startup saying so. The daemon's startup log names the inputs the host feeds it.

A throttle state makes sync slower. Only `Suspended` stops it, and only critical thermal pressure gives `Suspended`: a condition that can last (battery power, a user at the device, Low Power Mode) never stops sync. Battery power and fair thermal pressure give `Light`. Low Power Mode, keyboard or pointer input, serious thermal pressure, and system CPU at or above 60% give `Throttled`, which runs one planner, one hash worker, one upload, and one download. The daemon's own CPU above `resourceLimits.cpuPercent` first takes workers away (§User resource budgets) and gives `Throttled` only when one worker is left. Severe disk pressure also holds downloads and leaves uploads running. A provider that answers "slow down" narrows uploads, downloads, and planner probes to one each until its delay is over.

## Local safeguards (optional advanced protections)

- **Priority classes + reconcile boost.** Within one durable flush batch, key-config and code paths enqueue ahead of lockfile noise (reusing the debounce classification as the priority signal). `vapor reconcile` also opens a bounded 30s boost window: deferred reconciles release immediately (bypassing their not-before times) and the remote feed polls on the next tick. Execution still answers to the throttle ladder, so the boost speeds up scheduling, never resource impact.
- **Mass-change / ransomware guard.** Counts deletions in both directions at the moment the executor would make them irreversible: a local deletion about to remove a cloud object, a cloud deletion about to remove a local file. A deletion that turns out to be a no-op (the other side is already gone) never counts, and neither do the engine's own echoed deletes. The burst is judged as a whole: the deletions still waiting in the queue count with the ones already applied inside the rolling window, so a large batch is held before its first member lands. The guard trips when the burst reaches `massDelete.threshold` (default 1000) or `massDelete.ratioPercent` of the synced tree (default 25%, never fewer than 10 deletions). A tripped guard parks the whole burst behind one `mass-delete` decision (§Decisions): the deletion that tripped it, every deletion of that direction still queued at that moment, and every further one while the question is open. The rest of the sync keeps flowing; the daemon is never paused for it. `apply` releases the held deletions as approved and resets the window; `discard` drops them and enqueues a restore for each path from the side that still has the file. Because the whole burst is held at once, one answer covers what the question counted, and an answer given the moment the question appears does not leave stragglers to ask again. A held deletion whose target the other side removes meanwhile (the cloud reports the path gone, or the user deletes the file here too) is dropped as moot, and a question left holding nothing is withdrawn on its own. While the question is open, a deletion does not wait out its move-settle window: it goes to its probe and to the guard at once, so an answer given in that window covers it too. A held deletion stands for every later one of its path: a scan that finds the same file gone again adds no row next to it, so a question that waits for days holds one row for each file. The open question is read from the state database, so a daemon that restarts while it is open keeps holding.

## Remote to local (bidirectional MVP)

1. Provider poll fetches remote changes on throttle-aware cadence: each
   5, 15, and 60 seconds in `Idle`, `Light`, and `Throttled`, and never
   in `Suspended`. While a change is near (for a minute after a poll
   that brought changes, or after a transfer of this device) the poll
   runs at the `Idle` cadence in every state, so a device in use does
   not see the next change of a working session a minute late. A feed
   that a watcher on this device feeds
   (`ProviderCapabilities::changes_feed_is_watched`) costs no request
   and is read each second in every state but `Suspended`. The
   filesystem provider's feed is such a watcher, on the cloud root; a
   directory that appears there (created, moved in) is reported as
   one change per file inside it, because a per-directory watcher
   (inotify) never reports what landed before its watch was attached
   and no watcher reports the contents of a moved tree.
   Google Drive does the opposite: a folder that gets another name or
   another place is one entry of its feed, with none for the files in
   it. For a folder the provider knows, it sends a `FolderMoved` change
   with each path of the folder, the one it had and the one it has,
   and the engine walks the folder that holds that path. The walk
   finds what the change did: a name with another spelling, files
   under a new path, files that left. It is a walk of everything under
   that folder, so a folder at the top of the sync root costs a scan
   of the whole root on each device. An entry for a folder that is
   where it was says nothing and costs no request. When the provider
   cannot read where a changed file or folder is (the cloud did not
   answer), it sends the change with the root as its path, and the
   engine walks everything: the feed does not report that entry again.
   A folder the provider never saw is judged by its age. One that
   was made in the last two minutes is a new folder: its files come
   with entries of their own, and it costs no request. An older one
   moved into the synced folder from outside, came back from the
   trash, or was last seen before this daemon started: the feed has
   this one entry for it, so the provider sends the same change and
   the engine walks the folder that holds it.
   A watcher event is read at the next poll and is classified by what
   the folder holds then. An event of an object that is gone by then
   becomes a removal. Only a removal event carries the time of the
   removal: a create or a write that is read after the object went
   gets the time of the poll that found it gone, because the engine
   drops a removal that is older than the last sync of its path and
   the object was there at the time of that older event. The watcher
   can fold a write and the removal after it into one event, and then
   no later event carries the time of the removal.
2. Changes are mapped into durable intents with operation IDs. A change
   whose local-equivalent path matches the ignore rules is dropped here
   (counted as `ignored_changes`): ignore filtering is symmetric, so an
   ignored name never syncs in either direction. The reconcile
   comparison walk applies the same rules to both the local and the
   remote side of every directory pair: an ignored name (`.DS_Store`,
   `node_modules/`) is never descended into, never uploaded, never
   downloaded, and can never manufacture a keep-both conflict copy.
   The sync index row of a name that a rule hides stays while the rule
   holds: when the rule goes, the row says which side changed in the
   meantime, so an edit made under the rule uploads and is not a
   conflict.
3. Loop prevention filters self-originated writes.
4. Apply pipeline writes local changes and records conflict outcomes. A
   deletion leaves no tombstone: the sync index row of the path is the
   evidence, and it goes when the deletion is applied.
5. A file Vapor removes on this device (a deletion that arrived from the
   cloud in `two-way`, a mirror removal in `pull-only`) is never
   unlinked: it goes to the profile's managed trash under
   `vapor_dir/trash/<profile>/<entry>/` with a `meta.json` naming the
   original path, the time, and the reason (a file that is itself
   named `meta.json` is kept as `meta.json~payload`), or to the user's own trash
   when `trash.useSystem` is set and the platform `TrashBin`
   accepts it. A sync root on another volume (an external drive) has
   a second location on that volume, `<volume root>/.vapor/trash/<profile>/`,
   the runtime directory's layout at the volume root, so the discard
   is a rename on that volume and never a copy onto this one; both
   locations are listed, restored from, and purged together. A
   directory named `.vapor` is invisible to sync at any depth with
   everything under it (the path filter, the reconcile walk, and the
   filesystem provider's listings and feed all agree), so the volume
   trash inside a sync root that is a whole drive, a runtime directory
   inside a sync root, or a dev checkout's `./.vapor` with its logs and
   sandboxes never becomes an intent; `.vaporignore` is a different
   name and syncs. A volume that refuses the directory falls back to
   the home location at the cost of a copy. When the original cannot be
   removed after that copy, the copy stays in the trash as the one
   complete version, the discard counts as failed, and what is left of
   the original gets an entry of its own on the next try.
   `vapor trash list|restore|empty` work with or without a daemon; a
   restored file lands in the sync root and syncs like any write. The
   daemon purges entries older than `trash.retentionDays` (7 by
   default) at startup and every half hour, on a worker, for every
   profile directory on disk: the trash of a profile that does not run
   (disabled, removed, suspended) ages out too. An entry is written
   `meta.json` first, so a crash between the two steps leaves an entry
   with no payload, which the listing hides and the purge removes after
   one hour. `vapor trash empty` asks before it removes anything, or
   takes `--yes`. `trash.enabled: false` unlinks.
   What Vapor removes in the cloud follows the provider's own semantics
   (Google Drive trashes; the filesystem provider removes).

## Sync modes (directionality)

`syncMode` selects which direction changes may flow. Full design (semantics,
safety, config surface, rollout order) lives in `sync-modes.md`; the pipeline
touch-points are:

- **`two-way`** (default), both the local→remote and remote→local pipelines
  above run; divergence resolves by the keep-both conflict policy below.
- **`pull-only`** (cloud → local, strict mirror): the local→remote pipeline
  is gated off: local watcher events do not produce upload/remote-delete/
  remote-rename intents. The remote→local apply and reconcile actively make
  local match cloud, including reverting local edits to the cloud canonical
  and removing local-only files. Cloud is the source of truth; no keep-both.
- **`push-only`** (local → cloud, strict mirror): the remote→local pipeline
  is gated off: nothing is written, deleted, or reverted locally. The
  local→remote apply and reconcile make cloud match local, including
  overwriting divergent remote files and removing cloud-only files. Local is
  the source of truth; no keep-both.

The mode is a property of the sync scope / profile, so every operation
(upload, download, delete, rename, revert) flows through the same gate, no
operation bypasses it. `self_write_cache` is orthogonal and stays active in
every mode. One-way modes are strict-mirror and can delete/overwrite the
subordinate side, so they are opt-in per profile with the safety
requirements in `sync-modes.md §Safety`. `syncMode` is per-profile and
categorical (the top-level value is the default; each profile may override
outright, not MIN-lowering), so one device can run several `pull-only`
mirror profiles alongside a `two-way` profile.

## User resource budgets

User-configurable daemon-process ceilings (`resourceLimits.cpuPercent`, `memoryPercent`, `bandwidthPercent`) and an optional dynamic headroom expansion (`idleBoost`) layer on top of the internal throttle controller. Each ceiling is a measured limit: the daemon reads what it uses and what the link carries, and it never assumes a number.

Resolution and enforcement sequence on every tick:

1. Resolve effective ceilings by taking the MIN of the global values and each enabled profile's override; any enabled profile with `idleBoost.enabled = false` disables boost daemon-wide. Profile overrides can only *lower* effective ceilings relative to global values.
2. Sample device-level CPU, memory, and network utilization plus user-idle duration at the same 1s cadence as the throttle controller.
3. Run the idle-boost state machine. Boost engages only when: throttle state is `Idle`, user has been idle for at least `minIdleSeconds`, and non-Vapor utilization is at or below each `headroom*Percent`. Effective ceilings linearly ramp from `resourceLimits.*Percent` toward `idleBoost.*Percent` over `rampUpSeconds`; any condition break ramps back down over `rampDownSeconds` (bounded to `<= rampUpSeconds` so activity resumption is non-invasive).
4. Publish effective ceilings and reason codes to the workgate, the bandwidth shaper, and the memory-reactive caches:
   - **Small transfers.** A transfer of 256 KiB at most, and a deletion in the cloud, costs its commit and not its bytes. On a provider whose commit is one flush of a drive that the transfers of a moment share (`small_transfers_share_a_commit`: the `filesystem` provider), such transfers have a lane of their own next to the transfer slots: five for each slot of the throttle state, in each direction, so `Throttled` has the narrowest lane and `Suspended` none. A flush that carries more files is fewer flushes for the same work. The lane is closed while `resourceLimits.concurrentTransfers` is set, which counts every transfer, and while a provider asked for fewer requests.
   - **CPU.** While the daemon's own measured CPU is above the ceiling, the workgate takes one worker away at each sample, down to one. Below the ceiling the workers come back one at a time. When one worker is left and the CPU is still above the ceiling, the throttle state steps to `Throttled`.
   - **Bandwidth.** Uploads and downloads have a token bucket each, set to `bandwidthPercent` of the capacity of that direction (`core/daemon/src/bandwidth.rs`). The capacity starts from the interface speed the OS reports, which is an upper bound. From the first transfer on it is measured: the highest rate that a transfer step with no limiter on it reached in the last five minutes. A step that the limiter sized is short, and most of its time is the round trip to the provider, so it reads low: it can raise the estimate and never lowers it, and the same holds for the step that ends a transfer. When no step measured the link for five minutes, one step runs without the limiter as a probe, sized to last about a second on the link as it was last measured (1 MiB at least). A faster link gives a higher rate with no setting to change, and a direction with no known capacity has no limit until its first measure. Test harnesses pin the capacity with the static throttle inputs.
   - **Memory.** The ceiling is a share of device memory. While the daemon's measured resident size sits above it, `self_write_cache` TTLs and the timeline buffer trim to their documented floors, and they restore once usage falls below half the budget.

Invariants:

- Ceilings are measured limits, not instant caps: the daemon reads its use each second and corrects within a few samples. A short excursion above a ceiling is possible; a lasting one is a bug.
- User ceilings never relax the throttle controller: a `Suspended` decision always wins, even under full idle boost.
- When effective CPU ceiling drops below current in-flight concurrency, no new work is admitted but running work proceeds to its next slice checkpoint before yielding; this matches the existing interruptible-reconcile discipline.
- `self_write_cache` TTL and diagnostics buffer length are bounded by documented floors even under memory pressure so loop-prevention and observability guarantees are preserved.

### Ceiling transitions (idle-boost and throttle interaction)

Idle-boost and the throttle controller update asynchronously on the same 1s cadence, so a mid-ramp throttle transition must be resolved deterministically:

1. **Throttle exit from `Idle`.** When throttle state transitions from `Idle` to `Light`, `Throttled`, or `Suspended` while boost is active or mid-ramp, the effective ceiling snaps immediately to `min(current_ramped_value, resourceLimits.*Percent)` and boost is marked disengaged. No down-ramp occurs in this path; the cap collapses to the base ceiling in the same tick to guarantee the post-Idle throttle state never runs against boosted caps.
2. **Throttle return to `Idle`.** When throttle returns to `Idle` after having left it, boost is not auto-resumed. The idle-boost state machine re-evaluates all gating conditions (`minIdleSeconds` since last user input, `headroom*Percent` samples, `idleBoost.enabled`) from scratch and, if they pass, starts a fresh up-ramp from `resourceLimits.*Percent`. This prevents boost from re-engaging off stale conditions after even a brief throttle excursion.
3. **Graceful boost exit (throttle stays `Idle`, gating condition fails).** If boost is active and a non-throttle condition breaks (user HID input, non-Vapor utilization exceeds headroom, `idleBoost.enabled` flips to `false` via config reload), ceilings ramp down linearly over `rampDownSeconds` back to `resourceLimits.*Percent`. `rampDownSeconds` must be `<= rampUpSeconds` so activity resumption is non-invasive.
4. **Config reload mid-ramp.** Lowering `resourceLimits.*Percent` mid-ramp immediately clamps the current ramped value to the new (lower) base ceiling; raising it does not retroactively raise the ramp target, which remains the original `boost*Percent`. Changing `boost*Percent` or `rampUpSeconds` mid-ramp snaps the in-progress ramp to the new targets on the next tick (no restart, no glitch).
5. **In-flight work during snap-down.** When the effective ceiling drops below current in-flight concurrency as a result of any of the above, no new work is admitted but running work proceeds to its next slice checkpoint before yielding: same discipline as §"Invariants" above.

## Directory and symlink semantics

This section owns the "what syncs" contract: Vapor syncs file content,
and everything else on a filesystem is handled deliberately. The table
summarizes the contract; the prose below carries the engine-level
mechanics and the "why":

| Item | How Vapor treats it |
| ---- | ------------------- |
| Regular files | Fully synced, byte-for-byte, in both directions (following the configured sync direction). |
| Folders | Containers, not synced objects: each side creates them automatically as the files inside them sync, and deleting a folder propagates. An empty folder does not appear on the other side, and renaming a folder re-syncs its contents under the new name. |
| Ignored files | Anything matching the ignore rules never syncs in either direction: it can't be pulled down from the cloud, and it never causes a conflict. |
| Conflicting edits | When both sides change the same file, both versions are kept: the other device's version stays next to the local one as a `~conflict-` copy until resolved. Nothing is ever silently overwritten. |
| Symlinks | Never followed and never synced: a link could pull content from outside the sync root into scope, and cloud providers can't represent them faithfully. |
| Hard links | Synced as an ordinary independent file; the link relationship is not preserved on the other side. |
| Special files (pipes, sockets, devices) | Ignored entirely: they carry no transferable content and never block the files around them. |
| Metadata (permissions, extended attributes, timestamps) | Content is the synced object. POSIX permissions carry over on filesystem-backed transfers (an executable script stays `0755` on the other side), but a permissions-only change does not propagate (sync state compares content hashes), and providers without a native mode concept (Google Drive) do not preserve modes. Extended attributes and timestamps are not synced. Vapor's own bookkeeping tags (op-id xattrs / side-files) stay invisible and never appear in listings. |

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
(both directions, including strict-mirror removals). A folder that a
cloud-side move or removal leaves empty on this device in `two-way` is
asked about like any cloud deletion: it goes when the cloud has no such
folder any more (it was renamed or removed there), and it stays when the
cloud still has it. An empty folder is no deletion for the deletion
guard. First-class folder
sync was evaluated and rejected (project-owner decision, 2026-07-07):
keeping every synced object content-shaped is what keeps the conflict,
echo-suppression, and transfer machinery simple: folders cannot
"conflict", and parent materialization is idempotent by construction.

Two mechanics make the folder rename work with a file-only engine. A
directory that *appears* locally (created, or renamed in from
elsewhere) arrives from the watcher as one event with no events for its
children, so the runtime walks it and reports one `Created` event per
file underneath, pruning ignored names the way the watcher bridge would;
those synthesized events then debounce, compact, and upload exactly as
a `cp -r` would have. A directory that brings an ignore file with it (a
project folder moved in with its `.gitignore`) waits first: the reload
of the rules is asked for, and its files are reported when the rules of
that file are in force, so a file those rules hide never uploads. The
reconcile walk waits the same way. The rules record each ignore file
they were read from, with its modification time and size; a folder
that holds an ignore file in another state, or one the rules do not
know, is not compared until a reload that started after the request
lands, and then it gets a walk of its own. If the rules still do not
hold the file after that reload (a file that cannot be read), the walk
goes on with the rules there are.

The rules are read off the tick thread, the first time included. A
profile starts with the rules `vapor.json` gives, and the walk of the
tree for its ignore files runs on a worker. Until those first rules
are in, nothing is polled, walked, or leased for the profile. While
any later reload is asked for or runs, no watcher event becomes an
intent: the events wait in the recorder, and when the rules land each
one is checked against the rules in force then, since the watcher
bridge let it through under the rules of its own moment. A file
written next to a new `.gitignore` in the seconds before the reload
lands is therefore never queued under the old rules. A read of the
ignore files that fails puts no rules in force and is tried again each
`IGNORE_RULES_RETRY_SECONDS` (60); until one works, the first rules
are not in or the events keep waiting, because a name that an ignore
file hides must not sync on rules that are out of date. While the first
rules are read, `vapor status` gives the scan as `waiting` with that
reason. Profiles with one local root share the rules, and each one
releases its own waiting folders when a reload lands. A
subtree larger than
`SYNTHESIZED_SUBTREE_EVENT_CAP` files leaves a subtree reconcile marker
for the rest, the same deferral a storm gets. A local directory that
*disappears* arrives as one `Delete` intent; the provider never deletes
a directory recursively (that would destroy children the engine never
compared against the index), so the delete planner lists the remote
subtree and expands the intent into one guarded `Delete` per entry,
deepest first, with the directory itself re-enqueued last. Each child
delete keeps the usual rule (refused, and the newer remote content
downloaded, when the remote changed since the last sync). A directory
delete that still finds children waits while any of them has queued
work, and keeps the directory once the children that remain exist
locally again, which is what stops an expansion loop. An empty remote
directory is removed outright. The reconcile walk takes the same route
for a folder it finds gone from this device while the index holds rows
under it: one `Delete` of the folder, not a walk into it, which would
delete the files and leave every folder behind in the cloud.

**Names that differ only by case are one name on the filesystems Vapor
ships on today.** Whether a root folds case is read from the volume
(`fs_caps::case_sensitivity_at`), not assumed. On a volume that folds
case, a change in only the spelling of a name (`readme.md` to
`README.md`) is a rename: the walk and the watcher path carry it to the
other side as a move, with no conflict copy and no question. When both
sides changed the spelling in different ways before a sync, the cloud's
spelling stays and the local name follows it. Every comparison uses the
spelling the volume stores (`vapor_shared::paths::stored_spelling`), not
the spelling of the request. Only the names below the sync root are
compared: a folder above the root that changed its spelling (the root
itself renamed from `vapor` to `Vapor`) is not a fact about the files
below it. The name of the entry itself counts as another spelling
whenever it differs from the stored one, so no table of what a volume
folds has to agree with the volume. The filesystem provider holds an
upload while a folder on the way to its path is stored under another
spelling: written through, the file would land in a folder that then
does not answer for that path. A folder that the cloud renamed by
spelling only is followed too: a change that names a folder by another
spelling than this device stores asks for the walk of the folder above
it, which renames the local folder to the cloud's spelling.

A rename that changes only the spelling goes to the cloud as one move
of the file or of the folder, before anything else under that name is
planned. The sync index holds its rows under the old spelling, and the
device stores the entry under the new one. An intent for a path under
the new spelling would find no row: an edit would upload as a new file,
a deletion would find nothing to delete, and a change from the cloud
would download over the local file. So the first intent that meets such
a path (the index has a row for another spelling of it, or rows under
another spelling of a folder on the way to it, and that other spelling
is not on this device) sends `Provider::move_object` for the renamed
entry, and the index rows follow it to the new spelling. Other intents
under the same entry wait for that move. When the cloud has nothing
under the old name, or already has the new one, the rows of the old
spelling are dropped and the walk of the folder settles the names; with
no row, every file of the folder is compared as a file that never
synced, which keeps the data of both sides. "Nothing under the old
name" counts only as the word of the adopted cloud root: the worker
reads the identity of the root after the move, and when the root is
away, swapped, or cannot be read, the rows stay and the rename waits
for the root. Rows dropped in an outage would leave the walk with two
spellings and no evidence of which side changed, and it would take the
cloud's, which undoes the rename. A backend that cannot move
an object keeps the older route: the old name is deleted and the files
are uploaded under the new one.

A cloud can hold `Readme.md` next to `readme.md`; the
local root cannot. The engine never maps a remote name whose exact
spelling is absent locally while a differently-cased spelling is
present (the reconcile walk, the changes-feed mapping, and the download
apply all check), so the first object's local file is never rewritten
by the second object. The second object is materialized under a
conflict-copy name (`readme~conflict-<device>-<ms>.md`) and the pair is
recorded in `name_aliases`: from then on that local copy syncs with its
own cloud object in both directions (uploads, downloads, and deletes
resolve the remote path through the alias before the mirror), the walk
pairs the aliased remote name with the copy, the feed maps changes to
the copy, and the alias is released when either side deletes. The cloud
keeps its two objects; the device holds the kept name and the copy, and
a `collision` timeline event names both. Among remote-only names that
fold to one key the lexically first one takes the name and the rest are
materialized the same way. A colliding *directory* has no copy to make
and is reported and left untouched. Such a copy's `conflict` decision
offers only the local file that holds the folded name, and answering it
removes the cloud's other object; moving the copy onto the kept name
would only swap which cloud object is stranded.

**Special files (FIFOs, sockets, device nodes) are inert.** The
filesystem provider skips them in the cloud folder: its listing, its
stat, and its feed do not report one, and no call opens one. On this
device, the executor
refuses them at planning and the reconcile walk skips them: this must
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
Windows symlink creation requires elevated privileges, so the safe,
portable contract is "symlinks are invisible to sync."

## Conflict handling

This section describes the **`two-way`** policy. In the one-way `pull-only`
and `push-only` modes there is a declared source of truth, so divergence is
resolved in favor of the authoritative side with no conflict copy (see
`sync-modes.md`). Keep-both remains the default because `two-way` is the
default mode.

This section owns conflict *creation*. Everything after a copy exists:
notification, listing, and resolution as a `conflict` decision through
`vapor decisions` and the app surfaces: lives in `conflict-resolution.md`.

When local and remote versions of the same path diverge (both sides modified, or rename collides with an existing name), the default policy is "keep both; never silent overwrite." Concrete mechanics:

- **Winner and loser.** The side whose provider op-id completes first retains the canonical path. The losing side is renamed in-place to a derived "conflict copy" path. On the device that detects the conflict, that is its own edit: the local version moves to the copy, named with this device's `deviceId`, and the cloud's version comes down under the original name. With more than two devices, each losing device makes its own copy, so N concurrent edits leave the file plus N-1 copies, all of them one `conflict` decision.
- **Suffix template.** `{stem}~conflict-{device_id}-{timestamp_ms}{ext}` where `{stem}` and `{ext}` are the original basename split at the last `.`, `{device_id}` is the device identifier persisted once at first run under `vapor.json`, and `{timestamp_ms}` is the event time in UTC milliseconds (monotonic-within-device).
- **Device identifier.** Made at first run from the host name: the part before the first dot, normalized to `[a-z0-9-]`, then `-` and four random characters, so two devices with one host name still write different copies. The value is written to `vapor.json` as `deviceId` under the configuration lock and is never regenerated; a later change of the host name does not change it.
- **Collision-avoidance fallback.** If the derived conflict path already exists (on either side), append `-{seq}` starting at `2` and increment until free. Fallback template becomes `{stem}~conflict-{device_id}-{timestamp_ms}-{seq}{ext}`.
- **Rename-during-conflict.** If the loser is renamed by the user or remote during the conflict write, the conflict copy still lands at the resolved fallback path; the user-initiated rename becomes a separate follow-up intent through the normal scheduler.
- **Deletion against a change.** If one side has deleted the path while the other side has a modified version, the modification wins and is written to the original canonical path (no conflict suffix). "Delete wins" is never the default: data preservation always wins over deletion, and no modification time is compared to decide it. A local copy that is exactly what the index last synced is not a modification: when its cloud object is found gone at upload time (a watcher reported the file again just as the cloud deleted it), the deletion applies here through the guard and the trash instead of a re-upload that would undo it and whose own events the feed would read as echoes. An upload the user asked for (the restore after `discard`, the merge after a root answer) carries `approved` and is never read that way.
- **Determinism.** All inputs to the suffix (device_id, timestamp_ms) must be derivable from durable state or event metadata, so the same conflict replayed on the same device produces the same conflict path across daemon restarts.
- **A stop between the two steps.** The local version is first set aside under a temporary name that carries the original name, and the cloud's version is then downloaded under the name. A daemon that stops between the two loses nothing: the next reconcile walk finds the set-aside file and gives it its conflict-copy name.
- **Two uploads that land in the same moment.** The guard of an upload is a check of what the cloud holds, then the write. A second device can write between the two, and both uploads then land: the later one replaced a version that its device never compared. That version is not lost. It stays next to the file as a copy named `{stem}~conflict-replaced-{timestamp_ms}{ext}`, where `replaced` stands in the place of a device id because no device chose to make the copy, and the time is when the replaced version was written. The copy syncs to every device like any file and joins the `conflict` decision of the file. The filesystem provider commits a guarded overwrite as an exchange of two files, so the file it replaced is in its hands at once; a volume with no such exchange keeps the check as its one protection. Version 3 of the Google Drive API has no conditional write of content: after a guarded overwrite lands, the next changes poll reads the versions of the file. A commit whose answer did not come gets the same check, because the write can have landed and the retry then finds its own bytes in the cloud and sends nothing. Drive lists the versions by their times, and it gives a version its time when its bytes are in, about a second before the write commits. Three things follow. A write that the upload did not see has a time near the time of the upload's own version: the check takes a version for one only from twice the time the commit took plus 30 seconds before that time to the time the commit took plus 1 second after it. The history of the file never becomes copies, whatever times its versions carry. The next saves of this device are in the list too, seconds after the upload: the provider remembers what it wrote, and none of its own versions counts as a write of another device. And of two writes of one moment, the one that holds the name can be listed first (2 of 10 runs against a real account), so when the list holds a version of that moment the poll also reads which content holds the name. That version is not lost. Each other version of another writer is kept as a copy, whichever device asks; a copy that is there already is not made twice. The upload's own version is kept by the device whose write holds the name, when that device runs Vapor: its upload put its own tag on the file. A file that still carries the tag of this upload was replaced by a program that does not look at what it replaces, and then this device keeps its own version as the copy. A writer can set the time of its version (Drive for desktop sets the time the file has on its disk): a write of that moment whose time is outside those bounds is not found and stays in the version history of the file in Google Drive (`docs/tasks/core.md`). A cloud that cannot be reached, a rate limit, and a missing sign-in spend none of the tries of that check, so it waits through an outage; each poll reads up to 8 checks and makes one step of one copy. That check is held in memory: a daemon that stops between the upload and the poll makes no copy, and the version is then only in the version history of the file in Google Drive (`docs/tasks/core.md`).
- **A conflict that cannot finish yet.** A keep-both whose download fails (the cloud is away) puts the local version back under its name and is tried again; the local file is never left only under a temporary name.

## Decisions

A decision is a question the daemon parks when an irreversible action
rests on evidence that could be read two ways, and only the user can
say which reading is right. The rules:

- **Scope is the smallest thing that is ambiguous.** A decision holds
  one path, one batch, or one profile. Everything outside the scope
  keeps syncing. The daemon is never paused because a question is open.
- **Durable and listable.** A decision lives in the profile's state DB
  (`pending_decisions`), with its kind, scope, question, a short option
  list, and JSON evidence. `vapor decisions list|show` read it with or
  without a daemon; the app drives the same command. `vapor status`
  reports the open count (`decisions`), and a `decision` timeline entry
  marks both the opening and the outcome.
- **Held intents.** The intents the decision holds move to the `held`
  queue state, tied to the decision by `decision_id`. Held rows are not
  queued work: they are not leased, not counted in the queue depth, and
  they survive restarts. `vapor diagnostics` shows them in the `held`
  stage with the decision number as the reason.
- **Answering.** `vapor decisions resolve <id> --choose <key>` records
  the answer in the DB; the daemon applies it on its next tick, or at
  its next start, and marks it applied. Each kind has one applier; an
  answer a build cannot apply is logged and left answered-but-unapplied,
  never silently consumed. An answer whose apply fails (the cloud is
  away, a file is locked) is tried again after 5 seconds, 60 seconds,
  and then each 10 minutes, with one warning for each reason. Released intents carry `approved`, which the
  guard that held them respects.
- **Kinds today.** `mass-delete` (batch scope, options `apply` and
  `discard`; see the guard under Local safeguards); `root-missing`
  (profile scope, option `recreate`) and `root-replaced` (profile
  scope, option `reattach`), both under Root identity below;
  `type-mismatch` (path scope) for a name that is a file on one side
  and a directory on the other, with `keep-both` (the local side moves
  to a conflict name and the cloud side comes down under the original
  name), `prefer-local` (the cloud side is removed, guarded, and the
  local side goes up), and `prefer-cloud` (the local side goes to the
  trash and the cloud side comes down). Both sides stay untouched while
  it is open, the question is asked once, and an applied answer gets
  `DECISION_APPLY_GRACE_SECONDS` to land before the walk may ask about
  the path again. `one-way-mode` (profile scope, option `enable`): the data-loss
  warning of a `pull-only` or `push-only` profile, which holds the
  profile until it is answered (`sync-modes.md` §Safety).
  `unsyncable-name` (path scope, option `skip`) for a
  file whose name the sync path model cannot carry: a name
  that is not valid UTF-8 (possible on Linux; macOS refuses to create
  one), or a name with a backslash, which is a separator on Windows.
  The same holds for a name in the cloud folder, which the provider's
  listing reports (`enumerate_with_unsyncable`): those two in a cloud
  folder on a drive, and a `/` in a Google Drive name. The question
  says on which side the file is. Nothing is held: the file stays on
  its side only and the
  question is the user's notice. A rename fixes it, after which the
  next whole-scope walk finds the name gone and withdraws the
  question (a walk decides that when it has seen the whole scope, not
  after each slice of it); `skip` stops the asking for that name. A
  name of that kind in the cloud folder of the filesystem provider is
  left where it is, with one warning in the log. Watcher events for
  such a path are dropped at the callback bridge, since the durable
  queue is UTF-8. `conflict` (path scope, the conflicted file) for a
  file with keep-both copies, with one option per version keyed by its
  file name; the versions not kept go to the trash. Nothing is held, the
  files are the record, the decision is rewritten in place when a
  version arrives or goes, and it closes when no copy is left however
  they went. `vapor decisions resolve` applies a conflict answer itself,
  since it is local file operations (`conflict-resolution.md`). Further
  kinds land with the feature that needs them and are listed here.

### What stops a whole profile

Only conditions under which no file can be synced correctly stop a
profile. Everything else is file- or batch-scoped and leaves the rest of
the sync running.

| Condition | Effect | Way out |
|---|---|---|
| A sync root is missing (a volume unplugged, a folder deleted or moved) | profile holds; nothing is created or deleted anywhere; a `root-missing` decision is open | the root comes back (the hold lifts on its own), or `recreate` |
| A sync root is replaced (a different folder at the same path, an emptied one) | profile holds; nothing is synced into the stranger; a `root-replaced` decision is open | the original root comes back, or `reattach` (merge, no deletions) |
| The cloud is unreachable | profile waits and retries with backoff; intents keep accumulating durably | connectivity |
| The cloud storage is full | uploads wait and are tried again each 15 minutes without spending an attempt; downloads and deletions continue; the status reason and a `cloud-full` timeline entry say so. The queue row of each upload records the wait, so a daemon that restarts still says it | free space in the cloud; the next try sends the uploads, and `vapor reconcile` (the app's Sync now) tries them at once, after a restart too |
| No usable configuration (no `vapor.json`, a file that does not parse, no valid profile) | the daemon stays up, syncs nothing, and creates no folder; the run state is `unconfigured` and the reason says what to fix | `vapor init` (or `vapor profiles add` for the profile alone), or a fix to the file; the first profile composes live |
| A one-way profile whose warning is not acknowledged | profile holds; a `one-way-mode` decision is open | `enable`, a confirmation on the terminal by `vapor profiles add`, or a change of `syncMode` |
| A profile that cannot be composed | that profile is suspended and listed with its reason; a provider that is unknown and two folders that overlap are found before the local folder is made, so such a profile leaves no folder behind; the others keep syncing | the configuration is fixed (the daemon reads it again), or a restart |
| A profile whose state database cannot be opened, or whose runtime cannot be built (a disk that is full, a file that another process holds) | that profile is suspended and listed with its reason; the others keep syncing; the runtime tries it again after 1, 5, and 30 minutes, then each 30 minutes | the cause goes away; the next try composes it |
| A profile whose tick panicked, failed five times in a row, or was not saved by the state database (a full drive at the commit) | that profile is suspended and listed with its reason; the others keep syncing; the runtime composes it again after 1, 5, and 30 minutes, then each 30 minutes. A profile that composes and fails again before it ran for the wait it had reached continues from that wait | the cause is fixed; the next retry composes it |
| The provider refuses the sign-in (an expired or revoked token, none stored) | profile holds; nothing is leased and the feed is not polled; an intent that hit the refusal returns to the queue without spending an attempt; status sets `signInRequired` | `vapor auth login --profile <id>` or Sign In in the app; the next root check lifts the hold and the queue resumes as it stood (`operations/provider-auth-operations.md`) |
| The configuration becomes invalid while the daemon runs | the daemon keeps the last valid configuration and reports the error | `vapor config` fixes it; live reload picks it up |
| The daemon crash-loops | the lifecycle guard stops restarting it | `vapor service` after the cause is fixed |
| The user pauses (`vapor pause`, for one profile with `--profile`) | every profile stops, or the one that was named; a profile that is composed later starts paused when the pause covers it; a hold that comes and goes meanwhile (a root that went away, a full cloud) does not end the pause; ingest keeps capturing intent durably | `vapor resume` (for one profile with `--profile`), after which the state says what still stops work, if anything does |

Never a whole-profile stop: a conflict, a name collision, a type
mismatch, a deletion burst, a single failed transfer. Those hold their
own path or batch and, when a person has to choose, open a decision.

### A file that keeps failing

A transfer that fails is tried again with a growing wait, up to 15
minutes between tries. After five failures in a row the file counts as
blocked: `vapor status` takes it out of the queued count and names it
with the reason of the last try (a file that cannot be read, a target
that is locked), so a file that no retry will fix does not show as
work in progress for good. A blocked file gets one comparison of its
folder, with both listings, which finds what the intent alone cannot
see. Three things make the next try due at once: a new event for the
path (the cause may be gone; an event within ten seconds of a failed
try is taken for the trace of that try and is not counted), a
`vapor reconcile`, and an answer of the user about the path.

### A file where the other side has a folder

A name that is a file on one side and a folder on the other is settled
by the walk of its folder, which has both listings. The index says who
made the swap. When one side made it and the other side still holds
what was last synced, the swap applies like any deletion and any new
file, with no question: a folder here over a synced file removes the
cloud file and uploads the folder; a file here over a synced folder
removes the cloud folder, file by file under the usual guard, and then
uploads the file; a file in the cloud over a synced folder sends the
local files of the folder to the trash, when each one is exactly what
was synced, and downloads the file; a folder in the cloud over a synced
file sends the local file to the trash and downloads the folder. When
both sides changed, or the name never synced, the walk opens the
`type-mismatch` decision. An intent that meets such a name (an upload
onto a cloud folder, a download onto a local folder with content) does
not retry: it asks for that walk at once, so the swap is carried out,
or the question is open, seconds after the change.

## Root identity

The folders a profile syncs are the folders it adopted, not whatever
sits at the configured paths. Without that check an unplugged volume
reads as "every file was deleted", an empty folder at a mount point
gets mirrored into the cloud, and a re-created cloud folder swallows a
re-upload of everything. The mechanism (`core/daemon/src/root_identity.rs`,
`core/providers/src/root_marker.rs`):

- **Identity.** The local root carries a hidden `.vapor-root` marker
  (an internal name: never synced, hidden from listings and feeds,
  dropped by ingest). The cloud root carries whatever the provider
  offers through `Provider::root_identity`: the same marker on a
  filesystem-backed root, the folder id on Google Drive. A backend
  without one records an empty identity and is only checked for
  presence.
- **Adoption.** On a profile's first contact with a root (nothing
  recorded in `state_entries` under `root_identity.local` /
  `root_identity.cloud`), the root is created when missing, the marker
  is read or written (`Provider::adopt_root` on the cloud side), and
  the identity is recorded. This is the only time Vapor creates a sync
  root on its own, and it happens only for a profile whose two roots
  the user set: neither root has a default. A local root that is a
  symlink is followed to the folder it names. Two adopters that find
  no marker at the same moment (two profiles of one daemon on one
  cloud folder, two devices on a shared one) end with one identity:
  the marker is published with a hard link, which fails when the name
  is taken, so the first marker stays and the other adopter reads it.
  On a volume with no hard links, and when a file that is no marker
  has the name, the marker is renamed into place, and of two adopters
  that reach that step at the same moment the last one wins. The
  marker is flushed to disk before it gets its name.
- **Checks.** At every start and every 15 seconds afterwards
  (`ROOT_CHECK_INTERVAL_SECONDS`), the local root inline and the cloud
  root through a worker probe. A missing root holds the profile and
  opens a `root-missing` decision; a present root without the recorded
  identity holds the profile and opens a `root-replaced` decision.
  While held, nothing is leased, the status reason names the decision,
  and ingest keeps capturing intent durably. A marker that cannot be
  read (a folder the OS has not let Vapor into) counts as a root that
  cannot be reached, not as a replaced one.
- **The check before destructive work.** Fifteen seconds is long
  enough for a swapped root to look like a tree of deletions. So the
  executor reads the local marker again before it leases any intent,
  and before each deletion in the cloud. A root that is not the adopted
  one defers the work and marks the root suspect, and the next check
  opens the decision. The cloud side has the same check before each
  local removal that a cloud state asks for: the worker that finds the
  cloud object gone then reads the identity of the cloud root
  (`Provider::root_identity`), after that answer and never before it.
  That covers a cloud deletion applied in `two-way` and in `pull-only`,
  and the removal of local-only content by a `pull-only` mirror. The
  workers share these reads: a read that started after a worker's own
  call returned says the same to that worker, so a burst of deletions
  costs a few reads and not one for each file. A removal that finds the
  root in doubt starts the check of the cloud root at once. A root
  that is another folder, is missing, or cannot be asked defers the
  removal and marks the root suspect in the same way.
- **Return.** The original root coming back (the marker matches again)
  lifts the hold on its own, withdraws the decision, and runs a
  whole-scope reconcile. A local root missing at daemon start parks the
  profile as a placeholder that the multi-profile runtime composes
  again when the folder returns, with no restart.
- **Answers.** `reattach` adopts the folder now at the root (a fresh
  marker where there was none) and `recreate` creates the folder empty
  and adopts it. Both set `reconcile.merge_without_deletions` so the
  reconcile that follows merges the two sides and propagates no
  deletion in either direction, then lift the hold.
- **The outage is forgotten.** A root going away looks like deletions
  to a watcher that reports files one by one (inotify reports every
  file under a removed directory before the directory), so every
  recovery, on its own or by an answer, drops the queued `Delete` and
  `ApplyRemoteDelete` intents and re-baselines the remote changes
  cursor before the reconcile runs. A remote deletion whose probe
  fails (the root missing is one such failure) is retried, never
  applied: only a stat that answers "no object" finishes a deletion,
  and an intent dropped while its probe ran is honoured when the
  probe returns.

## Loop prevention (self-write cache)

Bidirectional sync must prevent the daemon from re-uploading changes it just applied from remote (and symmetrically re-downloading changes it just uploaded). The `self_write_cache` is an in-memory ring with durable-operation backing:

- **Record on every provider write.** On completion of any `upload`, `download`, `delete`, or `rename` issued by the daemon, record `(remote_path, op_id, content_hash, expiry_monotonic_ms)` into the cache.
- **Match on every inbound provider event.** Provider changes-feed events are matched against the cache before becoming intents. Primary correlator is the provider's `op_id` tag (xattr on filesystems that support it; a side-file fallback at `{path}.vapor-meta.json` when xattr is unavailable or write-failed). Content-hash is the fallback correlator when `op_id` is absent (e.g., third-party tool wrote the same bytes). A hit suppresses intent creation and records the suppression in diagnostics. An op-id match alone is not proof: the tag survives an in-place edit (an append, a same-size rewrite), so the change's size and mtime, which every feed reports when the backend has them, must still agree with what the transfer recorded; the durable sync index applies the same rule once the cache record has expired. Content-hash equality is positive proof on its own, and a change that carries another hash than the index is a change whatever else agrees. A change whose size and mtime are the ones the index recorded is no change, with or without an op-id: an object that no Vapor wrote has none, and a `chmod` of it changes no bytes.
- **A download asks whether there is anything to download.** A change that passes the echo check can still carry the bytes of the last sync (a `touch`, a rewrite with the same content). When the object has the size the index recorded and a local file is at the path, the planner compares the object's hash with the index before it transfers anything. Equal content means nothing is downloaded and the local file is not rewritten; the index takes the object's new time, and a local file that changed since the last sync gets its upload queued, because that edit is the one change there is. The apply step makes the same check on the payload it holds, so an edit that waits for its upload never becomes a conflict copy under the old bytes.
- **Local watcher echoes verify the current file, never the tag alone.** The local-side twin of the cache (suppressing watcher echoes of download-applies) requires the file's *current* size and modification time to match what the daemon wrote, with no hash on the tick thread: the op-id tag survives later writes, so a tag-only match would keep suppressing genuine user edits for the record's whole TTL after a download. The record of a deletion matches by the state of the path: it suppresses the echo while the path is gone and is dropped when the path is seen again.
- **Eviction and bounds.** TTL and max-entries are defined in `core/shared/src/constants.rs`, module `self_write_cache`, with `DEFAULT_TTL_MILLIS: u64 = 30_000`, `MIN_TTL_MILLIS: u64 = 5_000`, `MAX_ENTRIES: usize = 10_000`, `MIN_ENTRIES: usize = 1_000`. Eviction policy is LRU-on-insert; TTL expiry runs on the same 1s tick as throttle sampling. Memory-pressure floor: under memory pressure the cache may shorten TTL toward `MIN_TTL_MILLIS` and trim toward `MIN_ENTRIES`, but never below those floors (loop-prevention is a safety guarantee, not an opportunistic feature).
- **xattr vs side-file precedence.** Writes attempt xattr first; on `ENOTSUP`/`EACCES`/`EROFS` the fallback side-file is written atomically alongside the payload. The filesystem provider writes both before the payload is renamed into place: nothing that can fail (a full disk) runs after the commit, so an object never lands untagged. Reads check xattr first, then the side-file; if both are present the xattr wins. The filesystem provider is responsible for hiding side-files from enumeration so they do not surface as independent intents.
- **Crash safety.** The cache is purely in-memory. A daemon crash followed by restart forces a conservative whole-scope reconcile (already present in the runtime loop) which re-establishes baseline state without needing the cache to persist across restarts; cached entries for in-flight writes are re-derived from durable queue recovery.

## Multi-profile watch coordination

Two enabled profiles may have the same local root: each sends it to its own cloud folder. Local roots that lie one inside the other are refused, because the outer profile would sync the files of the inner one a second time. The later profile of such a pair in `vapor.json` does not run, `vapor status` lists it with the reason, and `vapor profiles add` refuses the folder. Two pairs on one shared root are refused the same way. A `pull-only` profile does not share its local root: it removes every file that its own cloud folder does not hold, the files of the other profile included. Two `filesystem` profiles do not share a local root when their cloud folders are one folder, or one inside the other: each file would be copied into the other folder without end. That second check is not built for Google Drive, where a path does not say whether two folders are one (`docs/tasks/core.md`). For profiles that share a root, the watcher must dedupe while keeping per-profile state isolated:

- **One watcher per distinct canonical local root.** Profile startup computes the canonical realpath of each profile's local root. Profiles sharing a canonical root share one fs-watch watcher; non-overlapping roots each get their own watcher.
- **Per-profile event fan-out.** On each raw fs-watch callback, the normalized event is matched against every enabled profile's sync-root prefix + ignore rules. Profiles that match each receive an independent copy of the event in their own bounded ingest queue, tagged with `profile_id`. Profiles that do not match do not see the event.
- **Per-profile debounce, scheduler, durable queue.** Each profile has its own debounce/coalesce tick, keyed scheduler, and durable queue. Implementation note: isolation is per-profile SQLite *files* (`state/profiles/<id>/vapor.sqlite`), not profile-keyed tables in one file; a corrupt or quarantined profile DB then cannot take out its siblings. The implicit `default` profile uses `state/vapor.sqlite`. No in-memory structure is shared across profiles below the raw watcher level.
- **Shared workgate, throttle, resource ceilings.** The workgate, throttle controller, bandwidth shaper, and effective resource ceilings are daemon-level (single process serving all profiles). Profile-override MIN-lowering resolves to a single effective ceiling set that gates all profiles; per-profile work still queues behind the shared workgate under the shared caps.
- **Blast-radius containment.** A panic or error inside one profile's scheduler, reconcile, or provider execution must not kill the watcher or other profiles. Release builds unwind, each profile's tick and each provider job runs behind a panic catcher, and a caught panic suspends that profile with its reason and leaves the watcher and the other profiles running. The runtime composes a suspended profile again after 1, 5, and 30 minutes, then each 30 minutes. A profile that was never composed (an unknown provider, an unknown `syncMode`, a relative root, a local root inside the one of an earlier profile) is listed the same way. A profile composed while the daemon runs (the first profile of an install, one whose root came back, one composed again) starts from the settings and the pause state that hold then, not from the ones the daemon started with.
- **Same-path double-write safety.** If two profiles target the same provider account and the same remote subtree, they are allowed to coexist but a write from profile A and a write from profile B to the same file are treated as simultaneous writes and resolved by the conflict-handling rules above (keep both). An op-id carries the device and a tag of the profile, so one profile never takes the write of the other for its own echo, and the two never stage an upload under one temporary name.
- **A change of the profile set applies while the daemon runs.** The daemon reads `vapor.json` again each second. A profile that was added or enabled is composed, with its own startup reconcile. One that was disabled or removed from the file stops: what its watcher reported goes to its durable queue first, as at a shutdown, and its runtime ends on a thread of its own, because the end waits for its provider calls in flight. A profile with that id is composed again only after that end. A new `name` is taken with no other effect. The other profiles keep their runtimes and start no scan. A profile that runs keeps the folders, the provider, and the mode it was composed with: a change of those waits for a restart, and status names the keys until the daemon restarts or the file names what runs again. While one profile waits for that restart, no profile starts: the file says which profiles may run next to each other for the set that it names, and a profile that it now allows could start next to the old folders of the one that waits. Profiles still stop and take a new name in that time. A file that is empty or gone ends every profile only when two polls in a row find it so, because a writer that truncates the file shows the same for a moment.
- **Crash recovery.** Restart reloads the enabled profile set, reconstructs watchers per distinct canonical root, and issues per-profile whole-scope reconcile intents at startup (profile-keyed) so each profile's volatile pre-DB intent loss is reconstructed independently.

## Control and observability

- App queries daemon over IPC for status, queue depths, reasons (see `docs/architecture/ipc-contracts.md`).
- App issues controls: pause/resume and reconcile (the app's Sync now) over IPC; settings go through `vapor.json`, which the daemon reloads.
- Diagnostics expose throttle cause, retry state, conflict outcomes, current effective resource ceilings, measured utilization, and the active idle-boost state with a human-readable reason.
