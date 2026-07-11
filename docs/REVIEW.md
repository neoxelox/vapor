# Vapor Repository Review

Full-repo review covering bugs, improvements (with emphasis on sync performance), and
security issues, plus a comment-cleanup pass over the codebase.

- **Scope**: all of `core/*`, `apps/macos`, `scripts/`, `.github/workflows`, `docs/`, skills.
- **Method**: multi-agent deep review per subsystem (25 review groups); every finding
  below was independently adversarially re-verified against the code before being
  recorded. Refuted candidates were discarded.
- **Severity scale**: `critical` (data loss / security compromise), `high` (real bug users
  will hit), `medium` (bug in edge cases or meaningful perf/robustness gap), `low`
  (minor issue or polish).

## Contents

- [1. Daemon engine (`core/daemon`)](#1-daemon-engine-coredaemon)
- [2. Providers, IPC, platform, lifecycle](#2-providers-ipc-platform-lifecycle)
- [3. CLI, shared, macOS app, scripts, CI, docs](#3-cli-shared-macos-app-scripts-ci-docs)
- [4. Comment cleanup pass](#4-comment-cleanup-pass)
- [Verification caveats](#verification-caveats)

---

## 1. Daemon engine (`core/daemon`)

Reviewed: runtime tick loop, multi-profile shell/bootstrap, durable state DB + retry,
staged executor + remote sync + conflict handling + self-write cache, fs-event ingest
(debounce, event intents, path filter, storm), scheduling (scheduler, throttle, workgate,
resource budget, auto-tune), reconcile, daemon-side IPC — plus two cross-cutting
specialists (end-to-end sync-latency trace, bidirectional data-loss hunt).

**40 findings confirmed** by adversarial re-verification.

| Sev | Category | Location | Finding |
|---|---|---|---|
| critical | bug | `core/daemon/src/executor.rs:1065` | Local delete propagates to the remote with no precondition, silently destroying a concurrent remote modification fleet-wide |
| critical | bug | `core/daemon/src/multi_runtime.rs:213` | Invalid provider kind falls back to a live no-op stub provider, which can mass-delete a pull-only profile's local data |
| critical | bug | `core/daemon/src/state_db.rs:151` | Corruption recovery quarantines the durable DB on ANY SQLite error, destroying all intent state on transient I/O failures |
| high | bug | `core/daemon/src/executor.rs:1390` | Divergence/conflict checks hash local files with hard-coded SHA-256 but compare against provider-algorithm hashes, breaking for MD5 providers (Google Drive MVP) |
| high | security | `core/daemon/src/executor.rs:1544` | Applying remote changes locally follows symlinked parent directories, allowing writes/deletes outside the local sync root |
| high | bug | `core/daemon/src/multi_runtime.rs:345` | A panicking or suspended profile permanently leaks shared-workgate permits, starving all other profiles' uploads/reconciles |
| high | bug | `core/daemon/src/multi_runtime.rs:417` | run_forever exits the whole daemon when a suspended profile coexists with a healthy profile that hits a single transient tick error |
| high | bug | `core/daemon/src/path_filter.rs:464` | Bare directory patterns in gitignore/vaporignore files do not ignore the directory's contents |
| high | bug | `core/daemon/src/retry.rs:68` | Server-supplied Retry-After is used uncapped: a bogus header can panic the daemon or persist a multi-year global retry slowdown |
| high | bug | `core/daemon/src/runtime.rs:1156` | Cloud-root recovery silently overrides a user pause and the mass-deletion (ransomware) guard pause |
| high | perf | `core/daemon/src/runtime.rs:1281` | Shipping daemon caps ALL transfers at ~312 KB/s: placeholder 10 Mbps 'measured' throughput feeds the bandwidth shaper |
| high | bug | `core/daemon/src/state_db.rs:604` | Stale-lease sweep reclaims leases still held by live executions (no lease renewal), causing duplicate concurrent work and dropped completions |
| medium | perf | `core/daemon/src/executor.rs:336` | One stage transition per tick adds ~1.25 s of fixed pipeline latency to every file |
| medium | perf | `core/daemon/src/executor.rs:650` | All provider I/O runs synchronously on the single tick thread: 'concurrency' caps are time slicing, and provider RTT stalls the whole runtime |
| medium | perf | `core/daemon/src/fs_events.rs:294` | Ignore-file events inside ignored directories trigger repeated full-tree filter rebuilds |
| medium | improvement | `core/daemon/src/multi_runtime.rs:619` | One profile's watcher/root failure at startup aborts the entire multi-profile daemon |
| medium | perf | `core/daemon/src/path_filter.rs:195` | should_ignore linearly evaluates every compiled rule per event on the fs-watch callback thread |
| medium | bug | `core/daemon/src/path_filter.rs:232` | Ignore rules (including negations) are loaded from ignore files inside user-ignored directories |
| medium | perf | `core/daemon/src/remote_sync.rs:139` | Remote poller drains one 256-change page per cadence: large remote bursts take minutes to enqueue |
| medium | bug | `core/daemon/src/runtime.rs:451` | Paused daemon keeps polling the remote changes feed, contradicting the pause semantics documented in the same block |
| medium | perf | `core/daemon/src/runtime.rs:1539` | Strict FIFO lease order lets a whole-scope reconcile backlog starve fresh user edits; priority classes only apply within one flush batch |
| medium | perf | `core/daemon/src/runtime.rs:1668` | Per-path serialization conflict breaks the whole admission batch: one in-flight long transfer starves all new work admissions |
| medium | perf | `core/daemon/src/runtime.rs:1723` | Reconcile walk fixed at 8 directories per tick (~32 dirs/s) regardless of throttle state |
| medium | perf | `core/daemon/src/runtime.rs:1810` | Echo suppression hashes entire files unchunked on the tick thread |
| medium | perf | `core/daemon/src/state_db.rs:201` | list_queue_intents ORDER BY cannot use the ready index — full scan and sort per diagnostics query |
| medium | perf | `core/daemon/src/state_db.rs:752` | Coalesced enqueue dedup lookup has no supporting index — full table scan per intent on the ingest flush path |
| medium | perf | `core/daemon/src/state_db.rs:1059` | failed_intents table grows without bound — no retention, pruning, or clearing path anywhere in the codebase |
| medium | bug | `core/daemon/src/state_db.rs:1120` | v3->v4 migration resets the queue AUTOINCREMENT sequence when the queue is empty, allowing reused ids to collide with failed_intents primary keys |
| low | improvement | `core/daemon/src/bootstrap.rs:184` | Shutdown signal does not wake the tick loop, delaying clean exit by up to a full idle sleep |
| low | improvement | `core/daemon/src/event_intents.rs:849` | Recorder drain-then-apply is not atomic, so concurrent state readers can apply event batches out of order |
| low | improvement | `core/daemon/src/fs_events.rs:225` | Watch-root/filter-root mismatch is only a debug_assert; in release it silently disables all ignore rules |
| low | improvement | `core/daemon/src/multi_runtime.rs:397` | After a caught panic, the suspended slot's runtime keeps being read every tick despite AssertUnwindSafe, outside any panic catcher |
| low | perf | `core/daemon/src/multi_runtime.rs:405` | tick_all runs per-profile SQLite queries and publishes a full status snapshot on every tick, even fully idle |
| low | bug | `core/daemon/src/runtime.rs:1040` | Timeline events hardcode DEFAULT_PROFILE_ID, misattributing all activity to 'default' in multi-profile daemons |
| low | bug | `core/daemon/src/runtime.rs:1202` | `vapor resume` while the cloud root is unavailable reports RunState::Running, masking the blocking Error condition |
| low | bug | `core/daemon/src/runtime.rs:1318` | apply_memory_ceiling squeezes the timeline capacity but never restores it after memory pressure clears |
| low | improvement | `core/daemon/src/state_db.rs:467` | schedule_retry spans five separate implicit transactions; a crash mid-sequence loses the durable rate-limit slowdown marker |
| low | improvement | `core/daemon/src/state_db.rs:477` | MAX_ATTEMPT_COUNT terminal-failure contract has no implementer, so the attempt cap wedges intents instead of finalizing them |
| low | perf | `core/daemon/src/storm.rs:96` | Storm detector clones the full event path into every ancestor directory's window on each event |
| low | perf | `core/shared/src/constants.rs:340` | 4 s default debounce window applies to the most common user documents |

### [critical] Local delete propagates to the remote with no precondition, silently destroying a concurrent remote modification fleet-wide  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/executor.rs:1065` · **Review group**: executor

The Delete route builds its plan with RemotePrecondition::None and never consults the sync index or stats the remote; the Upload/RemoteDelete arm then calls provider().delete unconditionally. The C8-17 'modification wins over deletion' guard exists only for the remote-delete-vs-local-modify direction (deletion_loses_to_local_state); the local-delete-vs-remote-modify race is unguarded. Failure scenario: device A deletes file F locally; A's Delete intent sits queued (Throttled/Suspended can hold it for minutes). Device B uploads a new version of F. A's Delete executes and deletes B's new remote content. B then receives the Removed change; B's deletion guard passes (B's local copy exactly matches B's index, and index.updated_at < enqueued_at), so B deletes its local copy too. The newest version is destroyed on every device with no conflict copy — a direct violation of the keep-both / never-lose-data guarantee in two-way mode.

**Suggested fix**: Guard remote deletes the same way uploads are guarded: at plan time stat the remote and compare op-id/content-hash against the sync-index entry; if the remote diverged from what was last synced, skip the delete and enqueue a Download instead (modification wins over deletion). Ideally extend the provider delete API with a precondition (hash/revision) so the check-then-delete window is closed, mirroring RemotePrecondition on uploads.

### [critical] Invalid provider kind falls back to a live no-op stub provider, which can mass-delete a pull-only profile's local data  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/multi_runtime.rs:213` · **Review group**: runtime-shell

In start_with_state_root, a profile whose provider_kind fails select_provider_for_profile (e.g. a typo like "gdrvie" in vapor.json) logs an error and substitutes vapor_providers::default_provider() (FilesystemStubProvider), claiming to run the profile "inert". The stub is not inert to the sync engine: ensure_cloud_sync_directory() returns Ok (so cloud_root_ready is true and RunState becomes Running in build_with_app), enumerate() returns Ok(vec![]) and poll_changes() returns an Ok empty page. start_with_state_root also unconditionally calls runtime.schedule_startup_reconcile(now). Concrete failure: a profile configured with syncMode "pull-only" and a misspelled provider starts, the startup reconcile walk enumerates the "remote" as successfully-empty, and per reconcile_walk.rs:326-331 every local file is classified local-only and scheduled for strict-mirror local deletion — the user's entire local sync root is wiped. The MassChangeGuard explicitly does not count engine-applied deletions (safeguards.rs:131-137), so nothing stops it. Push-only/two-way profiles are less catastrophic but still silently "complete" uploads as no-ops (NoopTransferSession), reporting sync success while syncing nothing. The stub's own doc comment says it "must never ship as a production default beyond the pre-GA bring-up".

**Suggested fix**: Do not substitute a functioning provider. Mark the profile as failed/suspended at composition time (set slot.failed = Some(reason), RunState::Error, skip schedule_startup_reconcile and watchers for it) so it surfaces in status but performs zero sync work until the configuration is fixed. At minimum, never allow the stub fallback for profiles whose sync_mode is not TwoWay.

### [critical] Corruption recovery quarantines the durable DB on ANY SQLite error, destroying all intent state on transient I/O failures  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/state_db.rs:151` · **Review group**: state-db

open_with_corruption_recovery treats every StateDbError::Sql as corruption. rusqlite surfaces disk-full (SQLITE_FULL), I/O errors (SQLITE_IOERR), locked/busy (SQLITE_BUSY after the 5s busy_timeout), and WAL-permission failures through the same Sql variant. Concrete failure: the device disk fills up (routine for a sync product), the daemon restarts, migrate_schema's Immediate transaction commit fails with SQLITE_FULL -> the perfectly healthy database containing every pending intent, the sync_index, and all tombstones is renamed to vapor.sqlite.corrupt-<ms> and replaced with an empty one. Pending uploads are only reconstructed by the whole-scope reconcile, but pending local-delete tombstones and the sync_index conflict baseline are gone: deletions never propagate (deleted files resurrect from remote) and concurrent-edit detection degrades. This directly violates the 'never lose intent state' core guarantee.

**Suggested fix**: Only quarantine when error.sqlite_error_code() is one of SQLITE_NOTADB / SQLITE_CORRUPT (and optionally after a failed 'PRAGMA integrity_check'). Propagate all other SQLite errors (BUSY, FULL, IOERR, PERM) as ordinary startup failures so the crash-loop guard retries instead of destroying state.

### [high] Divergence/conflict checks hash local files with hard-coded SHA-256 but compare against provider-algorithm hashes, breaking for MD5 providers (Google Drive MVP)  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/executor.rs:1390` · **Review group**: executor

hash_hex_of_file_or_err delegates to vapor_providers::filesystem::hash_hex_of_file, which is unconditionally SHA-256. Its callers compare the result against hashes produced in the provider's algorithm: index.content_hash (recorded from TransferOutcome.content_hash, computed by the provider session — MD5 for Google Drive per the comment at line 397-399) in preserve_diverged_local_before_apply (line 1322) and deletion_loses_to_local_state (line 1379), and against incoming_hash (the download outcome's provider hash) at line 1334. The executor's own StreamingFileHash carefully honors content_hash_algorithm(), so the multi-algorithm contract is clearly intended. Failure scenario with an MD5 provider: SHA-256(local) never equals MD5(index/incoming), so (whenever the size/mtime quick-check does not short-circuit) every download-apply manufactures a spurious keep-both conflict copy of identical content, and every remote deletion is refused as 'local file was modified' — the daemon fills the sync folder with ~conflict copies and never converges deletions. The same mismatch exists in runtime.rs is_local_self_write_echo (hash_hex_of_file vs the provider hash recorded in local_echoes), which would defeat watcher-echo suppression of downloads.

**Suggested fix**: Thread the provider's HashAlgorithm through these helpers (reuse StreamingFileHash or add hash_hex_of_file_with(path, algorithm)) so every local hash compared against index/outcome/echo hashes uses the provider's algorithm. Add a contract test that runs the conflict/deletion guards against an MD5-algorithm fake provider.

### [high] Applying remote changes locally follows symlinked parent directories, allowing writes/deletes outside the local sync root  ✅ DONE

**Category**: security · **Where**: `core/daemon/src/executor.rs:1544` · **Review group**: executor

RemotePath validation is purely lexical (no '..' segments), and paths.rs explicitly notes scope safety 'additionally requires the symlink checks in the filesystem provider' — but those checks only protect the cloud-root side. The local apply side has none: apply_downloaded_payload does fs::create_dir_all(parent) + fs::rename(staging, local_path), and apply_remote_delete_locally does fs::remove_file / fs::remove_dir_all, all of which resolve symlinks in intermediate path components. Failure scenario: the user has a symlink dir inside the sync root (`root/link -> /Users/alex/Documents`). Anyone with write access to the shared cloud folder creates remote `link/x` (or deletes `link/x`); the poller maps it to `root/link/x`, and the executor writes the downloaded payload into — or remove_dir_all's a directory under — /Users/alex/Documents, outside the configured sync root. This directly violates the 'never touch anything outside sync roots' invariant and is attacker-reachable through a shared cloud directory. Note the staging file (planned as intent.path.parent().join(...)) also lands through the symlink.

**Suggested fix**: Before applying any remote-sourced intent locally (download apply, remote-delete apply, staging-file creation), verify containment non-lexically: open/canonicalize the parent directory (or walk components with symlink_metadata refusing any symlink component) and confirm the resolved parent is still inside the canonical local root; fail the intent as Permanent otherwise. On Unix, O_NOFOLLOW/openat-style traversal of each component is the robust fix.

### [high] A panicking or suspended profile permanently leaks shared-workgate permits, starving all other profiles' uploads/reconciles  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/multi_runtime.rs:345` · **Review group**: runtime-shell

WorkPermit has no Drop guard — release is manual (executor.rs releases at exit paths; permits are stored in ActiveStage/RunningReconcile structs across ticks). ThrottleWorkgate::reconfigure (workgate.rs:123-126) only overwrites throttle_state and caps; it never rebuilds active_* counts, so the lib.rs:150-157 comment ("Counts are rebuilt by the next reconfigure; a leaked permit slot ... self-corrects as caps refresh") is factually wrong. When a profile panics mid-tick or is suspended after 5 consecutive tick errors while its StagedExecutor sessions and/or running reconcile hold permits, suspend_profile (multi_runtime.rs:573) only sets slot.failed — the shared workgate's active counts stay elevated for the process lifetime (no unsuspend or rebuild path exists). A leaked Reconcile permit permanently consumes a planner slot and a read token (reconcile admission is gated on planner slots and read tokens, not an active_reconciles cap — the original "reconcile capacity is 1 daemon-wide" wording was imprecise); a leaked Upload permit zeroes daemon-wide upload concurrency under Throttled (cap 1) and halves it under Light (cap 2); a leaked read-token holder blocks all hashing under Light/Throttled (1 token). Healthy profiles stall until daemon restart, silently violating eventual convergence.

**Suggested fix**: On suspend_profile, reclaim the slot's outstanding permits: give DaemonRuntime an abort/drain method that releases staged-executor session permits and aborts a running reconcile (reconcile.rs already has abort_running), and call it from the suspension paths (both the panic arm — best-effort under catch_unwind — and the repeated-error arm). Longer term, make WorkPermit an RAII guard over the shared workgate. Also fix the misleading poison-recovery comment in lib.rs::lock_workgate.

### [high] run_forever exits the whole daemon when a suspended profile coexists with a healthy profile that hits a single transient tick error  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/multi_runtime.rs:417` · **Review group**: runtime-shell

The "every profile has failed" exit condition is `ticked_profiles == 0 && failed_profiles > 0`, but tick_all counts a profile in NEITHER bucket when its tick returns Ok(Err(_)) below the 5-error suspension threshold (lines 352-374 only increment failed_profiles at the suspension edge). Concrete failure: profile A is suspended (failed_profiles += 1 every tick via the `continue` branch at line 341-344); profile B, otherwise healthy, hits one transient error (e.g. SQLITE_BUSY during a Time Machine snapshot) on a given tick. That tick reports ticked_profiles == 0, failed_profiles == 1, and run_forever logs "Every profile has failed" and returns Err — the entire daemon exits over a 1/5 consecutive-error blip in a live profile, directly violating the C8-24 guarantee stated on this function ("a single broken profile never takes the daemon down"). Repeated occurrences feed the process-level crash-loop guard, which can latch and stop the daemon entirely.

**Suggested fix**: Base the exit decision on durable per-slot state, not on the per-tick report: exit only when self.slots.iter().all(|s| s.failed.is_some()). Alternatively count error-but-not-suspended ticks as live (e.g. a `live_profiles` count of slots with failed.is_none()).

### [high] Bare directory patterns in gitignore/vaporignore files do not ignore the directory's contents  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/path_filter.rs:464` · **Review group**: ingest

expand_glob_patterns appends the `{resolved}/**` descendant pattern only when the raw pattern ends with '/'. In git semantics a bare pattern (e.g. `target`, `/target`, `__pycache__`) also matches directories and excludes their whole subtree. Here `target` compiles to matchers `target` and `**/target` with literal_separator(true), and should_ignore tests only the full relative path (no ancestor check), so should_ignore("target/debug/app.o") is false (empirically verified against globset 0.4.18). The live fs-events path (fs_events.rs:295) filters each per-file event individually, so a Rust repo with cargo's default `/target` gitignore entry has all build artifacts watched, debounced, hashed, and uploaded, and build storms churn the storm/compaction machinery — defeating the ignore feature for the most common directory-rule form and violating the low-impact goal. The reconcile walk is unaffected (it tests directory entries and prunes the subtree), making watcher and reconcile inconsistently apply the same rule. Shipped defaults are masked because every directory rule in DEFAULT_PRE_IGNORE_RULES uses a trailing slash — but note `target/` is not in the defaults at all, so the Rust scenario is fully live.

**Suggested fix**: Treat every non-negated pattern as potentially matching a directory: emit `{resolved}/**` for all expanded stems (not only directory_only ones), or track directory matches separately by also testing each ancestor of the relative path against the rule list. Alternatively adopt the `ignore` crate's Gitignore matcher, which implements git's directory-exclusion semantics exactly.

### [high] Server-supplied Retry-After is used uncapped: a bogus header can panic the daemon or persist a multi-year global retry slowdown  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/retry.rs:68` · **Review group**: state-db

decide() (core/daemon/src/retry.rs:68) uses std::cmp::max(exponential_delay, retry_after) with no upper bound, and gdrive parses the Retry-After header as raw u64 seconds (classify_api_failure). Failure scenarios: (a) a bogus 'Retry-After: 18446744073709551615' → Duration::from_secs(u64::MAX) → 'now + delay' at retry.rs:71 panics (SystemTime Add overflow); the panic is caught by catch_unwind in multi_runtime::tick_all, which immediately and silently suspends the entire profile until daemon restart (recurring while the header persists) — not a daemon crash loop, but a one-header sync outage for the profile; (b) 'Retry-After: 99999999999' puts available_at past MAX_TIMESTAMP_MILLIS (year 3000) so schedule_retry errors, the whole tick fails, the intent stays leased for the 15-minute lease timeout, and 5 consecutive occurrences suspend the profile; (c) a merely-large value (e.g. 31536000 = 1 year, or a server that mistakenly sends an epoch timestamp such as 1767225600 ≈ 56 years) parks the intent until then AND is persisted max-merged as slowdown_until via RETRY_SLOWDOWN_UNTIL_KEY, clamping upload_concurrency to 1 and signaling rate-limited to the auto-tuner until expiry, surviving restarts with no recovery path. This violates 'defer under pressure and converge eventually'. Fix: clamp retry_after (and derived slowdown_until) to a documented ceiling in RetryPolicy::decide and use checked/saturating time arithmetic.

**Suggested fix**: Clamp retry_after (and the derived slowdown_until) to a sane documented ceiling (e.g. max(RETRY_MAX_DELAY, 1h)) in RetryPolicy::decide, and use checked/saturating arithmetic for now + delay.

### [high] Cloud-root recovery silently overrides a user pause and the mass-deletion (ransomware) guard pause  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/runtime.rs:1156` · **Review group**: runtime

retry_cloud_root_if_needed() runs on every tick while cloud_root_ready is false, and on a successful ensure it unconditionally calls app.set_run_state(RunState::Running, ...). It never checks whether the current run state is Paused. Concrete failure: cloud root is unavailable at boot (network down) so cloud_root_ready=false; a local deletion storm trips the mass-change guard (stabilize_events, line 1479) which sets RunState::Paused with the 'review the changes, then run `vapor resume`' reason; within CLOUD_ROOT_ENSURE_RETRY_SECONDS (60s) the network returns, ensure_cloud_sync_directory succeeds, and this code flips run_state to Running — paused becomes false on the next tick and the queued mass-deletion intents lease and replicate to the cloud with no human review, defeating the C8-57 safeguard. The same path silently un-pauses an explicit `vapor pause` issued while the cloud root was unavailable.

**Suggested fix**: Only transition to Running when the previous run_state was the cloud-root Error state (or track 'blocked by cloud root' separately from run_state). If run_state == Paused, set cloud_root_ready = true but leave the run state and pause reason untouched.

### [high] Shipping daemon caps ALL transfers at ~312 KB/s: placeholder 10 Mbps 'measured' throughput feeds the bandwidth shaper

**Category**: perf · **Where**: `core/daemon/src/runtime.rs:1281` · **Review group**: sync-pipeline-e2e

sample_throttle_inputs always sets the shaper to Some(rate) where rate = capacity_kbps * 1000/8 * bandwidth_percent/100. capacity_kbps comes from inputs.network_throughput_kbps, falling back to ASSUMED_LINK_CAPACITY_KBPS (100 Mbps) only when None. But the production sampler chain (bootstrap.rs:116 NativePlatformMetricsSampler -> StaticPlatformMetricsSampler::default() -> ThrottleInputs::default() in core/shared/src/lib.rs:99) returns Some(10_000) — a hard-coded 10 Mbps placeholder that the shaper treats as a real link measurement. With DEFAULT_BANDWIDTH_PERCENT=25 the global ceiling for every upload+download combined is 10,000*1000/8*25/100 = 312,500 B/s (~305 KiB/s); even idle-boosted to 80% it is ~1 MB/s. The 100 Mbps assumed-capacity fallback is dead code in practice. Concrete scenario: default install on a gigabit link, first sync of a 1 GB folder -> ~55 minutes instead of ~1-2 minutes; a 100 MB file save takes >5 minutes to reach the cloud. This is a 10-100x throughput loss on typical links and dominates every other latency in the pipeline.

**Suggested fix**: Make the static/native fallback report network_throughput_kbps: None until a real per-OS measurement exists (then ASSUMED_LINK_CAPACITY_KBPS applies, giving ~3.1 MB/s at 25%), or leave the shaper unlimited when no measurement exists. Also consider whether a placeholder Some(10_000) belongs in ThrottleInputs::default() at all — it silently masquerades as a measurement everywhere inputs are defaulted.

### [high] Stale-lease sweep reclaims leases still held by live executions (no lease renewal), causing duplicate concurrent work and dropped completions  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/state_db.rs:604` · **Review group**: state-db

recover_stale_leases (core/daemon/src/state_db.rs:604) assumes in-flight leases are far younger than the 15-minute LEASE_TIMEOUT_MILLIS, but leased_at_ms is never renewed and the staged executor holds leases across many ticks. With 8 MiB/tick hash and transfer budgets, any sufficiently large file (roughly >14 GB even on a fast link, much smaller on slow uplinks or under a user bandwidth ceiling) or any Suspended-throttle stall longer than 15 minutes (uploads park holding their lease, executor.rs:631) causes the in-run sweep (runtime.rs:417, every 60 s) to flip the still-executing intent back to pending with attempt_count=0. Per-path serialization in StagedExecutor::try_start (executor.rs:271) prevents a concurrent duplicate execution of the same path, so there is no racing pair of provider writes; instead the reclaimed row churns lease/requeue cycles, and when the original execution finishes, complete_leased finds state != 'leased' and drops the completion (executor.rs:902). plan_upload has no "already synced" no-op, so the entire file is sequentially re-hashed and re-uploaded — indefinitely, if each attempt takes >15 minutes. Effects: unbounded duplicate transfer work (bandwidth/battery waste against the product's primary low-impact priority), erased retry history, and non-converging queue rows for large files; no silent data loss (uploads stay precondition-guarded).

**Suggested fix**: Add lease renewal: have the executor heartbeat leased_at_ms for intents it still holds (cheap UPDATE per sweep interval), or exclude intent ids currently tracked by the in-process executor from the in-run sweep. Do not reset attempt_count for in-run recoveries.

### [medium] One stage transition per tick adds ~1.25 s of fixed pipeline latency to every file

**Category**: perf · **Where**: `core/daemon/src/executor.rs:336` · **Review group**: sync-pipeline-e2e

advance_one performs at most one transition per tick, and several of those transitions do no budgeted work at all: WaitingForHash merely acquires a permit and opens the file (no hash step until the NEXT tick); WaitingForUpload merely acquires a permit and calls begin_upload (first byte moves a tick later); the tick that leases an intent doesn't run its planner because advance() executes before process_ready_queue in tick_with_inputs (runtime.rs:468/573). A small code-file save therefore costs: lease tick + planner tick + hash-open tick + hash tick + upload-begin tick + upload tick = ~6 ticks = ~1.5 s at the 250 ms cadence, on top of the 1.2 s debounce — roughly doubling the post-debounce latency for the common small-file case. Downloads pay ~3-4 ticks similarly on the remote->local path.

**Suggested fix**: Chain cheap transitions within one advance: after acquiring a permit, immediately perform the first budgeted step (open+first hash chunk; begin_upload+first transfer step), and run one advance pass over newly started intents after process_ready_queue in the same tick. The per-step byte budgets already bound the work; only the artificial tick boundaries between zero-cost transitions need removing.

### [medium] All provider I/O runs synchronously on the single tick thread: 'concurrency' caps are time slicing, and provider RTT stalls the whole runtime

**Category**: perf · **Where**: `core/daemon/src/executor.rs:650` · **Review group**: sync-pipeline-e2e

All provider I/O — session.step (up to 8 MiB per blocking ureq HTTP call in the gdrive provider), begin_upload/begin_download, delete, and plan_upload's provider().stat/content_hash (executor.rs:1172/1202) — executes inline in StagedExecutor::advance on the single runtime tick thread, one step per active session per tick. upload_concurrency=4 therefore time-slices four sessions on one thread with no parallel network transfer: aggregate throughput is bounded by one synchronous chain, and a tick with 4 active sessions plus a planner stat lasts the sum of their round trips (~0.5-1 s+ at 100 ms RTT). During such a tick, debounce stabilization, tick-side fs-event draining (ingest itself continues on the watcher thread), remote polling, pause/control application, and fresh IPC status snapshots are all delayed. This is the structural ceiling on cloud sync throughput and runtime responsiveness once a real network provider (gdrive, C8-52) is active.

**Suggested fix**: Move transfer sessions (and planner remote stats) onto worker threads or async tasks, keeping the workgate permits as the concurrency limiter and the tick loop as the orchestrator that harvests completions. At minimum, keep provider network calls off the same thread that drains fs events and runs debounce.

### [medium] Ignore-file events inside ignored directories trigger repeated full-tree filter rebuilds

**Category**: perf · **Where**: `core/daemon/src/fs_events.rs:294` · **Review group**: ingest

record_callback_result (fs_events.rs:294) calls note_observed_path before the should_ignore check, so a created/modified .gitignore or .vaporignore anywhere under the watch root — including inside ignored/heavy subtrees like node_modules — sets reload_requested. rebuild_if_requested then runs once per runtime tick (runtime.rs:416), synchronously rebuilding the filter on the runtime thread. During a package install writing many .gitignore files over several seconds, this triggers a rebuild on nearly every tick, stalling tick work. These rebuilds are entirely wasted: collect_ignore_files skips node_modules/.git/target/etc. (is_heavy_ignore_discovery_skip_dir), so those files can never contribute rules and each rebuild produces an identical filter. Note the rebuild walk skips those heavy directories, so its per-run cost is a walk of the non-heavy tree plus ignore-file parsing/glob compilation — significant on large sync roots but not the multi-minute node_modules walk. Fix by skipping note_observed_path for paths under the heavy-skip-dir list and/or debouncing rebuilds so bursts coalesce into one walk; gating purely on should_ignore would incorrectly drop reloads for .gitignore files in rule-ignored (non-heavy) directories, which do contribute rules under current discovery semantics.

**Suggested fix**: Only request a reload when the observed ignore file would actually contribute rules: check should_ignore(path) (and the heavy-skip-dir list) before calling note_observed_path, or debounce/rate-limit rebuilds (e.g. rebuild at most once per N seconds of ignore-file quiet) so bursts coalesce into one walk.

### [medium] One profile's watcher/root failure at startup aborts the entire multi-profile daemon  ✅ DONE

**Category**: improvement · **Where**: `core/daemon/src/multi_runtime.rs:619` · **Review group**: runtime-shell

start_with_state_root uses `?` on per-profile startup steps — DurableStateDb::open_with_corruption_recovery (multi_runtime.rs:202), DaemonRuntime::build_with_app (line 229, which `?`s normalize_watch_root at runtime.rs:747-751), schedule_startup_reconcile (line 249) — and start_deduplicated_watchers `?`s normalize_watch_root (line 619) and FsEventsWatcher::start_with_shared_filter (lines 639-643). Any one profile hitting a startup failure — a per-profile DB open error (permissions, non-recoverable disk error), a canonicalize failure on its local root, an FSEvents/notify stream creation failure (fd exhaustion), or a reconcile-scheduling DB error — fails MultiProfileRuntime::start entirely, so run_daemon exits, every other healthy profile stops syncing, and the crash-loop guard starts counting. (Note: a local root missing/uncreatable at resolve time already degrades gracefully — resolve_local_directory returns None and the profile is skipped — so the root-vanished case is only a narrow race between resolve and start.) This is inconsistent with the module's own blast-radius discipline: an invalid provider degrades per-profile (lines 203-215), and mid-run failures suspend only the offending profile.

**Suggested fix**: Treat per-profile startup failures like tick failures: catch the error, mark that slot failed = Some(reason) (skipping its watcher), and continue composing the remaining profiles. Only fail start() when zero profiles could be composed.

### [medium] should_ignore linearly evaluates every compiled rule per event on the fs-watch callback thread  ✅ DONE

**Category**: perf · **Where**: `core/daemon/src/path_filter.rs:195` · **Review group**: ingest

should_ignore runs a last-match-wins loop over all compiled rules, and each rule may hold several GlobMatchers (unanchored patterns expand to 2+, directory patterns to 4). It executes inside the notify/FSEvents callback (fs_events.rs:295) under the filter RwLock. A monorepo with a few thousand aggregate gitignore lines yields ~5-10k regex evaluations per event; a 5,000-event burst (git checkout, build) is tens of millions of regex executions on the FSEvents callback thread. The existing guardrail test only exercises the ~30 default rules, so this regression is invisible to CI. When the callback thread stalls, the kernel FSEvents queue backs up and events are delivered late or coalesced away, inflating sync latency during exactly the bursts the debounce/storm machinery is designed to handle.

**Suggested fix**: Compile all patterns into a single globset::GlobSet and use matches_candidate_into to get the set of matching rule indices in one pass (last-match-wins = highest matching index), or use the `ignore` crate's Gitignore which is built for this. Both reduce per-event cost from O(rules) regex runs to one automaton pass.

### [medium] Ignore rules (including negations) are loaded from ignore files inside user-ignored directories  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/path_filter.rs:232` · **Review group**: ingest

collect_ignore_files walks the whole tree skipping only the hardcoded SKIP_DIRS name list and never consults the user's ignore rules, unlike git, which never reads ignore files inside excluded directories. Concrete failure: the user ignores `vendor/` via a pre-user rule, and a vendored package ships `vendor/pkg/.gitignore` containing `!important.txt`. That negation compiles (base-anchored) as an Allow rule in the gitignore group, evaluated after pre_user_rules in the last-match-wins loop, so vendor/pkg/important.txt is re-included and synced despite the user explicitly excluding vendor/. (If the user instead excludes vendor/ via root .vaporignore, a vendored .gitignore negation is neutralized because the vaporignore group is appended after the gitignore group — but a vendored nested .vaporignore negation still leaks, since it sorts after the root .vaporignore within the same group.) Third-party files the user never opted into control what Vapor syncs. Secondarily, the unconditional walk descends huge ignored trees whose names are not in SKIP_DIRS (e.g. Pods, .venv, vendor), making startup and every filter rebuild scale with content the filter is supposed to exclude.

**Suggested fix**: During the discovery walk, apply the already-compiled preceding rules (pre-user rules and parent ignore files) to prune ignored directories, mirroring git's behavior of never reading ignore files under excluded paths. This fixes both the negation leak and the walk cost.

### [medium] Remote poller drains one 256-change page per cadence: large remote bursts take minutes to enqueue

**Category**: perf · **Where**: `core/daemon/src/remote_sync.rs:139` · **Review group**: sync-pipeline-e2e

poll_if_due issues exactly one poll_changes call (REMOTE_CHANGES_PAGE_MAX=256) per due cadence and then waits out the full cadence again, even when the returned page was full — i.e., when more changes are known to be pending right now (Google Drive even returns nextPageToken in that case, which the provider folds into next_cursor without surfacing a has-more signal). Concrete scenario: a collaborator adds 5,000 files remotely: 20 pages x 5 s (IdleDrain) = ~100 s just to enqueue the download intents; under user activity (Throttled, 60 s cadence) the same backlog takes ~20 minutes before most intents even enter the durable queue. (Initial sync against a populated cloud root is NOT affected: a cursor=None baseline returns an empty page and pre-existing content is handled by reconcile.) The cadence exists to bound steady-state polling cost, but gating page-draining on it turns a known backlog into artificial latency, most egregiously under IdleDrain whose purpose is aggressive draining while idle.

**Suggested fix**: Loop while the returned page is full (bounded, e.g., a few pages per tick to preserve interruptibility), or call request_immediate_poll()-equivalent when page.changes.len() == REMOTE_CHANGES_PAGE_MAX so the next tick continues draining instead of waiting the full cadence.

### [medium] Paused daemon keeps polling the remote changes feed, contradicting the pause semantics documented in the same block  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/runtime.rs:451` · **Review group**: runtime

The comment at lines 447–450 states 'a paused daemon also skips polling (nothing would be leased anyway)', but the guard is only `if self.cloud_root_ready` — `paused` (computed at line 430) is not consulted, and RemotePoller::poll_if_due (core/daemon/src/remote_sync.rs) checks only throttle state (cadence_for + remote_poll_allowed), never run_state. Concrete failure: user runs `vapor pause` (or the mass-deletion guard trips); the daemon continues issuing provider network polls every 5s at IdleDrain cadence (REMOTE_POLL_IDLE_DRAIN_SECONDS), processing change pages, and writing durable enqueues — network and DB activity that pause is documented to stop. For a Google Drive provider this is ongoing API traffic while the user believes sync is fully paused.

**Suggested fix**: Gate the poll on `!paused`: `if self.cloud_root_ready && !paused { report.remote_poll = self.remote_poller.poll_if_due(...) }`, or make poll_if_due check app.snapshot().run_state == Running. Enqueued intents already survive durably, so skipping the poll loses nothing.

### [medium] Strict FIFO lease order lets a whole-scope reconcile backlog starve fresh user edits; priority classes only apply within one flush batch

**Category**: perf · **Where**: `core/daemon/src/runtime.rs:1539` · **Review group**: runtime

flush_scheduler_to_durable_queue sorts by intent_priority_rank only *within one tick's batch* (the sort merely orders id assignment inside a single coalesced enqueue), while state_db leasing is strictly `ORDER BY available_at_ms ASC, id ASC` with no priority column (state_db.rs lease_ready_batch). Mechanism of the latency problem: a startup/cursor-expiry/user-requested whole-scope reconcile of a large tree enqueues thousands of Download/Upload intents at earlier (available_at, id) pairs; a file the user edits afterwards gets a later pair and is leased only after the entire reconcile backlog drains at planner-cap width. With tens of thousands of backlog intents and upload concurrency of 4, the user's active edit can wait hours to sync, defeating the C8-56 priority-class intent ('key config and code paths enqueue before lockfile noise') for any work that spans more than one flush batch.

**Suggested fix**: Persist the priority rank as a durable column and lease with `ORDER BY priority_rank, available_at_ms, id` (or interleave: reserve one planner slot per tick for the newest non-reconcile intent so fresh edits make progress while backlog drains).

### [medium] Per-path serialization conflict breaks the whole admission batch: one in-flight long transfer starves all new work admissions

**Category**: perf · **Where**: `core/daemon/src/runtime.rs:1668` · **Review group**: sync-pipeline-e2e

In process_ready_queue (runtime.rs:1659-1668), when try_start_staged_intent returns false the intent is requeued +1s and the loop breaks, abandoning the rest of the leased batch. try_start (executor.rs) returns false for executor-full and no-planner-permit (global; break is right) but also for the per-path active_paths serialization check (not global). A re-save of a file with a long in-flight transfer creates a fresh pending row (enqueue_intents_coalesced only coalesces pending rows), which repeatedly fails the per-path check. Because lease_ready_batch orders by (available_at_ms, id) and the whole abandoned batch is requeued to the same now+1s, any intent that lands in the same lease batch behind the blocked duplicate (higher id) is pinned behind it in lockstep for the entire remaining transfer duration — e.g., a debounce-flushed burst of saves colliding with the duplicate's ready tick gets stuck for minutes — plus one lease+requeue durable write pair per pinned intent per second. Intents arriving in the ~3/4 of 250 ms ticks where the duplicate is not yet ready (requeue delay is 1 s) sort earlier and are admitted normally, so this is partial, deterministic starvation of colliding batches rather than near-total starvation of all admissions. Fix: distinguish path-busy from capacity/permit exhaustion (enum return or pre-check active_paths) and continue instead of break for the path-busy case.

**Suggested fix**: Distinguish the per-path-busy case from permit/capacity exhaustion (e.g., have try_start return an enum, or check active_paths in the runtime before calling) and `continue` instead of `break` when only that one path is blocked. Optionally skip leasing rows whose path is currently active.

### [medium] Reconcile walk fixed at 8 directories per tick (~32 dirs/s) regardless of throttle state

**Category**: perf · **Where**: `core/daemon/src/runtime.rs:1723` · **Review group**: sync-pipeline-e2e

process_reconcile_walk (runtime.rs:1723) always passes the fixed RECONCILE_DIRS_PER_CHECKPOINT=8 per-tick budget. Reconcile walks only ever run under IdleDrain (the controller refuses/interrupts in any other state), but within IdleDrain the budget never scales with the available headroom: at the 250 ms busy tick the walk is capped at ~32 dirs/s, and each 500 ms slice expiry additionally costs a durable lease+requeue round trip plus re-lease latency. Startup reconstruction or cursor-expiry recovery over a 40,000-directory tree on an idle, plugged-in machine therefore needs >= ~21 minutes of ticking before the scope converges (and blocks all other intents during the 60 s startup barrier window). Fix: scale the IdleDrain per-tick directory budget up (or make the walk time-budgeted against RECONCILE_SLICE_MILLIS); do NOT add walking under Throttled — that would loosen the existing IdleDrain-only gating. Note the gain is largest when per-directory cost is cheap; with a real cloud provider, sequential enumerate RPC latency becomes the dominant bound instead.

**Suggested fix**: Scale the per-tick directory budget with the throttle state (e.g., 32-64 dirs under IdleDrain, 8 under Light, 1-2 under Throttled), or make the walk time-budgeted per slice (RECONCILE_SLICE_MILLIS already exists) instead of a fixed directory count.

### [medium] Echo suppression hashes entire files unchunked on the tick thread

**Category**: perf · **Where**: `core/daemon/src/runtime.rs:1810` · **Review group**: sync-pipeline-e2e

is_local_self_write_echo (core/daemon/src/runtime.rs:1810) calls hash_hex_of_file — a synchronous, unbudgeted whole-file SHA-256 — inline in stabilize_events on the runtime tick thread whenever a live write-echo record exists for the path and the observed size matches. After a large (e.g. multi-GB) download, the executor records the echo (size+hash, 30 s TTL); when the watcher event stabilizes, the tick loop reads and hashes the entire file inline, stalling debounce release, remote polling, and executor advancement for the duration. This bypasses the throttle-gated 8 MiB-per-tick budgeted hash stage (HASH_STAGE_STEP_BYTES, executor.rs:450) and, because stabilize_events runs before pause/throttle gating, the hash can execute even under Suspended, contradicting the "under Suspended, hashing stops" invariant. A same-size user edit within the TTL pays the same one-time cost (correctness is preserved; the hash mismatch lets the event through).

**Suggested fix**: Route the echo-confirmation hash through the budgeted streaming hash machinery (or a bounded-size fast path: hash inline only below a few MB, otherwise defer the decision to a chunked check across ticks). Alternatively correlate large-file echoes via the op-id tag + size + mtime instead of a full content hash.

### [medium] list_queue_intents ORDER BY cannot use the ready index — full scan and sort per diagnostics query  ✅ DONE

**Category**: perf · **Where**: `core/daemon/src/state_db.rs:201` · **Review group**: state-db

list_queue_intents (core/daemon/src/state_db.rs:201) orders by (available_at_ms, id) with no state constraint, so the sole index idx_queue_intents_ready(state, available_at_ms, id) is unusable and SQLite performs a full table scan plus temp b-tree sort (verified via EXPLAIN QUERY PLAN). This is worse than a per-poll cost: the diagnostics snapshot is rebuilt and pushed to the IPC service at the end of every daemon run cycle (250 ms busy / 1 s idle) via the always-attached status publisher (bootstrap.rs:150, multi_runtime.rs:406→539→runtime.rs:981), so with a large backlog (100k+ rows during initial sync) the daemon pays a continuous O(N) scan ~4x/second per profile, independent of whether anyone polls status.

**Suggested fix**: Add an index on (available_at_ms, id), or query the two states separately through the existing index and merge the top rows in memory.

### [medium] Coalesced enqueue dedup lookup has no supporting index — full table scan per intent on the ingest flush path  ✅ DONE

**Category**: perf · **Where**: `core/daemon/src/state_db.rs:752` · **Review group**: state-db

enqueue_intents_coalesced (state_db.rs:748-764) runs 'SELECT id FROM queue_intents WHERE path_text = ? AND kind = ? AND state = ?' once per intent in the batch. The only index is idx_queue_intents_ready(state, available_at_ms, id); SQLite uses its leading state=? equality but then must visit every pending row (index entry + rowid lookup) to test path_text/kind — no path_text index exists. During initial sync or a large reconcile the queue legitimately holds the whole tree (e.g. 200k pending rows), so every debounce flush of M events costs O(200k × M) row visits inside an Immediate write transaction, burning CPU (battery) and holding the writer lock exactly when event storms make flushes largest; reconcile_walk.rs:369 enqueues the tree through the same function, making the initial enqueue itself quadratic. enqueue_startup_reconcile_intent has the same unindexed shape but runs once per startup, so its impact is negligible.

**Suggested fix**: Add CREATE INDEX idx_queue_intents_path ON queue_intents(path_text, kind, state) (bump schema/migration accordingly); optionally fold the check into a single INSERT ... WHERE NOT EXISTS per row.

### [medium] failed_intents table grows without bound — no retention, pruning, or clearing path anywhere in the codebase  ✅ DONE

**Category**: perf · **Where**: `core/daemon/src/state_db.rs:1059` · **Review group**: state-db

Tombstones get prune_tombstones (30-day retention, called from the runtime) and the timeline is capacity-bounded, but nothing ever deletes from failed_intents (no 'DELETE FROM failed_intents' exists in any crate). Authentication failures are classified terminal, so a single expired/revoked OAuth token during a large backlog finalizes every leased intent into failed_intents — hundreds or thousands of rows per incident, each carrying up to MAX_DIAGNOSTIC_TEXT_LENGTH of error text, accumulating across months of operation. The DB file grows monotonically on the user's device (direct violation of the low-device-impact priority) and failed_depth/diagnostics queries slow down with it.

**Suggested fix**: Add a retention sweep symmetrical to prune_tombstones (e.g. FAILED_INTENT_RETENTION_MILLIS, pruned at startup/periodically) and/or a bounded row cap keeping only the newest N failures; expose a CLI clear/retry path for the surfaced failures.

### [medium] v3->v4 migration resets the queue AUTOINCREMENT sequence when the queue is empty, allowing reused ids to collide with failed_intents primary keys  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/state_db.rs:1120` · **Review group**: state-db

migrate_v3_to_v4 rebuilds queue_intents via CREATE TABLE + INSERT...SELECT + DROP + RENAME. SQLite's sqlite_sequence entry for the new table is derived only from the rows actually copied; the old table's sequence is dropped. Concrete failure: a v3 database where intent id 7 previously failed terminally (row id 7 lives in failed_intents) and the queue has fully drained (0 rows — the common steady state). After migration, new intents restart at id 1; when the reused id 7 eventually fails terminally, finalize_leased_failure's INSERT INTO failed_intents hits a PRIMARY KEY constraint violation -> StateDbError::Sql -> tick error, and the intent stays leased. The stale sweep later re-pends it with attempt_count=0, it fails again, collides again — the intent can never be finalized and produces recurring tick errors indefinitely.

**Suggested fix**: After the copy, restore the sequence explicitly (INSERT/UPDATE sqlite_sequence for queue_intents to MAX(old sequence, MAX(failed_intents.id))), or stop reusing queue ids as failed_intents PKs (give failed_intents its own rowid and store source_intent_id as a plain column).

### [low] Shutdown signal does not wake the tick loop, delaying clean exit by up to a full idle sleep

**Category**: improvement · **Where**: `core/daemon/src/bootstrap.rs:184` · **Review group**: runtime-shell

install_shutdown_signal_handlers registers runtime::request_shutdown, which only stores the SHUTDOWN_REQUESTED atomic (runtime.rs:56-58) and never notifies the MultiProfileRuntime tick_waker. run_forever checks the flag only at the top of the loop and otherwise sleeps in tick_waker.wait_timeout (multi_runtime.rs:430) — up to IDLE_TICK_MILLIS (1 s) when idle. Concrete effect: `vapor stop` / launchd SIGTERM consistently takes up to ~1 s longer than necessary to exit, which slows service round-trips (the CI service lifecycle test and app quit path both wait on daemon exit) for no benefit.

**Suggested fix**: Have the shutdown path notify the waker: e.g. register a handler closure that calls request_shutdown() and then notifies the multi runtime's TickWaker (a process-global shutdown waker registration alongside SHUTDOWN_REQUESTED keeps the signal-safe flag-flip pattern).

### [low] Recorder drain-then-apply is not atomic, so concurrent state readers can apply event batches out of order

**Category**: improvement · **Where**: `core/daemon/src/event_intents.rs:849` · **Review group**: ingest

drain_incoming_into_maps takes the incoming batch under the incoming_events mutex, releases it, then separately locks maps to apply. BoundedFsEventRecorder is Send+Sync and with_state/with_mut_state are &self, inviting calls from IPC/status threads. If two threads drain concurrently, thread A can take batch [Removed(x)] (older), thread B take batch [Created(x)] (newer), and B can acquire the maps lock first — the merged pending record's last_event_kind ends up Removed for a file that exists, propagating a remote delete that only a later reconcile repairs. Today this is latent (runtime.rs only drains from the single tick thread), but nothing in the type or API enforces that, and has_pending_work already calls with_state on a hot path where a future refactor could move it off-thread.

**Suggested fix**: Acquire the maps lock before taking the incoming batch (hold it across take + apply), or funnel all drains through a single &mut entry point so the compiler enforces single-threaded draining.

### [low] Watch-root/filter-root mismatch is only a debug_assert; in release it silently disables all ignore rules

**Category**: improvement · **Where**: `core/daemon/src/fs_events.rs:225` · **Review group**: ingest

start_with_shared_filter canonicalizes its own watch root but only debug_asserts (fs_events.rs:225) that the caller-provided SharedEventPathFilter is anchored to the same path; in release builds a mismatched filter root makes should_ignore's strip_prefix (path_filter.rs:185) fail for every event, silently bypassing all ignore rules with no log. All current production callers canonicalize the root first, so this is defensive hardening of a public API against future caller misuse rather than a reachable bug; returning a real FsEventsWatcherError on mismatch would make misuse fail loudly on every build profile.

**Suggested fix**: Return a real FsEventsWatcherError (e.g. FilterRootMismatch) when path_filter.watch_root() != normalized watch_root instead of debug_assert_eq!, so misuse fails loudly on every build profile.

### [low] After a caught panic, the suspended slot's runtime keeps being read every tick despite AssertUnwindSafe, outside any panic catcher  ✅ DONE

**Category**: improvement · **Where**: `core/daemon/src/multi_runtime.rs:397` · **Review group**: runtime-shell

tick_all wraps only slot.runtime.tick in catch_unwind(AssertUnwindSafe(..)). After a profile panics and is suspended, its runtime is still dereferenced every tick outside any catcher: the auto-tune loop reads state_db().queue_depth() and retry_slowdown_until() from failed slots (multi_runtime.rs:393-401, also skewing the tuning signal with a queue that will never drain), and aggregate_status/profile_summaries call snapshot(), mirror_counters(), conflict_count(), loop_suppression_count(), dropped_incoming_event_count(), and intent_diagnostics() on failed slots (509-551, 306-324). If any of these reads panics on the half-mutated state, the panic escapes and kills the daemon, defeating C8-24 containment. In practice most of these paths are panic-resistant (plain field reads, Result-based DB queries, and poison-tolerant locks via unwrap_or_else(into_inner) in lock_workgate and resource_budget_status); the main residual poisonable .expect is dropped_incoming_event_count (event_intents.rs), so this is a real but low-probability containment gap plus an auto-tune signal-quality issue.

**Suggested fix**: Skip failed slots in the auto-tune aggregation and have aggregate_status/profile_summaries emit a static suspended row (id, display_name, suspended_reason) for failed slots instead of live-querying their runtimes; alternatively capture a last-known-good snapshot at suspension time.

### [low] tick_all runs per-profile SQLite queries and publishes a full status snapshot on every tick, even fully idle

**Category**: perf · **Where**: `core/daemon/src/multi_runtime.rs:405` · **Review group**: runtime-shell

Every tick_all (250 ms busy / 1 s idle cadence, forever) runs auto-tune queue_depth() SQL per profile including suspended slots (multi_runtime.rs:397-401), then unconditionally publishes aggregate_status (line 405-407), which re-runs queue_depth() + failed_depth() + intent_diagnostics() (a SELECT of up to ~100 rows) per profile plus snapshot allocations (lines 509-551) — ~4N SQLite statements per tick while fully idle, with the auto-tune depths recomputed instead of reused. Note the 1 Hz idle wakeups occur anyway from the tick loop's wait_timeout, so this is redundant per-wakeup work (SQLite churn and allocations growing linearly with profile count), not additional wakeups; publish itself is only an in-memory mutex swap.

**Suggested fix**: Publish only when state changed (dirty flag set by tick reports / control requests / throttle transitions) or at a lower fixed cadence when idle; reuse the queue_depth values computed for auto-tune inside aggregate_status instead of re-querying; skip suspended slots in the auto-tune aggregation.

### [low] Timeline events hardcode DEFAULT_PROFILE_ID, misattributing all activity to 'default' in multi-profile daemons  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/runtime.rs:1040` · **Review group**: runtime

emit_timeline_events (runtime.rs:1040) and the flush-boost (line 1221) and mass-deletion-guard (line 1481) timeline pushes hardcode DEFAULT_PROFILE_ID, but multi_runtime.rs builds one DaemonRuntime per real profile and attaches the shared TimelineBuffer to each without conveying profile.id (DaemonRuntime has no profile-id field; RemotePoller at runtime.rs:843 is likewise hardcoded to DEFAULT_PROFILE_ID). With profiles 'work' and 'personal', conflicts, guard trips, mirror deletes, and run-state changes all land on the shared timeline tagged profile "default" — visible in `vapor timeline --json` (the plain-text renderer currently omits the profile field). Fix: store the resolved profile id on DaemonRuntime (mirroring the existing set_device_id pattern) and use it for every timeline push and the RemotePoller constructor.

**Suggested fix**: Store the resolved profile id on DaemonRuntime (it already carries device_id via a setter; multi_runtime already knows profile.id and passes it to RemotePoller-style consumers) and use it for every timeline push instead of DEFAULT_PROFILE_ID.

### [low] `vapor resume` while the cloud root is unavailable reports RunState::Running, masking the blocking Error condition  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/runtime.rs:1202` · **Review group**: runtime

apply_pending_control_requests handles a resume by unconditionally setting RunState::Running with a 'watching <dir>' reason. If cloud_root_ready is false (build set RunState::Error with the 'cloud sync directory ... unavailable' reason), the resume overwrites that Error state even though the tick loop still refuses to lease anything (paused = run_state == Paused || !cloud_root_ready). Concrete failure: cloud root unavailable → user pauses, then resumes → `vapor status` now shows Running / 'watching ...' while zero work is admitted and the actual blocker (unavailable cloud directory) has been erased from the reason string — observability no longer explains current state, violating the 'observability is sufficient to explain current state/reason' definition of done. intent_diagnostics still reports the right blocker, but the headline status lies.

**Suggested fix**: On resume, re-derive the run state: if !cloud_root_ready, restore the Error state/reason (cloud directory unavailable) instead of Running; only report Running when work can actually be admitted.

### [low] apply_memory_ceiling squeezes the timeline capacity but never restores it after memory pressure clears  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/runtime.rs:1318` · **Review group**: runtime

apply_memory_ceiling (core/daemon/src/runtime.rs:1300-1329) squeezes the shared activity timeline to budget_entries/4 in the over-budget branch but the hysteresis restore branch resets only the two SelfWriteCache bounds, never the timeline; since the configured timelineEventLimit is applied only once at daemon bootstrap, one transient memory-pressure event leaves the timeline capped at the squeeze value until restart, and repeated squeezes ratchet the cap to whatever the budget was at each event. Note: with all-default config (memoryPercent=10, timelineEventLimit=1000) the squeeze value coincidentally equals the default limit, so the observable loss of `vapor timeline` history depth requires a non-default timelineEventLimit (or memoryPercent) — diagnostics-only impact, no sync-correctness effect.

**Suggested fix**: In the restore branch, reset the timeline to its configured default capacity (mirror the echo-cache restore), e.g. timeline.set_max_entries(default_timeline_entries).

### [low] schedule_retry spans five separate implicit transactions; a crash mid-sequence loses the durable rate-limit slowdown marker  ✅ DONE

**Category**: improvement · **Where**: `core/daemon/src/state_db.rs:467` · **Review group**: state-db

schedule_retry performs intent_record (SELECT), requeue_leased_with_attempt_bump (UPDATE, own implicit tx), retry_slowdown_until (SELECT), set_state (INSERT, own tx), and a final intent_record re-read — with no enclosing transaction, unlike finalize_leased_failure which uses TransactionBehavior::Immediate. A crash between the requeue UPDATE and set_state persists the retried intent but drops the global rate-limit slowdown marker, breaking the documented crash invariant that retry slowdown is restored after restart (AGENTS.md §9.2). The failed intent itself keeps its slow per-intent backoff (available_at is persisted in the same UPDATE), but other queued work resumes at full speed against a provider that just rate-limited it until the next 429 re-sets the marker. The double intent_record read is also two avoidable queries per failure on the retry path.

**Suggested fix**: Wrap the whole operation in one TransactionBehavior::Immediate transaction (like finalize_leased_failure does) and build the returned record from the already-fetched row plus the known updates instead of re-reading.

### [low] MAX_ATTEMPT_COUNT terminal-failure contract has no implementer, so the attempt cap wedges intents instead of finalizing them  ✅ DONE

**Category**: improvement · **Where**: `core/daemon/src/state_db.rs:477` · **Review group**: state-db

schedule_retry errors with 'caller must finalize as terminal failure' at attempt_count >= MAX_ATTEMPT_COUNT, but the only caller (StagedExecutor::resolve_failure) just propagates the error with '?', and finalize_leased_failure rejects Transient/RateLimited kinds outright — so there is no legal way to finalize a transient failure at the cap. When reached, the tick errors, the intent stays leased, and the 15-minute stale sweep then resets attempt_count to 0 (line 614), silently defeating the cap and looping forever. Today the cap (10,000) is practically unreachable so this is latent, but the contract is dead code that turns into a tick-error loop the day the constant is lowered.

**Suggested fix**: Handle the cap in the executor: on schedule_retry reporting exhaustion, finalize with RetryFailureKind::Permanent ('retry budget exhausted'); or have state_db perform that conversion internally. Also stop resetting attempt_count in the in-run stale sweep.

### [low] Storm detector clones the full event path into every ancestor directory's window on each event

**Category**: perf · **Where**: `core/daemon/src/storm.rs:96` · **Review group**: ingest

observe_event builds a Vec<PathBuf> of all ancestors per event (directory_roots_for_path allocates one PathBuf per level), clones each ancestor again for the directory_windows entry lookup, and for each ancestor inserts a full clone of the event path into that window's path_last_seen plus runs a retain scan over the window map — roughly 2×depth PathBuf clones and depth map-scans per event on the runtime drain path. However, retention is bounded: once a directory window reaches the 200-unique-path or 600-event threshold, storm compaction absorbs subsequent events before they reach the detector, so per-window memory is capped near the thresholds and the worst case (tens of thousands of clones per burst) occurs only when a burst is spread across many directories that each stay below threshold.

**Suggested fix**: Store interned path IDs or relative-suffix hashes in path_last_seen instead of full PathBufs, reuse a scratch buffer for ancestor iteration (iterate Path::ancestors directly rather than collecting a Vec), and consider only tracking unique-path counts at the immediate parent while deriving ancestor rollups from child window aggregates.

### [low] 4 s default debounce window applies to the most common user documents

**Category**: perf · **Where**: `core/shared/src/constants.rs:340` · **Review group**: sync-pipeline-e2e

DEFAULT_DEBOUNCE_WINDOW_MILLIS=4000 (constants.rs:340) is the quiet window for every path not classified as lockfile, key-config, or code/text by debounce.rs classify_path — which includes .docx/.xlsx/.pptx, .pdf, and most images (svg/csv are the only image/data-like extensions in the code/text list). The windows are hardcoded (runtime always uses DebounceWindows::default(); no config knob). A saved Word document therefore cannot even enter the pipeline for 4 s of quiet time, and with 250 ms tick granularity plus per-tick executor stage latency reaches the cloud roughly 2.5–3 s later than an equivalent .txt (1.2 s window) — the documents ordinary users care about most sync slowest. Office atomic-save patterns settle within 1–2 s, and the per-path coalescing map (latest-wins, burst_count) already absorbs multi-event bursts, so a shorter document-class window would be safe.

**Suggested fix**: Add a document class (common office/document/image extensions) with a 1-1.5 s window like code/text, or lower the Other default toward 2 s and rely on the debounce map's coalescing (latest-wins, burst_count) to absorb multi-event save patterns.


### Addendum — additional verified findings (batch 2)

| Sev | Category | Location | Finding |
|---|---|---|---|
| critical | bug | `core/daemon/src/executor.rs:1357` | Two-way remote directory deletion applies remove_dir_all without checking children, wiping unsynced local files |
| high | bug | `core/daemon/src/executor.rs:841` | Download apply TOCTOU: a local write landing between the divergence check and the staging rename is silently overwritten and its watcher echo is then suppressed |
| high | bug | `core/daemon/src/profiles.rs:111` | VAPOR_LOCAL/CLOUD_SYNC_DIRECTORY env vars silently override every profile's per-profile directories, collapsing all profiles onto one root |
| high | bug | `core/daemon/src/reconcile_walk.rs:208` | Local read_dir/stat failure is treated as 'no local entries', so push-only reconcile enqueues remote Deletes for content that still exists locally |
| high | bug | `core/daemon/src/remote_sync.rs:80` | Self-write-cache TTL (30s) is shorter than the Throttled poll cadence (60s) and the Suspended pause, so the daemon's own upload echoes replay as remote changes |
| high | bug | `core/daemon/src/remote_sync.rs:228` | Remote poller enqueues intents with poll time instead of change.observed_at, so a stale remote Removed deletes a freshly re-uploaded/edited local file |
| medium | improvement | `core/daemon/Cargo.toml:10` | Daemon bypasses the core/platform FsWatcher trait with its own direct notify watcher |
| medium | bug | `core/daemon/build.rs:52` | build.rs emits no rerun marker for git worktrees or packed refs, embedding stale commit SHAs |
| medium | bug | `core/daemon/src/executor.rs:1209` | Crash/power loss between a completed provider upload and the durable index/completion write replays as a manufactured keep-both conflict duplicate on both replicas |
| medium | bug | `core/daemon/src/executor.rs:1276` | resolve_upload_conflict renames the local file before durably enqueuing the follow-up intents; a failure between the two strands the conflict copy and leaves the canonical path missing |
| medium | bug | `core/daemon/src/executor.rs:1404` | record_upload_index captures the local mtime after the upload finishes, pairing a post-edit mtime with the as-uploaded hash and enabling silent overwrite of a mid-upload edit |
| medium | improvement | `core/daemon/src/fs_events.rs:11` | Daemon local fs-watch bypasses the platform FsWatcher trait and duplicates the FSEvents mapping (already drifted) |
| medium | bug | `core/daemon/src/ipc_service.rs:142` | IPC config writes are unsynchronized read-modify-write with a shared fixed temp filename, so concurrent IPC calls lose updates |
| medium | bug | `core/daemon/src/lib.rs:419` | Retry-slowdown upload clamp is applied before the CPU-ceiling scale factor, which multiplies it back up during rate-limit storms |
| medium | bug | `core/daemon/src/lib.rs:421` | CPU-ceiling scaling can raise Throttled/Light caps far above the throttle ladder, contradicting the documented 'ceilings only ever lower' contract |
| medium | perf | `core/daemon/src/reconcile_walk.rs:142` | Reconcile walk performs up to 8 synchronous provider enumerations per chunk with no intra-chunk slice/throttle check, blocking the daemon tick loop |
| medium | bug | `core/daemon/src/remote_sync.rs:222` | Push-only strict mirror never restores a remotely-deleted directory because local_file_exists rejects directories |
| low | perf | `core/daemon/src/auto_tune.rs:105` | Auto-tuner regression check compares absolute queue depth, which is confounded by ingest — step increases are always rolled back exactly when a deep queue needs them |
| low | perf | `core/daemon/src/executor.rs:1274` | Download-side conflict resolution enqueues a redundant Download for a payload that is already fully staged |
| low | improvement | `core/daemon/src/executor.rs:1436` | record_download_index stores the daemon's own download op-id instead of the remote object's op-id, defeating the op-id fast path for every subsequent upload |
| low | bug | `core/daemon/src/reconcile_walk.rs:272` | Type-mismatch re-materialization intents do not share the clearing intent's path, so the claimed executor ordering does not hold for remote directories |
| low | improvement | `core/daemon/src/resource_budget.rs:257` | A single 1-second headroom blip cancels Active idle boost into a full down-ramp plus a fresh 30s up-ramp; RampingDown never re-checks the gates |
| low | bug | `core/daemon/src/safeguards.rs:73` | Rolling-window prune keeps future-dated events after a wall-clock rewind, so MassChangeGuard can spuriously pause sync and ActiveCodingHeuristic can pin Throttled — contrary to its documented fail-safe claim |

### [critical] Two-way remote directory deletion applies remove_dir_all without checking children, wiping unsynced local files  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/executor.rs:1357`

deletion_loses_to_local_state returns Ok(None) for anything that is not a regular file ('Directories and absent paths have no unsynced content to preserve; the apply path handles them'), but apply_remote_delete_locally then executes fs::remove_dir_all on directories — deleting the entire subtree with zero per-child preservation checks. Failure scenario: local `project/notes.txt` was just created and its Upload intent is still queued (throttled). Device B deletes the `project/` folder remotely; the feed emits a single Removed(project) change; ApplyRemoteDelete(project) passes the guard (path is a dir) and remove_dir_all deletes notes.txt, which had never been synced anywhere. This violates the two-way 'data preservation wins over deletion' contract that the same function enforces for individual files.

**Suggested fix**: When the ApplyRemoteDelete target is a directory in two-way mode, walk the subtree and run the per-file preservation check (sync-index provenance + divergence) on each child; delete only children that pass, preserve (and re-upload) the rest, and remove the directory only if it ends up empty. Alternatively, refuse directory tombstones and expand them into per-file ApplyRemoteDelete intents at plan time.

### [high] Download apply TOCTOU: a local write landing between the divergence check and the staging rename is silently overwritten and its watcher echo is then suppressed  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/executor.rs:841`

On TransferStep::Completed in two-way mode the executor runs preserve_diverged_local_before_apply (size/mtime check, then a synchronous, non-interruptible full-file hash whose duration scales with file size) and only afterwards apply_downloaded_payload does fs::rename(staging, local_path). A user write landing between the divergence decision and the rename is atomically clobbered — keep-both is violated inside its own implementation window. The loss is then hidden: local_echoes.record_write stores the downloaded hash+size, and when the user's edit event stabilizes, is_local_self_write_echo (runtime.rs:1779) hashes the current file — now the downloaded content — matches the record, and suppresses the event, so no Upload intent is created and no trace of the edit survives on either replica. The same check-then-act pattern exists in the remote-delete path: deletion_loses_to_local_state may hash the file, then apply_remote_delete_locally removes it and records a delete echo that suppresses the trailing Removed event, so an edit landing between the two is silently deleted.

**Suggested fix**: Close the window: always rename the existing local file aside (to a temp name) first, then rename the staging file in, then compare the set-aside file against the index/incoming hash and either delete it (unchanged) or promote it to the conflict-copy path (diverged). That makes the divergence decision operate on the exact bytes that were displaced instead of on a racy pre-check.

### [high] VAPOR_LOCAL/CLOUD_SYNC_DIRECTORY env vars silently override every profile's per-profile directories, collapsing all profiles onto one root

**Category**: bug · **Where**: `core/daemon/src/profiles.rs:111`

resolve_profiles builds an effective per-profile config and then calls sync_directories::resolve_with_config(&effective), but resolve_with_config (sync_directories.rs:32-33) reads VAPOR_LOCAL_SYNC_DIRECTORY / VAPOR_CLOUD_SYNC_DIRECTORY from the process environment FIRST, and env wins over the profile's explicit localSyncDirectory/cloudSyncDirectory override. Concrete failure: a user who set VAPOR_LOCAL_SYNC_DIRECTORY=~/Docs in the daemon's LaunchAgent/wrapper (from the single-profile era) later adds profiles [{id: docs, two-way, ~/Docs}, {id: mirror, pull-only, ~/CloudMirror}]. Every profile now resolves local root ~/Docs; the pull-only profile strict-mirrors its cloud root over ~/Docs, permanently deleting every local file not present in that cloud directory — a destructive mode applied to a directory the user never opted in for. The override is silent (no log) and directly contradicts the docs' claim that per-profile fields 'replace the top-level value outright'.

**Suggested fix**: Apply the env-var layer only to the top-level/implicit-default resolution (or only when the profile does not set the field). When profiles are configured and the env var is set, either ignore it with a loud warning or refuse to start; never let a process-wide env value replace an explicit per-profile root, especially for one-way profiles.

### [high] Local read_dir/stat failure is treated as 'no local entries', so push-only reconcile enqueues remote Deletes for content that still exists locally

**Category**: bug · **Where**: `core/daemon/src/reconcile_walk.rs:208`

In compare_directory, a non-NotFound fs::read_dir error (lines 208-216) only logs a warning and then FALLS THROUGH to remote enumeration and pair comparison with an empty local view. The same applies per-entry when symlink_metadata fails (line 189, silent `continue`). Every remote entry then classifies as (None, Some(remote)), and under SyncMode::PushOnly that branch (line 356-362) enqueues PendingIntentKind::Delete, which the executor executes as an unconditional RemoteDelete (executor.rs:1060). Concrete failure: the daemon runs a push-only profile and read_dir on a subdirectory returns EPERM (macOS TCC denial for ~/Documents), EACCES, EMFILE during heavy sync, or EIO — the walk deletes every corresponding file/directory in the cloud mirror even though the local (authoritative) copies are intact. If the error is at the scope root, the entire cloud tree is deleted. The data reappears only after the local error clears and a later walk re-uploads everything (losing provider-side revision history/shared links and generating a delete+reupload storm). In pull-only/two-way the same fall-through causes spurious Download churn for every entry in the unreadable directory.

**Suggested fix**: On any non-NotFound read_dir error, skip the directory entirely (return Ok(()) without comparing), and on a symlink_metadata error skip the whole directory or at least never let that entry classify as locally-absent. A strict-mirror delete must only be derived from a positively-observed absence, never from a failed local read.

### [high] Self-write-cache TTL (30s) is shorter than the Throttled poll cadence (60s) and the Suspended pause, so the daemon's own upload echoes replay as remote changes

**Category**: bug · **Where**: `core/daemon/src/remote_sync.rs:80`

remote_echoes records an upload at completion with DEFAULT_TTL_MILLIS = 30_000, but under Throttled the poll cadence is REMOTE_POLL_THROTTLED_SECONDS = 60, and under Suspended polling stops entirely for arbitrarily long; under memory pressure runtime.rs squeezes the TTL to MIN_TTL_MILLIS = 5_000, below even the Light cadence (15s). matches_write requires a live record, so once expired the daemon's own op-id carried in the feed change is never consulted, and neither remote_sync.rs nor the Download planning path checks the persisted sync_index.last_op_id as a fallback. Interleaving: an upload of P completes just after a poll while Throttled; the echo expires at +30s; the next poll at +60s sees CreatedOrModified(P) with our own op-id but no live record → enqueues Download(P) and re-downloads the daemon's own upload (wasted transfer exactly when the machine is under pressure). If the user edited P locally in the gap, preserve_diverged_local_before_apply sees local hash != index hash != incoming hash and manufactures a spurious keep-both conflict copy of the user's edit, which then uploads as a phantom conflict file to the cloud. (The originally claimed self-sustaining upload/download ping-pong is overstated: the download's local watcher echo is checked at the 1s-tick debounce drain, well within the TTL, so the loop normally terminates after one spurious download — but the per-upload echo replay and phantom conflicts violate the loop-prevention TTL discipline and the low-impact goal under Throttled/Suspended.)

**Suggested fix**: Make echo suppression not depend on a TTL shorter than the worst-case observation delay: either bound the TTL below by the active poll cadence (extend records while polling is deferred/suspended), or add a durable second-line correlator — e.g. suppress a CreatedOrModified whose op_id equals sync_index.last_op_id (and hash equals index.content_hash) for that path, which is already persisted.

### [high] Remote poller enqueues intents with poll time instead of change.observed_at, so a stale remote Removed deletes a freshly re-uploaded/edited local file

**Category**: bug · **Where**: `core/daemon/src/remote_sync.rs:228`

RemoteChange carries observed_at, but every batch.push uses `now` (poll time) as the intent timestamp. The C8-17 deletion guard (executor.rs deletion_loses_to_local_state) compares index.updated_at > intent.enqueued_at to detect 'deletion older than last sync' — with enqueued_at = poll time that comparison is wrong whenever the feed lags. Interleaving: (1) device B deletes remote P at t1; (2) device A edits local P and its Upload recreates remote P at t2 (RemotePrecondition::Absent path), setting index.updated_at = t2; (3) A's next poll at t3 (up to 60s later under Throttled) delivers the stale Removed(P); enqueued_at = t3 > t2, so the 'deletion is older than last sync' preservation does not fire, the local file matches the post-upload index (not diverged), and apply_remote_delete_locally deletes A's local file — even though remote P (A's own upload) still exists. The upload's feed echo is suppressed, so nothing restores the file until the next whole-scope reconcile (startup/cursor-expiry only): the just-edited file silently vanishes locally for an unbounded time.

**Suggested fix**: Enqueue remote-sourced intents with change.observed_at as their event time (so the guard's ordering comparison is against when the deletion actually happened), and additionally have the ApplyRemoteDelete planner re-stat the remote path — if the object exists again, the Removed event is stale and must complete as a no-op.

### [medium] Daemon bypasses the core/platform FsWatcher trait with its own direct notify watcher

**Category**: improvement · **Where**: `core/daemon/Cargo.toml:10`

vapor-daemon keeps its own notify = "=8.2.0" dependency and fs_events.rs:230 constructs notify::recommended_watcher directly for the production local-watch path, bypassing the core/platform FsWatcher trait (NativeFsWatcher) that the filesystem provider already uses for the remote root (feed.rs:25). The two notify→event translations have already drifted: the daemon splits paired renames (RenameMode::Both) into Removed(from)+Created(to) and records watcher errors, while the platform macOS impl maps all name-modify events uniformly to Renamed and silently drops errors. This conflicts with the CLAUDE.md §2/§8 intent (platform-sensitive watch code behind core/platform traits, contract-tested per OS) and leaves the platform-trait contract suite not covering the shipping local-watch path. No documented exception exists; docs/tasks/core.md C3-2 shows the trait impl was ported from fs_events.rs but the daemon was never migrated onto it.

**Suggested fix**: Route the daemon's local watcher through vapor_platform::fs_watch (keeping the callback-discipline half in fs_events), drop the direct notify dependency from vapor-daemon, and let the trait contract suite cover the shared translation logic; if the split is intentional, document it in AGENTS.md as an explicit exception.

### [medium] build.rs emits no rerun marker for git worktrees or packed refs, embedding stale commit SHAs

**Category**: bug · **Where**: `core/daemon/build.rs:52`

emit_git_rerun_markers resolves HEAD's ref with git_dir.join(reference) and only prints cargo:rerun-if-changed if that loose ref file exists. In a git worktree (gitdir points to .git/worktrees/<name>, where refs live in the commondir, not the worktree gitdir) the path never exists — verified in this very checkout: /Users/alex/Drive/Projectos/vapor/.git/worktrees/budapest/refs/heads/<branch> does not exist. The same happens in a normal checkout after `git pack-refs`/`git gc` moves the ref into packed-refs. Concrete failure: a contributor (or Conductor agent, which always works in worktrees) builds vapord, makes a new commit on the same branch, rebuilds — cargo sees VERSION and HEAD unchanged, skips build.rs, and the binary's GIT_COMMIT_SHORT (surfaced in --version, logs, diagnostics, support bundles) reports the previous commit. This silently breaks the AGENTS.md §7.1 provenance requirement for every incremental dev/worktree build; HEAD only changes on branch switch.

**Suggested fix**: In resolve_git_dir/emit_git_rerun_markers, read the gitdir's `commondir` file when present and resolve refs relative to it; additionally emit cargo:rerun-if-changed for `<commondir>/packed-refs`, and emit the ref path even when it does not yet exist (cargo re-runs when a watched missing path appears).

### [medium] Crash/power loss between a completed provider upload and the durable index/completion write replays as a manufactured keep-both conflict duplicate on both replicas  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/executor.rs:1209`

After an upload completes at the provider, record_upload_index + complete_leased are separate, non-fsync-guaranteed writes (PRAGMA synchronous=NORMAL explicitly allows the final transactions to roll back on power loss). On replay the leased intent is recovered and re-planned: the remote now carries the crashed attempt's op-id (op-ids embed attempt count and timestamp, so it never matches the stale index.last_op_id), and the op-id-mismatch branch compares the remote hash only against the stale index.content_hash — new content != old hash -> resolve_upload_conflict fires even though local and remote are byte-identical. Result: the local file is renamed to a conflict-copy path, the copy is uploaded, and the canonical is re-downloaded — a phantom duplicate file appears on both replicas after every crash/power-loss in the post-upload window. This is the documented at-least-once replay path producing a deterministically wrong outcome (the S15-style hash check fixed the untagged-external-write case but not this one).

**Suggested fix**: In the op-id-mismatch branch, when remote_hash != index.content_hash do not conflict immediately; set plan.verify_remote_before_upload = true and defer to the upload gate, which compares the remote hash against the *local* content hash — identical content (the crash-replay case) then converges silently, and genuine divergence still resolves as keep-both.

### [medium] resolve_upload_conflict renames the local file before durably enqueuing the follow-up intents; a failure between the two strands the conflict copy and leaves the canonical path missing  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/executor.rs:1276`

resolve_upload_conflict renames the local file to its conflict-copy path before durably enqueuing the follow-up intents. If enqueue_intents_coalesced fails (transient SQLite error) or the process dies between rename and enqueue, the retried original Upload intent stats the renamed-away path, hits NotFound, and completes as the "local file vanished before upload" no-op — so the Download(original) follow-up is never enqueued. In a live daemon the conflict copy is usually still uploaded via its unsuppressed fs-watch create event, but the remote canonical is never re-downloaded to the original path (the changes feed saw no remote change), leaving the canonical file missing locally until the next daemon restart's whole-scope startup reconcile repairs it.

**Suggested fix**: Reorder for crash-safety: durably enqueue the Download(original) follow-up before performing the rename, and enqueue the Upload(conflict_copy) immediately after the rename (or perform rename + enqueue such that a retried original intent detects the half-finished state — e.g. by finding the conflict-copy marker for its own enqueued_at timestamp — and completes the enqueue instead of no-opping).

### [medium] record_upload_index captures the local mtime after the upload finishes, pairing a post-edit mtime with the as-uploaded hash and enabling silent overwrite of a mid-upload edit  ✅ DONE

**Category**: bug · **Where**: `core/daemon/src/executor.rs:1404`

The sync-index entry written after an upload stores the streamed content hash (bytes actually transferred across ticks) together with the file mtime read at completion. A same-length edit during a multi-tick upload yields an index with post-edit mtime/size but pre-edit hash; both divergence quick-checks (executor.rs:1317-1319 and 1374-1377) treat an mtime match as non-divergence and skip hashing, so a subsequent remote apply (whose Download intent supersedes the queued re-upload via the scheduler's latest-wins rule) can silently overwrite the unsynced local edit with no conflict copy. Caveat narrowing exposure: the stored mtime is millisecond-truncated while the comparison uses full-precision SystemTime equality, so on APFS the fast path almost never matches and the hash fallback detects the divergence correctly; the silent overwrite is realistically reachable only on sync roots with coarse mtime granularity (exFAT, SMB, second-granularity filesystems) or rare ms-aligned timestamps.

**Suggested fix**: Capture (mtime, size) when the hash stage opens the file, and after the upload completes re-stat: if (mtime, size) changed during the transfer, record local_modified_at = None (forcing the hash path on the next divergence check) or skip the index write entirely and let the pending re-upload own it.

### [medium] Daemon local fs-watch bypasses the platform FsWatcher trait and duplicates the FSEvents mapping (already drifted)

**Category**: improvement · **Where**: `core/daemon/src/fs_events.rs:11`

core/daemon/src/fs_events.rs bypasses the vapor_platform::fs_watch trait: it imports notify directly and duplicates the watcher plumbing (root validation, canonicalization, OS-event→kind mapping) that NativeFsWatcher already provides and that the filesystem-provider changes feed consumes. The two mappings have already drifted — the daemon maps RenameMode::From→Removed / To→Created and splits paired renames (fs_events.rs:453-454), while the platform impl maps every ModifyKind::Name change to Renamed (fs_watch/macos.rs:107). AGENTS §2/§8 require engine code to consume platform traits, not OS-event APIs, and any FSEvents/notify fix must currently be made twice. Note: the divergent rename classification does not currently cause sync asymmetry, because feed.rs::normalize_watch_event ignores the event kind and re-derives Created/Removed by stat-ing the path — the drift is a latent, not active, correctness risk.

**Suggested fix**: Make core/daemon consume vapor_platform::fs_watch (extending WatchEventKind with the From/To rename distinction the daemon needs), delete the duplicate notify plumbing from fs_events.rs, and keep exactly one OS-event→kind mapping with shared tests.

### [medium] IPC config writes are unsynchronized read-modify-write with a shared fixed temp filename, so concurrent IPC calls lose updates

**Category**: bug · **Where**: `core/daemon/src/ipc_service.rs:142`

write_config_key does read vapor.json -> mutate one key -> write to the fixed temp path `vapor.vapor-tmp` -> rename, with no lock. The IPC server serves up to 32 concurrent connections, each on its own thread, so two clients (e.g., the macOS app calling set_auto_launch while the CLI calls update_excludes — which itself performs two sequential read-modify-write cycles) can interleave: both read the same base document, both write the SAME temp file, and the last rename wins with a document that silently dropped the other caller's key. The losing caller still received accepted=true. Additionally the second writer's rename can fail with NotFound after the first rename consumed the shared temp file, returning a spurious error for a write that was actually clobbered.

**Suggested fix**: Guard write_config_key with a process-wide Mutex (or route config mutations through RuntimeControl to the single tick thread), and use a unique temp filename per write (e.g., include the op/thread id) before the atomic rename.

### [medium] Retry-slowdown upload clamp is applied before the CPU-ceiling scale factor, which multiplies it back up during rate-limit storms

**Category**: bug · **Where**: `core/daemon/src/lib.rs:419`

effective_throttle_caps() applies the rate-limit slowdown first (`caps.upload_concurrency = caps.upload_concurrency.min(1)`) and THEN multiplies all caps by `ceiling / DEFAULT_CPU_PERCENT (15)`. Idle boost is enabled by default (constants::idle_boost::DEFAULT_ENABLED = true) and publishes a 50% CPU ceiling when active, giving scale 50/15 ≈ 3.33. Concrete failure: overnight bulk sync, machine idle -> boost Active (ceiling 50) -> Google Drive starts returning 429s -> apply_retry_decision sets retry_slowdown_until -> intended upload concurrency 1 -> scaling turns it into round(1×3.33) = 3 concurrent uploads for the whole slowdown window (7 with a user-configured cpuPercent=100). This defeats the throttle/retry discipline the slowdown exists for, prolongs the provider rate-limit storm, and fights the auto-tuner, which is simultaneously halving the transfer step because `rate_limited` is true.

**Suggested fix**: Apply the slowdown clamp AFTER the ceiling scaling (move the `min(1)` below the `scale_cap` block), or treat slowdown like the Suspended zero-cap case that scaling explicitly refuses to relax.

### [medium] CPU-ceiling scaling can raise Throttled/Light caps far above the throttle ladder, contradicting the documented 'ceilings only ever lower' contract

**Category**: bug · **Where**: `core/daemon/src/lib.rs:421`

resource_budget.rs's module contract (lines 20-21) states 'Ceilings are hard caps: they only ever *lower* what the throttle controller already allows.' But effective_throttle_caps() multiplies every cap by `ceiling / 15`, and ResourceBudget publishes the base ceiling (`resourceLimits.cpuPercent`) in EVERY throttle state, not just IdleDrain. Concrete failure: user sets resourceLimits.cpuPercent = 60 (valid, 1..=100) -> scale 4.0 -> while the user is actively typing (user_active -> Throttled, caps planner/hash/upload = 1) the workgate is reconfigured to 4 planners, 4 hash workers, 4 read tokens, and 4 concurrent uploads; with cpuPercent=100 it is 7 of each. The Throttled tier — whose entire purpose is minimal device impact while the user works — silently runs at above-IdleDrain-default concurrency. Idle boost raising IdleDrain caps is intentional, but the multiplicative scaling being unconditional across states means a raised base budget breaks the throttle ladder in the states where the ladder matters most.

**Suggested fix**: Only allow the scale factor to exceed 1.0 while the throttle state is IdleDrain (i.e., where boost is defined), or clamp scaled caps at the tier's compiled values for Light/Throttled: `scale_cap(cap).min(cap)` outside IdleDrain. Update the resource_budget doc if raising non-idle caps is actually intended.

### [medium] Reconcile walk performs up to 8 synchronous provider enumerations per chunk with no intra-chunk slice/throttle check, blocking the daemon tick loop

**Category**: perf · **Where**: `core/daemon/src/reconcile_walk.rs:142`

ReconcileWalker::process loops max_directories (RECONCILE_DIRS_PER_CHECKPOINT = 8) times, and each compare_directory issues a blocking provider.enumerate network call (line 219) — for Google Drive that is one or more paged HTTPS list requests per directory. The controller's slice budget (RECONCILE_SLICE_MILLIS = 500ms) and throttle-state check only run at the checkpoint BETWEEN chunks (reconcile.rs::checkpoint), never inside the chunk. With a slow or degraded provider (1-5s per listing, or paging on large directories) one chunk can hold the runtime tick thread for tens of seconds: throttle transitions to Throttled/Suspended when the user becomes active cannot interrupt the in-flight chunk, and since the walk runs on the tick loop, status publishing and all other intent processing for the runtime stall too. This violates the 'reconcile scans are deferred and interruptible' invariant on exactly the provider where reconcile is most expensive.

**Suggested fix**: Check elapsed slice time (and ideally the current throttle state) between each directory inside process() — e.g., pass a deadline/should-yield callback from the controller — or lower the per-chunk directory budget to 1 for network providers so the 500ms slice discipline applies to real network latency.

### [medium] Push-only strict mirror never restores a remotely-deleted directory because local_file_exists rejects directories

**Category**: bug · **Where**: `core/daemon/src/remote_sync.rs:222`

In push-only mode a Removed change only schedules a restore Upload when local_file_exists(&local_target) is true, and that helper requires metadata.is_file(). Google Drive emits a single Removed change for a trashed folder (no per-descendant entries), so when someone deletes a mirrored folder cloud-side, the local target is a directory, local_file_exists returns false, nothing is enqueued, and the strict-mirror contract is silently broken until a cursor expiry or daemon restart triggers a whole-scope reconcile.

**Suggested fix**: When the Removed target maps to a local directory, walk it and enqueue Upload intents for each contained file (or enqueue a ReconcileSubtree intent for that directory), so push-only restores the whole tree.

### [low] Auto-tuner regression check compares absolute queue depth, which is confounded by ingest — step increases are always rolled back exactly when a deep queue needs them

**Category**: perf · **Where**: `core/daemon/src/auto_tune.rs:105`

The auto-tuner's regression check (auto_tune.rs:105) compares absolute durable queue depth before/after a step increase, but depth is dominated by exogenous ingest. During a sustained large ingest where enqueue outpaces drain, every +25% increase is followed by a deeper queue and is rolled back, degenerating into a 3-cycle oscillation (increase/rollback/cooldown-hold) that keeps the step near base for most of the ingest — the exact scenario the growth path targets. Symmetrically, during net drain an increase always sticks even if it hurt. The signal should measure drain rate/throughput (or depth delta net of ingest), not absolute depth.

**Suggested fix**: Judge regressions on drain rate instead of depth: track completed intents (or bytes transferred) per cycle, or compare depth delta against the ingest counter, and roll back only when throughput fell after the increase.

### [low] Download-side conflict resolution enqueues a redundant Download for a payload that is already fully staged  ✅ DONE

**Category**: perf · **Where**: `core/daemon/src/executor.rs:1274`

When a completed download detects local divergence, preserve_diverged_local_before_apply → resolve_upload_conflict enqueues both an Upload for the conflict copy and a Download for intent.path, but the caller then applies the already-staged payload and records the sync index. Because the executing intent is leased (not pending), the follow-up Download is not coalesced away, and the Download route has no convergence short-circuit, so the identical remote content is re-downloaded in full and byte-identically re-applied. Every download-side keep-both conflict transfers the file twice; the redundant intent can also race a subsequent local edit into a spurious (but safe, keep-both) extra conflict.

**Suggested fix**: Split the follow-up enqueue out of resolve_upload_conflict (parameterize it): the upload-gate/precondition callers need both follow-ups, but the download-apply caller only needs the conflict-copy Upload since it applies the canonical payload itself.

### [low] record_download_index stores the daemon's own download op-id instead of the remote object's op-id, defeating the op-id fast path for every subsequent upload

**Category**: improvement · **Where**: `core/daemon/src/executor.rs:1436`

After a download, sync_index.last_op_id is set to plan.op_id — the op-id this device allocated for the download intent — but the remote object's tag is the op-id of whichever device wrote it. The two can never match, so every later upload of a downloaded path falls out of the 'remote unchanged since last sync' op-id branch in plan_upload and into the hash-comparison branch, which calls app.provider().content_hash() when the stat entry carries no hash (for providers where that is a full remote read). Correctness converges via the hash, but the op-id correlator — the primary loop-prevention/change-detection mechanism — is permanently dead for the download direction.

**Suggested fix**: When recording the post-download index entry, store the remote change's op-id (available from the feed change / a post-download stat) as last_op_id instead of the local download intent's op-id, so the op-id equality fast path works in both directions.

### [low] Type-mismatch re-materialization intents do not share the clearing intent's path, so the claimed executor ordering does not hold for remote directories

**Category**: bug · **Where**: `core/daemon/src/reconcile_walk.rs:272`

The comment asserts 'the clearing intent and the re-materializing intents share a path, so the executor's per-path serialization orders them safely'. That is true for the file/file arms, but in the pull-only local-file-vs-remote-directory arm the walk enqueues ApplyRemoteDelete(local_path) and pushes local_path into pending_dirs; the children later enqueue Download(local_path/child) — different paths, so per-path serialization imposes no order between the parent delete and the child downloads. Concrete failure: the delete's first attempt fails transiently (e.g., stat EACCES in apply_remote_delete_locally -> Transient retry with backoff) while child Download intents proceed; each staging write under a parent that is still a regular file fails, burning provider download calls and retry cycles until the delete retry finally lands. It converges, but through avoidable multi-round retry churn on exactly the provider-billed path, and the walk also re-reads local_path as a directory before the delete applies (read_dir NotADirectory warning path).

**Suggested fix**: Either enqueue the child downloads only after the clearing intent completes (defer descending into the mismatched directory to the next reconcile pass), or give the executor an explicit parent-path dependency for re-materializing intents; at minimum fix the comment so future changes don't rely on ordering that does not exist.

### [low] A single 1-second headroom blip cancels Active idle boost into a full down-ramp plus a fresh 30s up-ramp; RampingDown never re-checks the gates

**Category**: improvement · **Where**: `core/daemon/src/resource_budget.rs:257`

gates_pass() gates the Active idle-boost state on the instantaneous 1s non-Vapor CPU sample; a single sample over the ~30% headroom (which can occur while throttle stays IdleDrain, since the Light threshold is 35%) flips Active to RampingDown, and the RampingDown arm never re-checks gates, so the budget rides the full 10s down-ramp to base and must start a fresh 30s up-ramp — ~40s of reduced ceilings per 1s blip. On machines with periodic short background spikes this would make boost oscillate instead of holding Active. Note: production impact is currently latent because NativePlatformMetricsSampler still forwards static inputs (real per-OS sampling lands later).

**Suggested fix**: Require the headroom gate to fail for N consecutive samples (small debounce) before leaving Active, and/or let RampingDown reverse into RampingUp from the current ceiling when gates pass again mid-ramp.

### [low] Rolling-window prune keeps future-dated events after a wall-clock rewind, so MassChangeGuard can spuriously pause sync and ActiveCodingHeuristic can pin Throttled — contrary to its documented fail-safe claim

**Category**: bug · **Where**: `core/daemon/src/safeguards.rs:73`

The field doc claims 'a rewound clock only shrinks the observed rate (fails safe: less triggering, never spurious triggering)', but prune() only removes entries with `*front < cutoff` where cutoff = now - window. After a backwards wall-clock step (NTP correction), previously recorded SystemTime stamps are in the FUTURE relative to `now`, are never < cutoff, and therefore stay in the window until the wall clock catches back up past them. Concrete failure for MassChangeGuard (threshold 200 / 60s): user deletes 150 files at wall time T, NTP steps the clock back 30 minutes, user deletes 50 more files over the next few minutes — count(now) sees all 200 'inside' the 60s window and trips the ransomware guard, pausing the daemon until a manual `vapor resume`. For ActiveCodingHeuristic the same mechanism keeps `user_active` forced true (Throttled, upload concurrency 1) for up to the full rewind duration. The rewind test in this file only passes because it uses 2 events against a threshold of 3, not because pruning occurs.

**Suggested fix**: In prune() (or record()), also drop or clamp entries with timestamps greater than `now` (e.g. `while front > now { pop }` or clamp on push), and fix the misleading field comment.


### [low] Orphaned `.vapor-tmp-dl-*` staging files accumulate in the user's sync folder after crashes or failed applies

**Category**: improvement · **Where**: `core/daemon/src/executor.rs:1074` (staging-name generation; line pre-cleanup)

Download applies stage into `.vapor-tmp-*` files inside the local sync root, and the ingore/path filter hides that prefix from sync in both directions. There is no startup or periodic sweep that removes stale staging files (`TEMP_FILE_PREFIX` is only used to generate and filter names — verified by grep across `core/`), so any crash or failed apply between staging and rename leaves an invisible orphan in the user's folder forever.

**Suggested fix**: sweep `TEMP_FILE_PREFIX`-prefixed files older than a threshold at daemon startup (per profile root), or track staging paths durably and remove them on recovery.

---

## 2. Providers, IPC, platform, lifecycle

Reviewed: provider trait core (paths, op-id tags, bandwidth shaper, HTTP helper),
filesystem reference provider + changes feed, Google Drive provider, OAuth/PKCE flow
(security focus), `core/ipc` (UDS transport, framing, protocol, server, client),
`core/platform` (fs-watch natives + fakes, fs-caps, secrets, service, idle, metrics,
process), `core/lifecycle` (crash-loop guard, manager, durable store, auto-launch).

**37 findings confirmed**, 11 candidates refuted and discarded during verification.

| Sev | Category | Location | Finding |
|---|---|---|---|
| high | bug | `core/cli/src/commands/auth.rs:224` | Authorization code is never percent-decoded, so real Google logins fail with invalid_grant |
| high | bug | `core/providers/src/filesystem/mod.rs:451` | delete() falls back to remove_dir_all, destroying an entire remote directory tree the engine never saw |
| high | bug | `core/providers/src/gdrive/mod.rs:415` | Duplicate file names in a Drive folder are silently resolved to an arbitrary match |
| high | bug | `core/providers/src/gdrive/mod.rs:440` | Stale path->id cache hit validates only existence, so uploads write into a file that was renamed/moved away |
| high | bug | `core/providers/src/gdrive/mod.rs:544` | path_for_changed_file prefers the stale cached path, so remote renames/moves are reported at the old path and never converge |
| high | bug | `core/providers/src/gdrive/mod.rs:659` | Google-native files (Docs/Sheets/shortcuts) are enumerated as 0-byte regular files and 'download' instantly as empty local files |
| high | bug | `core/providers/src/gdrive/mod.rs:706` | Upload precondition is check-then-act across the whole (potentially long) transfer, allowing silent last-write-wins overwrite |
| high | bug | `core/providers/src/http.rs:55` | NativeHttpTransport has no read/overall timeout: a stalled connection hangs the daemon forever |
| medium | bug | `core/cli/src/commands/auth.rs:208` | Loopback listener accepts exactly one connection; browser preconnect/speculative sockets or any stray probe kill or hang the login |
| medium | perf | `core/daemon/src/ipc_server.rs:111` | Accept-loop error path spins hot with no backoff (e.g. EMFILE) — sustained 100% CPU on a low-impact-first daemon |
| medium | bug | `core/daemon/src/ipc_server.rs:126` | Server connections have no write timeout: a client that stops reading blocks a handler thread forever and permanently exhausts the 32-connection cap |
| medium | bug | `core/ipc/src/server.rs:184` | Unknown Method variants from a newer (in-window) peer are answered with Backend parse errors instead of the documented MethodNotFound |
| medium | security | `core/ipc/src/transport.rs:120` | Client performs no peer verification on the socket; the deterministic shared-temp relocation path enables daemon impersonation on multi-user hosts |
| medium | bug | `core/providers/src/filesystem/mod.rs:609` | Upload precondition check and rename are not atomic; a concurrent writer between the check and the rename is silently overwritten |
| medium | perf | `core/providers/src/filesystem/mod.rs:671` | HashEquals precondition hashes the entire existing target inside a budgeted step() call, breaking bounded-checkpoint interruptibility |
| medium | perf | `core/providers/src/gdrive/mod.rs:852` | Changes feed is Drive-wide and each out-of-scope change triggers an uncached N+1 parent-chain walk |
| medium | perf | `core/providers/src/gdrive/mod.rs:959` | ProviderHandle builds a fresh TokenManager per HTTP call: a SecretStore/Keychain read per transfer chunk and no 401 refresh-retry |
| medium | bug | `core/providers/src/gdrive/mod.rs:1019` | Fixed multipart boundary makes files containing the boundary bytes permanently unsyncable or corrupted |
| medium | bug | `core/providers/src/gdrive/mod.rs:1161` | Resumable upload ignores the 308 Range response header — partial chunk persistence corrupts the offset math |
| medium | bug | `core/providers/src/gdrive/mod.rs:1246` | Download session trusts status 200 + transport's silent 64 MiB body cap, allowing a truncated file to complete 'successfully' |
| medium | bug | `core/providers/src/gdrive/mod.rs:1295` | 403 dailyLimitExceeded quota errors classified as Permanent, dropping sync intents instead of backing off |
| medium | security | `core/providers/src/gdrive/oauth.rs:43` | PKCE code verifier is generated from std's hash RandomState, not a CSPRNG |
| medium | security | `core/providers/src/gdrive/oauth.rs:60` | No OAuth state parameter and no request validation on the loopback redirect endpoint |
| medium | bug | `core/providers/src/http.rs:90` | HTTP responses larger than 64 MiB are silently truncated instead of erroring |
| medium | bug | `core/providers/src/paths.rs:59` | Backslash normalization and multi-segment join() silently remap legal filenames containing separators to nested paths |
| medium | bug | `core/providers/src/tags.rs:45` | Op-id side-file namespace collides with real user files: silent overwrite and silent exclusion from sync |
| low | improvement | `core/ipc/src/client.rs:31` | Client deadline bounds each syscall, not the whole call — a byte-trickling daemon keeps the CLI alive nearly unboundedly |
| low | bug | `core/ipc/src/client.rs:114` | Client connect() is not covered by the deadline — the timeout is applied only after the blocking connect returns |
| low | improvement | `core/ipc/src/server.rs:150` | Malformed JSON in the handshake frame closes the connection silently instead of returning a typed error |
| low | improvement | `core/ipc/src/server.rs:171` | HelloAck server_id reports the IPC schema version where its own contract documents the product version |
| low | perf | `core/providers/src/bandwidth.rs:63` | Bandwidth grant is consumed even when the transfer step uses fewer bytes or fails, systematically undershooting the configured rate |
| low | bug | `core/providers/src/filesystem/feed.rs:240` | Watch events are silently dropped when stat fails with anything other than NotFound, permanently losing the change from the feed |
| low | improvement | `core/providers/src/filesystem/mod.rs:306` | Non-UTF-8 remote file names are silently invisible to enumerate, stat-by-feed, and the changes feed — files never sync with no diagnostic |
| low | improvement | `core/providers/src/filesystem/mod.rs:370` | Orphaned upload temp files from crashes are never cleaned up and are permanently invisible |
| low | improvement | `core/providers/src/gdrive/mod.rs:932` | poll_changes falls back to re-using the same cursor when Drive returns neither nextPageToken nor newStartPageToken |
| low | improvement | `core/providers/src/http.rs:12` | HttpRequest derives Debug/Clone with raw Authorization headers, making bearer-token leaks one {:?} away |
| low | bug | `core/providers/src/tags.rs:131` | Side-file temp `.vapor-meta.json.tmp` is not recognized as internal and leaks into the sync scope on crash |

### [high] Authorization code is never percent-decoded, so real Google logins fail with invalid_grant

**Category**: bug · **Where**: `core/cli/src/commands/auth.rs:224` · **Review group**: gdrive-oauth

run_gdrive_pkce_flow() extracts the code from the raw redirect request line and uses it verbatim. Google authorization codes contain '/' (format '4/0A...'), which arrives percent-encoded in the redirect query ('4%2F0A...') per RFC 6749 appendix B. The undecoded value is then passed to exchange_code(), whose url_encode() re-encodes '%' as '%25', so the token endpoint receives a double-encoded code ('4%252F0A...'), decodes it once to '4%2F0A...', and rejects the exchange with invalid_grant. Concrete failure: every real `vapor auth login gdrive` run completes the consent hop, then fails at the exchange with 'authorization is no longer valid (invalid_grant)... run vapor auth login gdrive' — an unbreakable retry-login loop. The offline unit tests never catch this because their scripted codes ('code') contain no reserved characters.

**Suggested fix**: Percent-decode the extracted code (and any other query values) before passing it to exchange_code. Add a test that feeds a redirect line containing 'code=4%2F0Axyz' and asserts the exchange form body carries 'code=4%2F0Axyz' exactly once-encoded.

### [high] delete() falls back to remove_dir_all, destroying an entire remote directory tree the engine never saw

**Category**: bug · **Where**: `core/providers/src/filesystem/mod.rs:451` · **Review group**: provider-fs

FilesystemProvider::delete (core/providers/src/filesystem/mod.rs:451) falls back to fs::remove_dir_all when remove_file fails and the resolved path is a directory. Delete intents are planned with RemotePrecondition::None and no remote stat/kind check (executor.rs:1059, :584), so if the remote side replaces the target file with a directory — or adds never-polled children to a directory being delete-mirrored — before the intent executes, the entire tree is recursively and permanently destroyed, including content the engine never observed. This violates the two-way keep-both/never-silent-overwrite guarantee; gdrive is unaffected (it trashes), but filesystem is the pre-GA default provider. Fix: use fs::remove_dir (fails on non-empty, converges via retry once the changes feed surfaces new content) or return precondition_failed on file-vs-directory kind mismatch so the engine re-plans.

**Suggested fix**: Use `fs::remove_dir` (fails on non-empty) for the directory fallback, or return `precondition_failed` on a file-vs-directory kind mismatch so the engine re-plans against fresh remote state instead of recursively deleting.

### [high] Duplicate file names in a Drive folder are silently resolved to an arbitrary match

**Category**: bug · **Where**: `core/providers/src/gdrive/mod.rs:415` · **Review group**: gdrive

Google Drive allows multiple children with the same name in one folder. find_child (core/providers/src/gdrive/mod.rs:404-416) queries with pageSize=2 (suggesting duplicate detection was intended) but takes files.into_iter().next() with no orderBy and no duplicate check, so path resolution binds to an arbitrary duplicate and can flip across calls and daemon restarts (the id cache is in-memory only). enumerate (mod.rs:638-672) emits two RemoteEntry values with the identical path and its cache_mapping calls leave id_by_path pointing at whichever duplicate was listed last; reconcile_walk then collapses the pair last-wins, permanently shadowing one duplicate. Consequences: hash comparisons flap whenever the binding flips, so in two-way mode the engine manufactures spurious keep-both conflict copies indefinitely and sync never converges; delete trashes an arbitrary duplicate (the other resurrects the path); rename/download act on an arbitrary duplicate. In two-way mode a silent overwrite of the divergent duplicate is largely prevented by the HashEquals precondition and the verify-before-upload gate (divergence routes to keep-both), but in push-only mirror mode the upload PATCHes an arbitrary duplicate with no precondition, and the shadowed duplicate is never mirrored away, so the mirror also never converges. Fix: detect files.len() > 1 in find_child and produce a deterministic outcome (stable-key pick used consistently everywhere, or a Permanent 'ambiguous remote name' error surfaced as a conflict), and de-duplicate same-name children deterministically in enumerate instead of emitting colliding paths.

**Suggested fix**: In find_child, detect files.len() > 1 and surface a deterministic outcome (e.g., pick by stable key such as smallest id — consistently everywhere — or return a Permanent 'ambiguous remote name' error the engine can surface as a conflict). In enumerate, de-duplicate same-name children deterministically instead of emitting colliding paths.

### [high] Stale path->id cache hit validates only existence, so uploads write into a file that was renamed/moved away

**Category**: bug · **Where**: `core/providers/src/gdrive/mod.rs:440` · **Review group**: gdrive

On a path->id cache hit, GoogleDriveProvider::resolve() (core/providers/src/gdrive/mod.rs:429-451) fetches the file by id and accepts it if it merely exists and is not trashed, never verifying that its name/parents still match the requested path. After another Drive client renames or moves the file, the stale mapping persists for the daemon's lifetime: evict_path only removes exact keys (never a moved folder's descendants), and poll_changes cannot self-heal because path_for_changed_file returns the cached OLD path for the changed id. Consequences: begin_upload PATCHes local edits onto the file id now known as b.txt or moved outside the configured sync root (a sync-scope violation), a.txt is never recreated remotely; stat/content_hash/begin_download return or fetch the wrong file's data. In two-way mode the executor's HashEquals/op-id guard (executor.rs plan_upload) blocks most overwrites of divergently edited remote content, but rename-only moves pass the guard (hash unchanged), and push-only mode uploads with no precondition, making the overwrite unconditional. Fix: on cache hit, validate the fetched file's name against the path's final segment and its parent against the resolved parent id, evicting and falling through to the segment walk on mismatch; also evict all cached entries under a directory prefix when a folder mapping is invalidated.

**Suggested fix**: On cache hit, verify the fetched GdFile still matches the cached path: compare file.name against the path's final segment and file.parents against the cached parent id (resolving the parent path the same way). On mismatch, evict and fall through to the segment walk. Also evict all cache entries under a directory prefix when a folder mapping is invalidated.

### [high] path_for_changed_file prefers the stale cached path, so remote renames/moves are reported at the old path and never converge

**Category**: bug · **Where**: `core/providers/src/gdrive/mod.rs:544` · **Review group**: gdrive

For a changed file id, path_for_changed_file (core/providers/src/gdrive/mod.rs:544) returns the path_by_id cache entry before consulting the change's actual name/parents, and the early return skips the cache_mapping refresh. When a remote user renames report.txt to final.txt (same id), changes.list delivers the new name but poll_changes emits CreatedOrModified at the stale path report.txt. The engine enqueues a Download for the old local path; the download resolves via the stale id mapping and rewrites report.txt with the renamed file's content (identical bytes for a pure rename, or final.txt's new content on rename+edit — content at the wrong path). final.txt never appears locally, report.txt never disappears, and the stale mapping is never corrected because the cache-refreshing walk never runs. Since reconcile only triggers on daemon startup, cursor expiry, or storm compaction (no periodic cadence), the rename is silently dropped for the remainder of a steady-state run.

**Suggested fix**: Do not trust the cached path for the changed file itself. Recompute the leaf from file.name + file.parents (the cached path of the parent id is fine as a short-circuit), compare with any cached path for the id, and when they differ emit Removed(old_path) + CreatedOrModified(new_path) and update both cache maps.

### [high] Google-native files (Docs/Sheets/shortcuts) are enumerated as 0-byte regular files and 'download' instantly as empty local files

**Category**: bug · **Where**: `core/providers/src/gdrive/mod.rs:659` · **Review group**: gdrive

poll_changes filters out folders (gdrive/mod.rs:909) but nothing anywhere filters Google-native MIME types (application/vnd.google-apps.* other than folder, including shortcuts): enumerate/stat map a Google Doc to RemoteEntryKind::File with size_bytes 0 (Drive omits `size`) and content_hash None (no md5). The engine enqueues Download intents for these entries (remote_sync.rs:212, reconcile_walk.rs:352) with no hash/MIME gate, begin_download computes total_bytes=0, and GdriveDownloadSession::step finishes on the first call without issuing a single request — materializing a genuine 0-byte local file with a valid empty-content md5. Every Google Doc/Sheet/shortcut in the synced folder produces a phantom empty local file, plus a fresh no-op download on each remote edit. content_hash() already rejects these types ("Google-native document types cannot sync as files") but the download path never consults it. Note: the originally claimed "later push uploads the empty file back as a duplicate" does not occur in the normal path (local echo suppression + size 0 == size 0 in reconcile + in-place PATCH on existing file id); the real secondary hazard is that a user deleting the confusing phantom file locally propagates a Delete that trashes the actual Google Doc remotely (recoverable via Drive trash). Fix: filter application/vnd.google-apps.* (except folder) out of enumerate, stat, and poll_changes, or surface them as an explicit unsupported-entry kind so the engine never plans transfers for them.

**Suggested fix**: Filter application/vnd.google-apps.* (except folder) out of enumerate, stat, and poll_changes results — or surface them as an explicit unsupported-entry kind — so the engine never plans transfers for them. Shortcuts (vnd.google-apps.shortcut) need the same treatment.

### [high] Upload precondition is check-then-act across the whole (potentially long) transfer, allowing silent last-write-wins overwrite

**Category**: bug · **Where**: `core/providers/src/gdrive/mod.rs:706` · **Review group**: gdrive

RemotePrecondition::HashEquals/Absent are verified once in GoogleDriveProvider::begin_upload (core/providers/src/gdrive/mod.rs:695-715) and the precondition is not retained in GdriveUploadSession, so the multipart commit and every resumable chunk (including the final committing PUT) run with no server-side or client-side guard. The executor holds in-flight upload sessions indefinitely under Throttled/Suspended or bandwidth exhaustion (core/daemon/src/executor.rs:631-648) without re-validation, making the check-to-commit window for large files unbounded. If a collaborator edits the same Drive file during the transfer, the final chunk commits Vapor's content as the new head revision, the provider reports success, the engine's PreconditionFailed→keep-both path (executor.rs:681) never fires, and the echo cache suppresses the subsequent changes-feed entry as a self-write — a silent overwrite that violates the two-way keep-both invariant while capabilities() advertises supports_write_preconditions. The sibling filesystem provider checks the precondition at commit time inside finalize() (core/providers/src/filesystem/mod.rs:609-610), confirming gdrive deviates from the intended contract; the shared contract test only covers start-time checks, so CI does not catch it. (Note: the overwritten revision is recoverable manually via Drive revision history for ~30 days, but Vapor surfaces no conflict.) Fix: re-stat md5Checksum/modifiedTime immediately before the committing request and return PreconditionFailed on divergence, shrinking the window to one round-trip.

**Suggested fix**: Re-stat the target immediately before the committing request (final resumable chunk / the multipart call) and fail with PreconditionFailed if md5Checksum or modifiedTime moved since begin_upload; even better, for updates capture modifiedTime at begin_upload and compare. This shrinks the race window from the whole transfer to one round-trip, which the engine's deterministic re-plan can then handle.

### [high] NativeHttpTransport has no read/overall timeout: a stalled connection hangs the daemon forever

**Category**: bug · **Where**: `core/providers/src/http.rs:55` · **Review group**: provider-core

NativeHttpTransport::execute uses `ureq::request(...)` on the default agent. In ureq 2.12.1 the default agent sets timeout_connect=30s but timeout_read/timeout_write/timeout are all None (the ureq docs for the vendored crate literally say 'requests may block forever on reads by default'). The whole Provider trait is synchronous and stepped by the executor tick, so a single black-holed connection blocks the tick indefinitely. Concrete scenario: mid-download the device switches Wi-Fi networks (or a NAT drops the flow) after the TCP handshake; the read in `read_to_end` never returns and never errors. The executor thread is stuck inside `session.step()`, the retry machinery never fires (no error is ever surfaced), throttle transitions and shutdown requests are not honored (violating AGENTS.md §3 slice-interruptibility), and sync stalls permanently until the process is killed.

**Suggested fix**: Build a shared `ureq::Agent` via `AgentBuilder` with an explicit `timeout_read`/`timeout_write` (or an overall per-request `timeout`) sized to the chunk budget (e.g. 30-120s), and surface the timeout as a transient HttpTransportError so the existing retry/backoff machinery handles it.

### [medium] Loopback listener accepts exactly one connection; browser preconnect/speculative sockets or any stray probe kill or hang the login

**Category**: bug · **Where**: `core/cli/src/commands/auth.rs:208` · **Review group**: gdrive-oauth

The loopback OAuth flow in run_gdrive_pkce_flow (core/cli/src/commands/auth.rs:207) calls listener.accept() exactly once and reads one request line from whatever connection arrives first, with no read timeout, no accept loop, and no overall deadline. Any stray connection that beats the genuine redirect during the consent window (a local process probing the port, or a speculative browser socket) either hangs the CLI forever at "Waiting for the authorization redirect..." (peer sends no bytes; TcpStream has no default read timeout) or aborts the login with "the redirect did not carry an authorization code" (peer closes empty), after which the listener is dropped and the user's real redirect hits a closed port. The trigger is intermittent and race-dependent rather than routine (learned browser preconnect cannot fire for a fresh ephemeral 127.0.0.1 port, and favicon requests arrive after the real GET), and the failure is recoverable by rerunning `vapor auth login`. Fix: loop on accept() with a per-connection read timeout and an overall deadline, discarding connections that do not produce a parseable GET carrying a code.

**Suggested fix**: Loop on listener.accept() with a read timeout per connection, discard connections that produce no parseable GET with a code (or that fail state validation, see the state finding), and only stop once a valid code is received or an overall deadline expires.

### [medium] Accept-loop error path spins hot with no backoff (e.g. EMFILE) — sustained 100% CPU on a low-impact-first daemon

**Category**: perf · **Where**: `core/daemon/src/ipc_server.rs:111` · **Review group**: ipc

The IPC accept loop in core/daemon/src/ipc_server.rs (lines 110-113) treats every accept() error as an immediate silent retry: `for stream in listener_clone.incoming() { let Ok(stream) = stream else { continue; }; … }`. The listener is a blocking UnixListener (core/ipc/src/transport.rs; set_nonblocking is never called) and std's `Incoming` never terminates on error, so persistent errors like EMFILE/ENFILE — plausible since the daemon holds fds for fs-watchers, the SQLite state DB, provider connections, logs, and per-session IPC streams — cause accept() to fail instantly and be retried instantly. The `vapor-ipc` thread then busy-loops at ~100% of a core until fd pressure clears, with no log line to explain it, starving legitimate IPC clients and violating the product's primary low-device-impact invariant exactly when the daemon is already under resource pressure. Fix: on Err from accept, log once (rate-limited) and sleep with capped backoff (e.g. 10 ms → 1 s) before continuing; optionally surface a degraded-IPC status on repeated identical errors.

**Suggested fix**: On `Err` from accept, log once (rate-limited) and sleep with backoff (e.g. 10 ms → 1 s capped) before continuing; optionally treat repeated identical errors as a reason to surface a degraded-IPC status.

### [medium] Server connections have no write timeout: a client that stops reading blocks a handler thread forever and permanently exhausts the 32-connection cap

**Category**: bug · **Where**: `core/daemon/src/ipc_server.rs:126` · **Review group**: ipc

The daemon IPC accept loop (core/daemon/src/ipc_server.rs:126) sets only set_read_timeout on accepted streams; there is no set_write_timeout. serve_connection writes responses with blocking write_all (core/ipc/src/framing.rs:97-99), so a client that completes the handshake, keeps the socket open, and stops reading (e.g. a malicious same-user process pipelining requests, or a SIGSTOPped client mid-large Diagnostics/Timeline response) parks the handler thread in write_all indefinitely — the idle read timeout never fires because the thread is blocked in write. The active_connections counter is decremented only after serve_connection returns (ipc_server.rs:144), so each wedged connection consumes one of the 32 slots for as long as the peer holds the socket open; if 32 accumulate, the accept loop (lines 114-124) drops all new connections and daemon IPC (vapor status, pause/resume, macOS app shim) is unavailable until restart. Sync itself is unaffected, and a suspended client that resumes or is killed releases its slot via EPIPE, so permanent full exhaustion effectively requires a hostile same-user process. Fix: also call stream.set_write_timeout(Some(idle_timeout)) so a stalled write errors out and the slot is released.

**Suggested fix**: Set a write timeout on the accepted stream alongside the read timeout (e.g. `stream.set_write_timeout(Some(idle_timeout))`), so a blocked `write_all` returns `WouldBlock`, `serve_connection` errors out, and the slot is released. Optionally add a total per-session deadline as defense in depth.

### [medium] Unknown Method variants from a newer (in-window) peer are answered with Backend parse errors instead of the documented MethodNotFound

**Category**: bug · **Where**: `core/ipc/src/server.rs:184` · **Review group**: ipc

The IPC protocol doc (core/ipc/src/protocol.rs:53-55) promises that unknown method names are rejected with ErrorBody::MethodNotFound, and the handshake skew window (check_skew, core/ipc/src/server.rs:254-281) explicitly admits a peer one schema version ahead. But Method is a closed serde enum, so when an in-window newer client sends a method variant the daemon doesn't know, serde_json::from_slice::<Request>() fails on the whole envelope and the per-method loop (server.rs:181-188) replies ErrorBody::Backend("invalid request: unknown variant …"). MethodNotFound is never constructed anywhere in the repo, making the documented error taxonomy unreachable dead code, and the client cannot distinguish "daemon too old for this command" from a genuine daemon-side fault. Latent today (schema current = 2; no newer methods exist yet), but it will surface the first time a v3 CLI talks to a v2 daemon. Fix direction: on parse failure in the per-method loop, do a lenient decode (serde_json::Value, check kind == "Call", extract the method tag) and answer MethodNotFound(name) for unknown-variant errors, reserving Backend for genuinely malformed frames.

**Suggested fix**: In the per-method loop, on parse failure re-attempt a lenient decode (e.g. parse to `serde_json::Value`, check `kind == "Call"`, extract the method name/tag) and answer `ErrorBody::MethodNotFound(name)` for unknown variants, reserving `Backend`/parse errors for genuinely malformed frames.

### [medium] Client performs no peer verification on the socket; the deterministic shared-temp relocation path enables daemon impersonation on multi-user hosts

**Category**: security · **Where**: `core/ipc/src/transport.rs:120` · **Review group**: ipc

`connect_to_socket` (and `Client::connect` above it) connects to whatever socket exists at the resolved path with zero verification of who owns it — no `stat` ownership check on the socket file and no `SO_PEERCRED`/`getpeereid` check after connect. The server side is protected by the 0o700 parent dir + 0o600 socket mode, but the client side is not symmetric. Concrete failure: when `<vapor_dir>/vapord.sock` exceeds `MAX_SOCKET_PATH_BYTES` (100), `runtime_paths::ipc_socket_location` deterministically relocates the socket to `env::temp_dir()/vapor-<fnv1a64(vapor_dir)>/vapord.sock`. The hash input (the victim's vapor_dir) is guessable, and on Linux (planned surface; this code is the portable transport) `env::temp_dir()` is world-writable `/tmp`. Another local user pre-creates `/tmp/vapor-<hash>/vapord.sock` and listens: the victim's daemon fails to bind (chmod on the attacker-owned dir returns EPERM), but the victim's CLI/app happily connects to the attacker's socket, completes the handshake (nothing in `HelloAck` is authenticated), and now receives forged status/acks — the attacker can fake 'Running/synced' state while sync is actually dead, and receives `UpdateExcludes` rule contents. macOS is currently shielded only because `TMPDIR` is per-user.

**Suggested fix**: Before/after connecting, verify the peer: check `fs::metadata(socket_path).uid() == geteuid()` on the socket file, and/or verify the connected peer's uid via `getpeereid` (macOS) / `SO_PEERCRED` (Linux) before sending `Hello`. Fail with a typed 'foreign socket' error.

### [medium] Upload precondition check and rename are not atomic; a concurrent writer between the check and the rename is silently overwritten

**Category**: bug · **Where**: `core/providers/src/filesystem/mod.rs:609` · **Review group**: provider-fs

FilesystemUploadSession::finalize() (core/providers/src/filesystem/mod.rs:609) runs check_precondition() — a full streaming SHA-256 of the current target for HashEquals, or a bare exists() for Absent — and only afterwards fsyncs the temp payload, writes the op-id tag, and calls fs::rename(temp, target) (line 633). There is no lock, RENAME_NOREPLACE, or re-check, so a concurrent writer (second daemon or any process sharing the cloud directory, e.g. a network mount — a deployment the README explicitly advertises) that lands content on target between the check and the rename is silently overwritten last-write-wins. Because the renamed file carries the uploader's op-id tag, the change feed suppresses it as a self-write, making the clobbered content unrecoverable — exactly the outcome the precondition (C8-17 deterministic race resolution / keep-both policy) exists to prevent. The window is widened by the full-file hash of the target and the sync_all of the entire uploaded payload. Suggested fix: narrow the race with an exclusive advisory lock or renameat2(RENAME_NOREPLACE)/link-based create for Absent, re-check the hash immediately before rename, and document the residual TOCTOU as a known limitation of the reference provider.

**Suggested fix**: Narrow the race with an exclusive advisory lock (flock) or a link/renameat2(RENAME_NOREPLACE)-style create for `Absent`, and at minimum re-order to hash immediately before rename; document the residual race as a known limitation of the reference provider if full atomicity is out of scope.

### [medium] HashEquals precondition hashes the entire existing target inside a budgeted step() call, breaking bounded-checkpoint interruptibility

**Category**: perf · **Where**: `core/providers/src/filesystem/mod.rs:671` · **Review group**: provider-fs

In the filesystem provider, `FilesystemUploadSession::step()` invokes `finalize()` at source EOF, and for `RemotePrecondition::HashEquals` `check_precondition()` calls `hash_hex_of_file(&self.target)` (core/providers/src/filesystem/mod.rs:671) — a full, unbudgeted read + SHA-256 of the entire previous remote copy inside a single budgeted `step()` call. This breaks the TransferSession bounded-checkpoint contract (core/providers/src/lib.rs module doc; AGENTS §3), which the executor otherwise enforces (budgeted hash stage, slice-budget stepping, suspended-hold-at-checkpoint test). The HashEquals precondition is set on the common two-way overwrite path (core/daemon/src/executor.rs:1185/1204), so overwriting a multi-GB file via the filesystem provider stalls the tick loop for the full hash of the old file, ignoring throttle transitions and shutdown. Google Drive is unaffected (it checks HashEquals against md5 metadata at begin_upload). Fix by hashing the target in budgeted chunks across step() calls, or by recording cheap (size, mtime) evidence at begin_upload with full hashing moved to a chunked path.

**Suggested fix**: Verify the precondition incrementally (hash the target in budgeted chunks across step() calls before/while streaming the payload) or record the target's (size, mtime) at begin_upload and re-check cheaply at finalize, reserving full hashing for a chunked path.

### [medium] Changes feed is Drive-wide and each out-of-scope change triggers an uncached N+1 parent-chain walk

**Category**: perf · **Where**: `core/providers/src/gdrive/mod.rs:852` · **Review group**: gdrive

The changes.list request (core/providers/src/gdrive/mod.rs:852) sets no restrictToMyDrive/spaces filtering, so the feed contains every change in the user's entire Drive (including shared-with-me items), not just the configured sync root. For every non-removed, non-folder change whose file id is not in path_by_id, path_for_changed_file issues up to 64 sequential files.get calls walking the parent chain; when the file is outside the root the negative result is not cached, and intermediate parents visited during the walk are never cached either (only the changed file's own path is, and only on success). A busy file outside the Vapor root (e.g., a doc a colleague edits every minute in the user's My Drive) re-triggers a full parent-chain walk on each 5–15s poll cycle, burning battery/network and Drive API quota in violation of the low-impact invariant and risking rate-limit storms. Fix: TTL-based negative cache for out-of-scope file ids, cache every parent id visited during a walk, and/or maintain a folder-id set for the sync subtree so most out-of-scope changes are rejected with zero requests.

**Suggested fix**: Cache negative results (file-id -> out-of-scope, with TTL) and cache every parent id visited during the walk (currently only the changed file's path is cached, not intermediate parents outside the mapping). Consider maintaining a folder-id set for the sync subtree so most out-of-scope changes are rejected with zero requests.

### [medium] ProviderHandle builds a fresh TokenManager per HTTP call: a SecretStore/Keychain read per transfer chunk and no 401 refresh-retry

**Category**: perf · **Where**: `core/providers/src/gdrive/mod.rs:959` · **Review group**: gdrive

ProviderHandle::execute_authed (core/providers/src/gdrive/mod.rs:959) constructs a new TokenManager with an empty cache on every call, so every resumable-upload chunk PUT and every download range GET performs a SecretStore get() plus a token-JSON parse on the hot transfer path. Today NativeSecretStore is still the in-memory placeholder (Keychain bridge deferred to Wave 5/C4-5), so the current cost is small, but once the real Keychain bridge lands this becomes one securityd IPC round-trip per chunk (~1,280 for a 10 GB upload at 8 MiB chunks), conflicting with the device-impact invariant. More importantly, ProviderHandle drops the one-shot 401 invalidate+refresh retry that GoogleDriveProvider::execute_authed has: if the stored expiry is inaccurate and a chunk PUT returns 401, classify_api_failure yields an Authentication error, the executor aborts the session and finalize_failure marks the intent terminally failed — the multi-gigabyte upload is not merely restarted, the sync intent fails outright instead of refreshing and resending one chunk. Fix: share an Arc<TokenManager> between the provider and its ProviderHandle (TokenManager already has interior mutability and Arc'd secrets/transport) and mirror the 401 forced-refresh retry.

**Suggested fix**: Give ProviderHandle a persistent Arc<TokenManager> shared with the provider (TokenManager already has interior mutability and owns Arc'd secrets/transport), and mirror the one-shot 401 invalidate+refresh retry there.

### [medium] Fixed multipart boundary makes files containing the boundary bytes permanently unsyncable or corrupted

**Category**: bug · **Where**: `core/providers/src/gdrive/mod.rs:1019` · **Review group**: gdrive

simple_multipart (core/providers/src/gdrive/mod.rs:1019) uses the fixed boundary "vapor-multipart-boundary" and embeds raw file bytes unencoded with no collision check. Any file <= 5 MiB (SIMPLE_UPLOAD_MAX_BYTES) whose content contains a full multipart delimiter line ("\r\n--vapor-multipart-boundary" followed by CRLF/whitespace or "--" — e.g., an HTTP capture of Vapor's own upload traffic or a fixture of a built request body) breaks the multipart framing. Drive then either rejects with 400 (classified Permanent by classify_api_failure's catch-all, so the executor finalizes the intent as a terminal failure and drops it) or stores content truncated at the fake boundary with an md5 that never matches local. The failure is deterministic per file content, so the affected file can never sync. Fix: generate a per-request random boundary and defensively regenerate if it occurs in the payload.

**Suggested fix**: Generate a random per-request boundary (long random hex string) and, defensively, verify it does not occur in the payload before use (regenerate if it does).

### [medium] Resumable upload ignores the 308 Range response header — partial chunk persistence corrupts the offset math

**Category**: bug · **Where**: `core/providers/src/gdrive/mod.rs:1161` · **Review group**: gdrive

On a 308 the resumable upload session unconditionally does sent_bytes += chunk_len (core/providers/src/gdrive/mod.rs:1161) and ignores the response 'Range: bytes=0-N' header, which the Drive resumable protocol defines as the authoritative committed offset. If the server ever acknowledges fewer bytes than sent on a successful 308 (permitted by the protocol; defended against by all official Google clients), the next PUT's Content-Range has a gap, Drive rejects it with a 4xx, classify_api_failure maps that to Permanent, and the intent fails terminally. Impact is bounded: Vapor aborts the session on any error and retries with a brand-new session from byte 0, and reconcile eventually re-plans failed uploads, so the outcome is a wasted full re-upload and delayed convergence rather than data loss. Note the specific narrated trigger (truncated chunk body followed by a 308) cannot occur — an incomplete PUT yields a transport error, not a 308 — and chunks are 256 KiB-aligned, so the partial-commit window is narrow. Fix: parse the Range header on every 308 and set sent_bytes = last_acked_byte + 1 (0 if absent), re-seeking when behind the local counter; optionally add a 'bytes */total' status probe before abandoning a session on transient chunk failure.

**Suggested fix**: Parse the Range header from every 308 response and set sent_bytes = last_acked_byte + 1 (0 if absent). If it is behind the local counter, re-seek and resend from the acknowledged offset instead of failing. Consider also issuing a 'Content-Range: bytes */total' status probe when a chunk PUT fails transiently, instead of abandoning the session URL.

### [medium] Download session trusts status 200 + transport's silent 64 MiB body cap, allowing a truncated file to complete 'successfully'

**Category**: bug · **Where**: `core/providers/src/gdrive/mod.rs:1246` · **Review group**: gdrive

NativeHttpTransport (core/providers/src/http.rs:90) silently caps every response body at 64 MiB via .take() with no truncation signal, and GdriveDownloadSession::step (core/providers/src/gdrive/mod.rs:1246) finishes unconditionally on status 200 without verifying received_bytes == total_bytes (finish() also never checks the md5 against Drive's declared checksum). If a ranged alt=media GET gets a 200 full-body response (Range ignored due to transcoding or an intermediary) for a file larger than 64 MiB, the transport clamps the body to 64 MiB, the session reports Completed with bytes_total = 64 MiB and an md5 of the truncated bytes, and the executor (core/daemon/src/executor.rs:807-858) — which performs no cross-check against remote metadata — renames the truncated staging file into the user's sync root and records it in the sync index as fully synced. Result: silent local file corruption presented as a successful sync. (The 206 path self-heals via re-request from received_bytes; only the 200 path is unguarded.) Fix: on status 200 verify received_bytes == total_bytes (when known) and return a Transient error on mismatch; separately make the transport error when the 64 MiB cap is hit instead of silently clamping.

**Suggested fix**: On status 200, verify received_bytes == total_bytes (when total is known) before finishing, and return a Transient error on mismatch. Separately, make the transport signal truncation (error when the 64 MiB cap is hit) instead of silently clamping.

### [medium] 403 dailyLimitExceeded quota errors classified as Permanent, dropping sync intents instead of backing off

**Category**: bug · **Where**: `core/providers/src/gdrive/mod.rs:1295` · **Review group**: gdrive

classify_api_failure (core/providers/src/gdrive/mod.rs:1295) detects 403 quota/rate-limit errors by case-sensitive substring match on "ateLimitExceeded" / "quotaExceeded". This catches rateLimitExceeded, userRateLimitExceeded, sharingRateLimitExceeded, and quotaExceeded, but misses two documented Drive 403 reasons: dailyLimitExceeded and storageQuotaExceeded (capital Q). Both fall through to ProviderError::permanent, which the engine maps to RetryFailureKind::Permanent and finalizes the intent terminally (executor.rs finalize_failure) instead of scheduling a backoff retry. Affected files stop syncing until a reconcile scan re-plans them (no data loss, but stalled convergence and misleading "permanent failure" logs), and the upload session skips its adaptive chunk-shrink for these errors. Fix: parse the structured error reason (errors[0].reason) from the 403 JSON body; map dailyLimitExceeded and the *RateLimitExceeded family to RateLimited, and give storageQuotaExceeded a distinct surfaced/retry-eventually treatment rather than generic Permanent. Add classification tests for both reasons.

**Suggested fix**: Classify 403 bodies by the structured error reason (parse errors[0].reason from the JSON body) instead of substring sniffing; map dailyLimitExceeded / userRateLimitExceeded / rateLimitExceeded / sharingRateLimitExceeded to RateLimited, and keep storageQuotaExceeded as a distinct actionable (but retry-eventually) state rather than generic Permanent.

### [medium] PKCE code verifier is generated from std's hash RandomState, not a CSPRNG

**Category**: security · **Where**: `core/providers/src/gdrive/oauth.rs:43` · **Review group**: gdrive-oauth

generate_code_verifier() (core/providers/src/gdrive/oauth.rs:39-49) builds the RFC 7636 PKCE verifier from four std RandomState/SipHash-1-3 words instead of a CSPRNG. RFC 7636 §4.1 requires a cryptographically random verifier; std explicitly disclaims crypto guarantees for this hasher and may change it silently. All four 64-bit words derive from one lazily-seeded 128-bit per-thread value with a counter-incremented key, so the construction rests on unclaimed related-key PRF properties of SipHash-1-3. Because the authorization URL carries no state parameter and the loopback listener validates nothing, the verifier is the sole defense against authorization-code interception for full-Drive tokens. Mitigating factor: the per-thread seed comes from the OS CSPRNG and no practical attack on SipHash-1-3 is known, so this is not exploitable today — it is a standards-compliance and robustness defect, not an active hole. Fix: generate 32 octets via the getrandom crate and encode with the existing base64_url_no_pad() (43-char verifier), and update verifier_shape_satisfies_pkce_requirements (line 234-240) to assert the unreserved alphabet instead of hex.

**Suggested fix**: Generate 32 octets from a CSPRNG (getrandom crate, or /dev/urandom via getrandom::getrandom) and encode with the existing base64_url_no_pad(), yielding a 43-char verifier per the RFC. Update verifier_shape_satisfies_pkce_requirements to assert the unreserved alphabet instead of hex.

### [medium] No OAuth state parameter and no request validation on the loopback redirect endpoint

**Category**: security · **Where**: `core/providers/src/gdrive/oauth.rs:60` · **Review group**: gdrive-oauth

authorization_url() (core/providers/src/gdrive/oauth.rs:58) emits no state parameter, and run_gdrive_pkce_flow (core/cli/src/commands/auth.rs:170) validates nothing on the loopback redirect: it does a single listener.accept(), extracts any 'code=' pair from the first request line, replies 'Vapor is authorized', and exchanges the code. While the user is mid-consent, any local process or a drive-by web page fetching http://127.0.0.1:<port>/?code=x consumes the one-shot accept; the bogus exchange fails with invalid_grant and the login deterministically aborts, losing the genuine redirect. Even benign stray connections (browser speculative sockets, favicon requests) can break the flow. PKCE prevents token theft or account fixation, so impact is limited to login denial and fragility, but this deviates from RFC 8252 §8.9 / OAuth Security BCP. Fix: generate a CSPRNG state alongside the verifier, add it to authorization_url as a parameter, and loop on accept, ignoring requests whose state does not match.

**Suggested fix**: Generate a CSPRNG state value alongside the verifier, append '&state=...' in authorization_url (make it a parameter), and have the listener ignore any request whose state does not match instead of consuming it.

### [medium] HTTP responses larger than 64 MiB are silently truncated instead of erroring

**Category**: bug · **Where**: `core/providers/src/http.rs:90` · **Review group**: provider-core

NativeHttpTransport (core/providers/src/http.rs:90) caps body reads with .take(64 MiB) and returns Ok with the truncated body — no error, no marker, no Content-Length verification. GdriveDownloadSession::step (core/providers/src/gdrive/mod.rs:1246) treats any status-200 response as payload-complete via `status == 200 || received_bytes >= total_bytes`, so if Drive ignores the Range header and replies 200 with the full body of a >64 MiB file, the session receives exactly 64 MiB, calls finish() with received_bytes < total_bytes, and reports Completed with a self-consistent MD5 of the truncated bytes. The daemon executor (core/daemon/src/executor.rs:807-857) trusts the outcome with no remote-hash comparison: it commits the truncated file, records the truncated hash/size in the download index, and marks the intent complete — silent local corruption with no retry. Note the ranged 206 path self-heals (received_bytes advances by actual body length and the next Range resumes), so only the 200 branch corrupts; and paginated Drive JSON responses realistically never reach 64 MiB, so the JSON-truncation retry-loop claim is theoretical. Fix: read limit+1 bytes and return an HttpTransportError when the body exceeds the cap, and/or have the download session require received_bytes == total_bytes (or verify against remote md5Checksum) before reporting completion.

**Suggested fix**: Read up to limit+1 bytes and return an HttpTransportError (or a distinct permanent-classifiable error) when the body exceeds the cap, or honor Content-Length and verify received length before returning Ok.

### [medium] Backslash normalization and multi-segment join() silently remap legal filenames containing separators to nested paths

**Category**: bug · **Where**: `core/providers/src/paths.rs:59` · **Review group**: provider-core

RemotePath::new (core/providers/src/paths.rs:59) unconditionally rewrites '\' to '/' in the whole string, and join()/from_local accept name "segments" containing separators, so legal filenames containing separators are silently remapped into nested paths. On macOS (the shipping OS) a local file named `foo\bar.txt` is uploaded via executor.rs plan_intent → gdrive ensure_parent_id as Drive folder `foo` containing `bar.txt`; the reverse mapping (resolve_under/to_local) then targets nested local `foo/bar.txt`, a different file than the original, causing permanent duplication and re-upload churn on every edit. Symmetrically, a Google Drive file legally named `reports/2024.txt` is turned into a nested path by directory.join(&file.name) (gdrive/mod.rs:660) and the parent-walk segments.join("/"), and can no longer be resolved by gdrive resolve() which splits on '/'. No traversal escape exists (dot segments and leading separators are rejected); the defect is silent structural remapping. Fix: translate '\' only at the Windows-native-path boundary, make join() reject segments containing '/' or '\', and refuse or escape remote names containing separators.

**Suggested fix**: Only translate '\\' to '/' where it is genuinely a separator (i.e. in Windows-origin native paths, at the from_local/OS boundary), not in RemotePath::new; make join() reject segments containing '/' or '\\'; percent-escape or refuse remote names containing the separator until an escaping scheme exists.

### [medium] Op-id side-file namespace collides with real user files: silent overwrite and silent exclusion from sync

**Category**: bug · **Where**: `core/providers/src/tags.rs:45` · **Review group**: provider-core

side_file_path derives `{path}.vapor-meta.json` inside the user's sync root with no collision handling, and the suffix is hidden everywhere (path_filter.rs:181/207, reconcile_walk.rs:183, providers/filesystem mod.rs:309, feed.rs:214). Three concrete failures: (1) on filesystems where the xattr write fails (Unsupported/PermissionDenied/ReadOnlyFilesystem — FAT/exFAT/network/read-only mounts), the executor tags a downloaded file (executor.rs:1548) and write_side_file (tags.rs:125-134) unconditionally rename-overwrites an existing user file literally named e.g. `notes.vapor-meta.json`, destroying it with no conflict copy — violating the keep-both/never-silent-overwrite policy; (2) OpIdTagStore::remove (tags.rs:107) unconditionally deletes the side-file path and runs on the remote-delete apply path (executor.rs:1515) on every filesystem, so a same-named user file is deleted even on xattr-capable APFS (relocate_side_file has the same clobber on rename); (3) any legitimate user file whose name ends in `.vapor-meta.json` is silently excluded from sync in both directions forever — the exclusion is enforced before user ignore rules and surfaced by no diagnostic — and consequently the file destroyed in (1)/(2) is unrecoverable from the cloud. Preconditions are an unlikely naming collision, hence medium.

**Suggested fix**: Before writing a side-file, stat the target: if a file exists that does not parse as a Vapor SideFilePayload, refuse the fallback (or divert to a shadow directory under vapor_dir keyed by path hash, which also fixes the exclusion problem). At minimum, log a warning and surface excluded suffix-named user files in doctor/status output.

### [low] Client deadline bounds each syscall, not the whole call — a byte-trickling daemon keeps the CLI alive nearly unboundedly

**Category**: improvement · **Where**: `core/ipc/src/client.rs:31` · **Review group**: ipc

DEFAULT_CALL_TIMEOUT (core/ipc/src/client.rs:31) is applied as SO_RCVTIMEO/SO_SNDTIMEO on the UnixStream, so it bounds each syscall and re-arms on every successful partial read. read_frame (core/ipc/src/framing.rs) reads the length prefix in a loop and the payload via read_exact, so a peer delivering ≥1 byte per 3 s window never times out; with the 4 MiB MAX_PAYLOAD_BYTES cap a single call can theoretically run for ~4.2M × 3 s. The shipping CLI (core/cli/src/commands/ipc.rs:121) and therefore the macOS app shim use this default path, and the only timeout test covers a fully silent peer, so the L3-7 "never hang" contract holds only against silence, not a slow/pathological daemon. Not exploitable across users (0600 socket) and requires a pathologically wedged daemon, so impact is minor. Fix: track an absolute per-call deadline and re-apply the shrinking remainder before each read/write, erroring once the total budget is spent.

**Suggested fix**: Track an absolute deadline per call: recompute and re-apply the remaining timeout before each read (or read via poll with a shrinking deadline), erroring out once the total budget is spent.

### [low] Client connect() is not covered by the deadline — the timeout is applied only after the blocking connect returns

**Category**: bug · **Where**: `core/ipc/src/client.rs:114` · **Review group**: ipc

`connect_with_timeout` (core/ipc/src/client.rs:114) applies the read/write deadline only after `connect_to_socket` — a plain blocking `UnixStream::connect` (core/ipc/src/transport.rs:121) — has returned, so the connect phase is unbounded. If the daemon's `vapor-ipc` accept thread dies while `IpcServerHandle` keeps the listener fd open (core/daemon/src/ipc_server.rs), clients keep landing in the kernel accept backlog (std backlog = 128); each such connection is still bounded by the 3 s handshake read timeout, but once the backlog fills, subsequent `connect()` calls on Linux block indefinitely per AF_UNIX blocking-connect semantics, and the CLI hangs forever — violating the L3-7 "never hang" guarantee documented at client.rs:20-31. macOS fails fast with ECONNREFUSED on a full backlog, masking the bug on the current shipping OS; Linux is a planned but not-yet-shipping surface. Fix: bound the connect phase with the same deadline (nonblocking connect + poll, or connect on a helper thread joined with the timeout).

**Suggested fix**: Perform a non-blocking connect with a poll/select deadline (or connect on a helper thread joined with the timeout) so the connect phase is bounded by the same deadline as reads/writes.

### [low] Malformed JSON in the handshake frame closes the connection silently instead of returning a typed error

**Category**: improvement · **Where**: `core/ipc/src/server.rs:150` · **Review group**: ipc

In serve_connection (core/ipc/src/server.rs:149-150), a first frame that passes framing but fails Request JSON parsing returns ServeError::Parse without writing anything to the peer — and the daemon's connection handler (core/daemon/src/ipc_server.rs:143) discards that error unlogged, so the failure is silent on both ends. All sibling error paths send a typed courtesy reply first (HandshakeRequired for non-Hello first frames, Backend for post-handshake parse errors, PayloadTooLarge for oversized frames per read_next_frame's stated design intent). A skewed or buggy client whose Hello serialization differs sees only FrameError::UnexpectedEof (client.rs:134), indistinguishable from a crashed daemon, making protocol/version mismatches hard to diagnose. Suggest a best-effort typed error response (e.g., HandshakeRequired or a dedicated parse-error body) before returning Parse, matching the existing courtesy-reply pattern; optionally also log the ServeError at the daemon call site.

**Suggested fix**: Before returning `ServeError::Parse` on the handshake frame, best-effort send `Response::Err(ErrorBody::HandshakeRequired(..))` (or a dedicated parse-error body), matching the courtesy reply the other error paths already give.

### [low] HelloAck server_id reports the IPC schema version where its own contract documents the product version

**Category**: improvement · **Where**: `core/ipc/src/server.rs:171` · **Review group**: ipc

HelloAck.server_id (core/ipc/src/server.rs:171) is built as format!("vapord/{current}") where current is SCHEMA_VERSION_CURRENT (2), so every handshake advertises the constant "vapord/2" — redundant with the adjacent schema_version field and inconsistent with the repo's identity-string convention (StatusResponse.daemon_id is documented and implemented as "vapord/<product version>" from build_info::VERSION; Hello.client_id likewise uses the product version). Note: server_id itself has no doc comment mandating a format (the "vapord/<product version>" doc at protocol.rs:222-225 belongs to StatusResponse.daemon_id), and no current consumer reads server_id (the IPC client discards the HelloAck body), so the impact is a latent diagnostics inconsistency rather than an active bug. Suggested fix: populate server_id with the product version (VERSION-synced CARGO_PKG_VERSION or a caller-supplied identity, since core/ipc cannot depend on vapor_daemon::build_info), keeping the schema version in the dedicated schema_version field.

**Suggested fix**: Build `server_id` from the product version (root `VERSION` via the existing build-info plumbing), e.g. `vapord/<product-version>`, keeping the schema version in the dedicated `schema_version` field.

### [low] Bandwidth grant is consumed even when the transfer step uses fewer bytes or fails, systematically undershooting the configured rate

**Category**: perf · **Where**: `core/providers/src/bandwidth.rs:63` · **Review group**: provider-core

BandwidthShaper::budget() (core/providers/src/bandwidth.rs:63) debits the full granted token amount, but the grant is only an upper bound for session.step(): GdriveUploadSession::chunk_size aligns the budget down to 256 KiB multiples (and clamps to the adaptive chunk_hint, which starts at 256 KiB), the final chunk is shortened to the remaining bytes, and a step returning Err transfers zero bytes after the executor (executor.rs:639/789 via grant_transfer_budget at :1585) already consumed the grant. No refund API exists and the executor discards Progressed.bytes_transferred. With the default 8 MiB step request and rate = 1 MB/s, each ~1,000,000-byte grant sends only 786,432 bytes (~21% waste per step; up to 75% before the chunk hint grows), so sustained upload throughput under a configured bandwidth ceiling runs materially below the configured rate, and failed steps burn up to a full second of shared budget, delaying concurrent sessions. Downloads request exactly the granted range, so the effect is upload-dominated; sub-256 KiB grants actually overshoot (chunk_size floors at CHUNK_GRANULARITY), a separate mild shaping violation in the opposite direction. Fix: add a refund/settle method clamped to the 1-second bucket cap and call it from the executor with granted minus actual bytes transferred (full refund on Err).

**Suggested fix**: Add a `refund(unused: u64)` (or `settle(granted, used)`) method that returns unspent tokens to the bucket (clamped to the 1-second cap), and call it from the executor after each step with granted minus actual bytes_transferred.

### [low] Watch events are silently dropped when stat fails with anything other than NotFound, permanently losing the change from the feed

**Category**: bug · **Where**: `core/providers/src/filesystem/feed.rs:240` · **Review group**: provider-fs

`normalize_watch_event` (core/providers/src/filesystem/feed.rs:240) returns `None` on any `symlink_metadata` error other than NotFound (e.g. transient EACCES during a permission change, or EIO on a network-mounted remote root). The event has already been consumed from the watch channel in `drain_watch_events`, so it never enters the feed ring and the feed will never deliver that change; there is no log line explaining the drop. Because reconcile runs only at startup, config reload, cursor expiry, or manual request (not periodically), the divergence can persist on a long-running daemon until the file changes again, the ring overflows (forcing CursorExpired → reconcile), or a restart/manual reconcile occurs. Suggested fix: on non-NotFound stat errors, emit the change optimistically as CreatedOrModified with `op_id: None` (the engine re-checks anyway), or at minimum log the drop so missed-change divergence is diagnosable.

**Suggested fix**: On non-NotFound stat errors, emit the change optimistically as CreatedOrModified with `op_id: None` (the engine re-stats and hash-checks anyway), or at minimum log the drop so missed-change reports are diagnosable.

### [low] Non-UTF-8 remote file names are silently invisible to enumerate, stat-by-feed, and the changes feed — files never sync with no diagnostic

**Category**: improvement · **Where**: `core/providers/src/filesystem/mod.rs:306` · **Review group**: provider-fs

enumerate (core/providers/src/filesystem/mod.rs:306) skips directory entries whose names are not valid UTF-8 with a bare `continue`, and normalize_watch_event (feed.rs:213) drops watch events for such paths via `?` — both silently. Because RemotePath is a UTF-8 String newtype, such files are also unaddressable by stat. A file with a non-UTF-8 name in the cloud directory is therefore permanently excluded from sync with no log line, counter, or doctor diagnostic; the tree appears converged while the file is missing. Note this is part of a deliberate system-wide UTF-8-path design (state_db UTF-8 storage, identical silent skips on the local side in reconcile_walk.rs:180 and fs_events.rs:164), and macOS/APFS — the only shipping surface — enforces UTF-8 file names at creation, so the exposure today is minimal. The actionable improvement is observability only: log a warning (lossy path bytes) the first time a non-representable name is skipped and surface a skip count through diagnostics/doctor, on both the provider and local walk sides.

**Suggested fix**: Log a warning (redacting nothing sensitive — path bytes lossily) the first time a non-representable name is skipped, and surface a count through diagnostics/doctor so the exclusion is observable rather than silent.

### [low] Orphaned upload temp files from crashes are never cleaned up and are permanently invisible

**Category**: improvement · **Where**: `core/providers/src/filesystem/mod.rs:370` · **Review group**: provider-fs

A daemon crash (SIGKILL, panic-abort, power loss) between `begin_upload` and finalize/abort leaves `.vapor-tmp-<opid>` files in the cloud directory, since only Drop/abort/finalize remove them and the upload session lives across ticks for the duration of the throttle-gated transfer. `is_internal_file_name` hides these orphans from enumeration, the changes feed, and the reconcile walk, and no startup or reconcile sweep removes them, so repeated unclean crashes accumulate hidden partial payloads in the user's cloud folder indefinitely. Post-crash retries never reclaim them: `allocate_op_id` embeds attempt_count and a millisecond timestamp, so every replay writes a new temp file. Suggested fix: sweep stale `TEMP_FILE_PREFIX` files older than a conservative age during `ensure_cloud_sync_directory` or the reconcile walk.

**Suggested fix**: Sweep stale `TEMP_FILE_PREFIX` files (older than some conservative age) during `ensure_cloud_sync_directory` or the reconcile walk.

### [low] poll_changes falls back to re-using the same cursor when Drive returns neither nextPageToken nor newStartPageToken

**Category**: improvement · **Where**: `core/providers/src/gdrive/mod.rs:932` · **Review group**: gdrive

next_cursor falls back to cursor.to_string() when both tokens are absent. If Drive (or a proxy returning a well-formed but empty JSON object with status 200) ever omits both, the engine durably persists the same cursor and re-polls the identical page forever — with changes non-empty this re-emits the same RemoteChanges every poll cycle, and with changes empty it spins without ever advancing the baseline. A malformed-but-parsable response should not be silently accepted as progress.

**Suggested fix**: Treat the absence of both tokens as a protocol violation: return ProviderError::transient (retry) instead of echoing the input cursor, so the engine backs off rather than looping on identical state.

### [low] HttpRequest derives Debug/Clone with raw Authorization headers, making bearer-token leaks one {:?} away

**Category**: improvement · **Where**: `core/providers/src/http.rs:12` · **Review group**: provider-core

HttpRequest (core/providers/src/http.rs:12) derives Debug while carrying raw "Authorization: Bearer <access token>" headers (gdrive/mod.rs:318, :967) and OAuth secrets in bodies (gdrive/oauth.rs:138), so any future `{request:?}` in an error path or panic message would emit the live token. No current code Debug-prints an HttpRequest, and the two main sinks are already guarded (logger inline redaction via `redact_inline_secrets` catches "bearer "; persisted queue errors pass through `sanitize_persisted_error`), so the exposure today is limited to panic payloads on stderr and future unguarded format paths. Replacing the derived Debug with a manual impl that redacts Authorization/Proxy-Authorization values (and elides the body) is cheap defense-in-depth consistent with AGENTS.md §6.

**Suggested fix**: Replace the derived Debug with a manual impl that redacts values of Authorization/Proxy-Authorization (and any header name containing 'token'/'secret'), keeping method/url/header-names visible for diagnostics.

### [low] Side-file temp `.vapor-meta.json.tmp` is not recognized as internal and leaks into the sync scope on crash

**Category**: bug · **Where**: `core/providers/src/tags.rs:131` · **Review group**: provider-core

write_side_file (core/providers/src/tags.rs:131) stages the op-id side-file through `{name}.vapor-meta.json.tmp`, a name outside Vapor's reserved internal namespace: it matches neither TEMP_FILE_PREFIX (`.vapor-tmp-`) in filesystem::is_internal_file_name nor INTERNAL_IGNORE_FILE_PREFIXES/SUFFIXES in the path filter's unconditional internal-artifact check. If the daemon crashes between fs::write and fs::rename (side-files are only written when xattr writes fail, e.g. on network/FAT mounts), the temp file is permanently orphaned in a user-visible sync directory with no cleanup path. Under the default configuration it is NOT synced in either direction — the editable default pre-ignore rule `*.tmp` is applied to local ingest, the remote changes poll, and both sides of the reconcile walk — but this relies on the user-editable rule set, violating the documented invariant that internal artifacts are enforced in the filter itself: a user who removes `*.tmp` from preIgnoreRules/VAPOR_PRE_IGNORE_RULES or re-includes it via `!*.tmp` in .gitignore/.vaporignore/post rules would sync the orphaned internal metadata file. Fix: stage the temp inside the reserved namespace (e.g. TEMP_FILE_PREFIX-prefixed name) or extend is_internal_file_name/INTERNAL_IGNORE_FILE_SUFFIXES to cover the staging name.

**Suggested fix**: Name the temp file inside the reserved namespace, e.g. prefix it with TEMP_FILE_PREFIX (`.vapor-tmp-{name}.vapor-meta.json`) or extend is_internal_file_name/INTERNAL_IGNORE_FILE_SUFFIXES to cover the `.tmp` staging name.


### Addendum — additional verified findings (batch 2)

| Sev | Category | Location | Finding |
|---|---|---|---|
| high | bug | `core/lifecycle/src/manager.rs:236` | bootstrap/install launches the daemon via RunAtLoad before the crash-loop guard is consulted, bypassing pause and backoff |
| medium | improvement | `core/lifecycle/src/auto_launch.rs:137` | Autolaunch write rewrites the whole vapor.json from a stale read, losing concurrent edits by other commands |
| medium | perf | `core/platform/src/idle.rs:88` | NativeIdleNotifier reports the user as always idle on macOS, defeating idle-gated throttling on the shipping OS |
| medium | perf | `core/platform/src/metrics.rs:77` | NativePlatformMetricsSampler returns constant fabricated ThrottleInputs on macOS, so battery/thermal/CPU pressure never throttles the daemon |
| low | improvement | `core/platform/src/service/macos.rs:230` | stop_daemon swallows all launchctl failures and returns success before the daemon has exited |

### [high] bootstrap/install launches the daemon via RunAtLoad before the crash-loop guard is consulted, bypassing pause and backoff

**Category**: bug · **Where**: `core/lifecycle/src/manager.rs:236`

bootstrap_if_needed (core/lifecycle/src/manager.rs:236) invokes installer.install_and_enable() before checking the crash-loop guard; on macOS this bootstraps a RunAtLoad=true LaunchAgent, so launchd starts the daemon immediately — bypassing a durable crash-loop pause/backoff on every app launch or `vapor service bootstrap` while the manager reports RelaunchDeferred and the UI shows "paused". Additionally, the bootout+bootstrap (plus the subsequent kickstart -k) kills and restarts a healthy running daemon twice on every app launch. The in-memory fake models install_and_enable as leaving the service Stopped, so the test suite cannot catch this native-behavior divergence.

**Suggested fix**: Consult the guard before install: in bootstrap_if_needed and set_auto_launch_enabled(true), return RelaunchDeferred without calling install_and_enable() when the guard is paused or a backoff is pending; alternatively make install_and_enable not auto-start (RunAtLoad=false or bootstrap without launch) and rely solely on start_daemon() for launching.

### [medium] Autolaunch write rewrites the whole vapor.json from a stale read, losing concurrent edits by other commands

**Category**: improvement · **Where**: `core/lifecycle/src/auto_launch.rs:137`

JsonFileAutoLaunchSettingStore::write does read-document → insert autoLaunch → rewrite entire file, with no cross-process lock and a fixed temp name (vapor.vapor-tmp). Concrete scenario: the macOS app toggles autolaunch (spawning `vapor service install`) while the user runs a config-writing command (the doc itself notes `vapor config` uses the same serializer) — whichever writer renames last re-emits its stale snapshot of every other top-level key, silently reverting the other command's change to vapor.json. Note also that auto_launch_enabled() (manager.rs lines 219-226) triggers this write path from read-only commands like `vapor service status` on first observation, widening the race window to commands users consider side-effect-free.

**Suggested fix**: Share the same advisory file-lock discipline proposed for lifecycle.json for all vapor.json writers, and use a unique temp filename per writer so concurrent renames cannot fail with ENOENT or publish another writer's payload.

### [medium] NativeIdleNotifier reports the user as always idle on macOS, defeating idle-gated throttling on the shipping OS

**Category**: perf · **Where**: `core/platform/src/idle.rs:88`

NativeIdleNotifier (core/platform/src/idle.rs:88) is a stub that always reports 24h of idleness on every OS including shipping macOS; since it is wired unconditionally into production runtimes (multi_runtime.rs:185) and idle-boost is enabled by default, the min_idle gate in resource_budget.rs is permanently satisfied and the daemon runs at boosted ceilings (50% CPU, 80% bandwidth) regardless of real user activity — the "verifiably idle" precondition is never verified. The stub is documented as Wave 4 follow-up work, but its tracking tasks (C3-6, C8-55 in docs/tasks/core.md) are both marked complete while deferring the native CGEvent bridge to each other, leaving the gap untracked.

**Suggested fix**: Implement the macOS bridge via CGEventSourceSecondsSinceLastEventType(kCGEventSourceStateHIDSystemState, kCGAnyInputEventType) (or IOHIDSystem HIDIdleTime); until it lands, make NativeIdleNotifier on macOS return Duration::ZERO (user treated as Active → work deferred) so the stub fails safe for device impact.

### [medium] NativePlatformMetricsSampler returns constant fabricated ThrottleInputs on macOS, so battery/thermal/CPU pressure never throttles the daemon

**Category**: perf · **Where**: `core/platform/src/metrics.rs:77`

NativePlatformMetricsSampler::sample() (core/platform/src/metrics.rs:77) is a stub that returns a fixed ThrottleInputs::default() snapshot; since bootstrap.rs:116 injects it into the production daemon on macOS, throttle decisions never reflect real battery/thermal/CPU/disk state (violating the "defer under pressure" / low-device-impact invariant on the shipping OS), and the hard-coded Some(10_000) kbps placeholder is consumed by the bandwidth shaper as if it were a real link measurement. This is a documented, tracked Wave 4 follow-up (docs/tasks/core.md C3-5), not an accidental bug, but it remains a real invariant gap until the native mach2/IOKit bridge lands.

**Suggested fix**: Land the macOS bridge (host_statistics64 for CPU, IOPSCopyPowerSourcesInfo for battery, OSThermalNotification/thermal pressure sysctl, statfs for disk). Until then, at minimum log a prominent startup warning that throttle inputs are static, and bias the static default toward the conservative side (e.g., on_battery=true) so the stub errs toward low impact rather than maximum impact.

### [low] stop_daemon swallows all launchctl failures and returns success before the daemon has exited

**Category**: improvement · **Where**: `core/platform/src/service/macos.rs:230`

stop_daemon() ignores the result of `launchctl kill TERM` entirely and returns Ok(()) immediately. Two problems: (a) the comment claims errors 'just mean the service was already stopped', but launchctl can also fail for unrelated reasons (wrong gui domain in a non-Aqua session, launchctl I/O errors), and those are reported as success; (b) SIGTERM is asynchronous — the call returns while vapord is still shutting down. Callers propagate this as truth: DaemonLifecycleManager::stop_daemon_for_termination (manager.rs:319-320), `vapor service stop --json`, and the Swift app's Quit flow all report the daemon stopped while it may still be running. The fake flips status to Stopped synchronously, so no test observes the race. Concrete failure: 'Quit Vapor' → CLI reports stopped → app terminates → vapord is still finishing work; a subsequent status/health check or reinstall races the dying instance.

**Suggested fix**: Distinguish 'service not found' (treat as already-stopped) from other launchctl failures (return Backend error), and optionally poll status() briefly until the pid disappears (bounded, e.g. a few hundred ms) so stop reports the actual terminal state. Mirror whatever contract is chosen in the fake.


### [low] `StoredTokens` derives `Debug` with plaintext token fields, bypassing the redaction the module relies on

**Category**: security · **Where**: `core/providers/src/gdrive/oauth.rs:23`

`StoredTokens` is `#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]` while holding access/refresh tokens in plain `String` fields; the doc comment above it claims the value never reaches logs via redaction markers, but any `{:?}` format of the struct (or of a type embedding it) prints both tokens verbatim, sidestepping the logging redaction layer entirely.

**Suggested fix**: hand-implement `Debug` to print `[REDACTED]` for token fields (or wrap tokens in a `SecretString`-style newtype with a redacting `Debug`).

---

## 3. CLI, shared, macOS app, scripts, CI, docs

Reviewed: `core/cli` (entrypoint + all commands), `core/shared` (config, constants,
device id, logging, runtime paths), the macOS app (VaporCore logic + app shell semantics
vs the documented component model), all `scripts/*` (shell correctness + security),
GitHub Actions workflows (correctness + supply chain), docs-vs-code consistency, and
repo hygiene (.env.example, .gitignore, locales, agent skills, Cargo metadata).

**55 findings confirmed** by adversarial re-verification.

| Sev | Category | Location | Finding |
|---|---|---|---|
| high | security | `.github/workflows/release.yml:112` | Third-party actions referenced by mutable tags inside the secret-bearing release job |
| high | bug | `apps/macos/Sources/Vapor/VaporApp.swift:21` | Main window auto-presents at launch, violating menubar-first startup (CLAUDE.md 2.1) |
| high | bug | `apps/macos/Sources/VaporCore/VaporConfiguration.swift:252` | App config save clobbers daemon-owned keys with a stale startup snapshot |
| high | bug | `core/cli/src/commands/auth.rs:216` | OAuth authorization code is never percent-decoded, then re-encoded on exchange — real Google logins fail with invalid_grant |
| medium | bug | `.github/workflows/release.yml:73` | Preflight `git fetch origin main --depth=1` can shallow-graft main and falsely reject a valid release tag |
| medium | bug | `.github/workflows/release.yml:96` | Release job uses GitHub Environment `release` but the contract and docs mandate `release-macos` |
| medium | security | `.github/workflows/release.yml:122` | Signing/release job restores shared incremental build cache into the artifacts it signs and notarizes |
| medium | bug | `.github/workflows/release.yml:334` | Re-running the release workflow demotes an already-published release back to draft (and never clears the prerelease flag) |
| medium | perf | `.github/workflows/test.yml:58` | Identical cargo cache key across lint/test/build/perf/release jobs means only one job's target/ ever gets saved |
| medium | perf | `apps/macos/Sources/Vapor/AppShellViewModel.swift:214` | Crash-loop acknowledge spawns two CLI subprocesses synchronously on the main thread |
| medium | bug | `apps/macos/Sources/Vapor/AppShellViewModel.swift:239` | Quit can leave the daemon running: race between queued auto-launch work on lifecycleQueue and the main-thread quit path |
| medium | bug | `apps/macos/Sources/Vapor/AppShellViewModel.swift:240` | Quit path blocks the main thread on unbounded CLI subprocess waits — Quit Vapor can hang forever |
| medium | perf | `apps/macos/Sources/VaporCore/DaemonLifecycle.swift:215` | Main thread blocks on vapor CLI subprocess behind a queue shared with the 30s health tick |
| medium | bug | `apps/macos/Sources/VaporCore/DaemonLifecycle.swift:276` | Login-item registration failures are swallowed: UI reports 'Start at login' ON while the app will not launch at login |
| medium | bug | `apps/macos/Sources/VaporCore/VaporCLIServiceController.swift:56` | ProcessVaporCLIRunner reads pipes only after waitUntilExit and has no timeout — latent permanent deadlock |
| medium | security | `core/cli/src/commands/auth.rs:207` | PKCE loopback listener accepts exactly one connection with no state parameter, no read timeout, and no request filtering |
| medium | bug | `core/cli/src/commands/config.rs:85` | vapor.json read-modify-write has no locking and shares a fixed temp filename with the lifecycle autolaunch store — concurrent writes are lost or fail |
| medium | bug | `core/cli/src/main.rs:470` | vapor auth login gdrive silently ignores the documented stdin-token path and blocks on a browser OAuth flow |
| medium | bug | `core/shared/src/config.rs:268` | One malformed field value silently discards the user's entire configuration (whole-file serde failure -> all defaults) |
| medium | improvement | `core/shared/src/config.rs:280` | Unknown/misspelled top-level config keys are silently dropped even though ALL_KEYS exists for validation |
| medium | bug | `core/shared/src/device_id.rs:47` | resolve_or_persist read-modify-write races with other config writers and can silently revert their settings |
| medium | security | `core/shared/src/device_id.rs:74` | resolve_or_persist writes vapor.json with default (world-readable) permissions and creates vapor_dir 0755, bypassing the PRIVATE_*_MODE policy |
| medium | improvement | `core/shared/src/logging.rs:175` | No log rotation or size cap: vapor.logs / vapord.logs grow without bound for a long-running daemon |
| medium | bug | `scripts/e2e.sh:402` | S9 runs rm -rf on a directory derived from an unvalidated log-parsed path |
| medium | perf | `scripts/hooks.sh:63` | Pre-commit hook runs clean.sh, forcing a full cold rebuild of the entire workspace on every commit |
| medium | bug | `scripts/version.sh:152` | sync_cargo_lock swallows all cargo output, so release prep dies silently after already mutating VERSION/Cargo.toml on main |
| low | improvement | `.github/workflows/release.yml:291` | Release output validation checks only 2 of the 3 mandated bundle executables — Contents/Helpers/vapor is unverified |
| low | improvement | `.github/workflows/test.yml:3` | No concurrency groups on PR-triggered workflows — superseded pushes keep burning macOS runners |
| low | bug | `apps/macos/Sources/Vapor/AppShellViewModel.swift:245` | toggleAutoLaunch computes the target value from stale UI state, so rapid double-toggle re-applies instead of reverting |
| low | improvement | `apps/macos/Sources/VaporCore/AppLifecycleCoordinator.swift:46` | Quit from menubar blocks the main thread on a subprocess with no time bound |
| low | bug | `apps/macos/Sources/VaporCore/DaemonLifecycle.swift:223` | Crash-loop state fails open: CLI errors report 'not paused', letting the UI clear a real pause |
| low | improvement | `apps/macos/Sources/VaporCore/StructuredLogger.swift:126` | StructuredLogger silently loses all logging after the log file is deleted |
| low | bug | `apps/macos/Sources/VaporCore/VaporConfiguration.swift:224` | First-launch default-config write can race the daemon and silently swallows errors |
| low | bug | `apps/macos/Sources/VaporCore/VaporPaths.swift:34` | Runtime-dir resolution diverges from Rust under XCTest: app and spawned CLI use different vapor_dirs |
| low | bug | `core/cli/src/commands/config.rs:128` | timelineEventLimit accepts negative and zero values that the daemon silently ignores |
| low | improvement | `core/cli/src/commands/conflicts.rs:96` | Conflict scan silently skips unreadable subdirectories, contradicting the 'never silently incomplete' contract |
| low | improvement | `core/cli/src/commands/conflicts.rs:198` | Keep-copy fallback deletes the canonical file on any rename error, not only the Windows exists-collision it was written for |
| low | perf | `core/cli/src/commands/ipc.rs:258` | vapor logs without --tail loads the entire log file into memory before printing |
| low | improvement | `core/cli/src/commands/run.rs:21` | --foreground is an accepted no-op flag whose presence implies a background default that does not exist |
| low | improvement | `core/cli/src/commands/service.rs:166` | vapor service stop unconditionally reports {"result":"stopped"} even when nothing was installed or running |
| low | improvement | `core/cli/src/commands/support.rs:65` | Support-bundle directory name collides silently: same-millisecond or pre-epoch timestamps merge two bundles into one directory |
| low | bug | `core/cli/src/main.rs:25` | vapor --version omits the git commit, diverging from vapor version, vapord --version, and the documented contract |
| low | improvement | `core/cli/src/main.rs:356` | Support bundle drops all live captures and reports daemonReachable=false if any one of three IPC calls fails |
| low | bug | `core/cli/src/main.rs:421` | vapor timeline help and empty-state message falsely claim the C8-30 timeline buffer has not shipped |
| low | bug | `core/cli/src/main.rs:453` | println!-based output panics on closed stdout (broken pipe) — vapor logs \\| head exits 101 with a panic message |
| low | bug | `core/cli/src/main.rs:537` | Explicit --token accepts empty/whitespace values that the stdin path rejects, storing a useless credential |
| low | improvement | `core/shared/src/config.rs:57` | Doc/code mismatch: config.rs claims resource-limit/idle-boost clamping happens at load and is surfaced via load_issue; the loader does neither |
| low | security | `core/shared/src/logging.rs:236` | Log redaction misses JSON-shaped secrets ("access_token": "...") — tokens can reach log files verbatim |
| low | bug | `core/shared/src/runtime_paths.rs:17` | Any CI env value (including CI=false or empty) silently redirects vapor_dir to cwd-relative ./.vapor, even when VAPOR_ENV=prod |
| low | improvement | `core/shared/src/runtime_paths.rs:283` | Lexical `..` normalization and lack of canonicalization mis-resolve symlinked VAPOR_DIR spellings |
| low | improvement | `scripts/e2e.sh:175` | cleanup uses pkill -f with the repo path as an unescaped regex |
| low | bug | `scripts/e2e.sh:348` | S5 hangs forever (no timeout) if the singleton lock regresses |
| low | bug | `scripts/e2e.sh:529` | e2e harness silently aborts and deletes the failure sandbox when a glob/grep assignment matches nothing |
| low | perf | `scripts/lint.sh:18` | lint.sh runs the identical Swift lint command twice per invocation |
| low | bug | `scripts/tests/version.sh:90` | version.sh fixture tests inherit the developer's global git config and fail on gpgsign/hooksPath machines |

### [high] Third-party actions referenced by mutable tags inside the secret-bearing release job

**Category**: security · **Where**: `.github/workflows/release.yml:112` · **Review group**: ci

The release job runs `maxim-lobanov/setup-xcode@v1.6.0` (personal-account action) and `actions-rust-lang/setup-rust-toolchain@v1.9.0` by mutable git tag, not commit SHA (same pattern in build.yml, lint.yml, test.yml, perf.yml for checkout/cache/setup actions). Tags can be force-moved. Failure scenario: the `v1.6.0` tag of setup-xcode is retargeted to malicious code (account compromise); on the next `v*` tag push it executes inside the release job, where it can poison PATH/$GITHUB_ENV so later steps leak `APPLE_DEVELOPER_ID_P12_BASE64`, `APPLE_DEVELOPER_ID_P12_PASSWORD`, and the notary API key — enabling the attacker to sign and notarize malware as this Developer ID. The isolated GitHub Environment does not help because the action runs inside the environment-scoped job.

**Suggested fix**: Pin every third-party (and ideally first-party) action to a full commit SHA with a version comment, e.g. `maxim-lobanov/setup-xcode@60606e260d2fc5762a71e64e74b2174e8ea3c8bd # v1.6.0`, and add Dependabot/Renovate for github-actions to keep pins fresh. At minimum do this for the release.yml jobs that can see signing secrets.

### [high] Main window auto-presents at launch, violating menubar-first startup (CLAUDE.md 2.1)

**Category**: bug · **Where**: `apps/macos/Sources/Vapor/VaporApp.swift:21` · **Review group**: macos-app-shell

The `Window` scene in apps/macos/Sources/Vapor/VaporApp.swift:21 is the primary scene, and SwiftUI auto-presents it at every app launch; nothing in the codebase suppresses or closes it. `prepareMenubarOnlyStartupSurface()` only routes to `setDockVisible(false)`, which sets activation policy `.accessory` — a guaranteed no-op in the packaged app because package.sh sets `LSUIElement=true` (MacAppRuntimeController even early-returns on a matching policy) — and an activation-policy change cannot suppress window presentation anyway. Result: on login-item launch (SMAppService.mainApp) or any manual launch, the main window appears on screen (with no Dock icon, an inconsistent state), violating CLAUDE.md §2.1 and docs/architecture/macos/app-lifecycle.md ("menubar surface without opening the main window"). Fix: apply `.defaultLaunchBehavior(.suppressed)` and `.restorationBehavior(.disabled)` (macOS 15+; baseline is macOS 26) to the Window scene so it opens only via the menubar `openWindow` action; `prepareMenubarOnlyStartupSurface()` can then be reduced to a log line.

**Suggested fix**: Apply `.defaultLaunchBehavior(.suppressed)` (and `.restorationBehavior(.disabled)`, macOS 15+; the project baseline is macOS 26) to the `Window` scene so it only opens via the menubar `openWindow` action. Then `prepareMenubarOnlyStartupSurface()` can be removed or reduced to a log line.

### [high] App config save clobbers daemon-owned keys with a stale startup snapshot

**Category**: bug · **Where**: `apps/macos/Sources/VaporCore/VaporConfiguration.swift:252` · **Review group**: macos-app-core

save() (apps/macos/Sources/VaporCore/VaporConfiguration.swift:252) atomically rewrites vapor.json from the caller's in-memory snapshot with no reload, merge, or lock, and AppShellViewModel loads the config exactly once at init (AppShellViewModel.swift:23) then saves that snapshot on every settings change (lines 337/365/411/442). The additionalKeys passthrough only preserves unknown keys that existed at load time, and encodeIfPresent drops a nil deviceId. Concrete failure: on a fresh install the app writes a default vapor.json without deviceId, bootstraps the daemon, and the daemon persists deviceId (core/daemon/src/bootstrap.rs:96, documented in core/shared/src/device_id.rs as "never silently regenerated"); any later settings toggle in the same app session rewrites the file from the stale snapshot and deletes deviceId. On the next daemon start the id is re-derived from the hostname — usually the same value, but permanently different if the machine was renamed since first persist or the random 12-hex fallback applies, which changes the keep-both conflict-suffix identity. Independently and unconditionally, any `vapor config set` made after app launch (syncMode, profiles, resourceLimits, provider, idleBoost — the CLI carefully preserves other keys, core/cli/src/main.rs:171) is silently reverted by the app's next save; reverting syncMode to a stale one-way value re-enables a strict-mirror mode that can permanently overwrite divergent edits. This directly contradicts the struct's own guarantee comment ("an app-side settings write can never destroy runtime configuration").

**Suggested fix**: In save(), re-load the on-disk file, re-capture unknown/daemon-owned keys (deviceId, profiles, syncMode, provider, resourceLimits, idleBoost) from the fresh read, overlay only the app-modeled fields being changed, and write under an advisory lock (or route config writes through the vapor CLI so the Rust side owns the read-modify-write).

### [high] OAuth authorization code is never percent-decoded, then re-encoded on exchange — real Google logins fail with invalid_grant

**Category**: bug · **Where**: `core/cli/src/commands/auth.rs:216` · **Review group**: cli-commands

run_gdrive_pkce_flow (core/cli/src/commands/auth.rs:216-227) extracts the 'code=' query value verbatim from the raw redirect request line with no percent-decoding (no decode helper exists anywhere in the codebase). Google authorization codes contain '/' ('4/0A...') and arrive percent-encoded ('4%2F...') per RFC 6749 §4.1.2 form-urlencoding. exchange_code (core/providers/src/gdrive/oauth.rs:92-98) then url_encodes the still-encoded code, turning '%' into '%25', so the token endpoint receives 'code=4%252F...', decodes once to '4%2F...', and rejects with invalid_grant. Every real interactive `vapor auth login gdrive` therefore fails at the token exchange after a successful consent screen. Fix: percent-decode the extracted query value (%XX and '+') before calling exchange_code, plus a test with a code containing %2F asserting the form body carries the single-encoded form.

**Suggested fix**: Percent-decode the extracted query value (at minimum decode %XX sequences and '+') in run_gdrive_pkce_flow before passing it to exchange_code, and add a test with a code containing '%2F' asserting the exchange body carries the single-encoded form.

### [medium] Preflight `git fetch origin main --depth=1` can shallow-graft main and falsely reject a valid release tag

**Category**: bug · **Where**: `.github/workflows/release.yml:73` · **Review group**: ci

In .github/workflows/release.yml the preflight job checks out with fetch-depth: 0 (full history) but then runs `git fetch origin main --depth=1` (line 73). A depth-limited fetch records the fetched tip in .git/shallow, grafting it with no traversable parents even though the full history already exists locally. If any commit merges to main between the tag push and this step, `git merge-base --is-ancestor "$GITHUB_SHA" origin/main` cannot walk from the shallow-grafted tip back to the tagged commit and exits non-zero, so a legitimate release fails with "tag commit must be reachable from origin/main". This is not merely a transient race: re-running the workflow performs a fresh shallow fetch of the then-current main tip, so once main has advanced past the tagged commit the preflight fails deterministically on every re-run and the tag must be recut to release. Fix: drop --depth=1 (checkout already fetched full history, so `git fetch origin main` is cheap) or verify reachability against the already-fetched refs (e.g., `git branch -r --contains "$GITHUB_SHA"`). Verified by local reproduction: shallow fetch after a new main commit yields .git/shallow containing the tip and is-ancestor exit 1; plain fetch passes.

**Suggested fix**: Drop `--depth=1` (the checkout already fetched full history, so `git fetch origin main` is cheap), or use `gh api` / `git branch -r --contains` against the fully-fetched refs instead of a fresh shallow fetch.

### [medium] Release job uses GitHub Environment `release` but the contract and docs mandate `release-macos`

**Category**: bug · **Where**: `.github/workflows/release.yml:96` · **Review group**: ci

The release job in .github/workflows/release.yml:96 declares `environment: release`, but AGENTS.md §7.1 and docs/operations/macos/distribution-trust-chain.md:49 (plus docs/ci/README.md, docs/plans/macos.md, docs/operations/distribution-trust-chain.md) require macOS signing/notarization secrets to live in an isolated `release-macos` GitHub Environment, explicitly not repository-wide secrets. If an operator provisions the Apple secrets in `release-macos` as documented, the job's secrets.* references (lines 146–153) resolve empty, `signing_config` computes signing_configured=false, and any stable tag push hard-fails at lines 176–179 ("stable release requires signing identity, notarization profile, Developer ID certificate, and notary API credentials"). Alternatively, if secrets are provisioned in `release` to make the workflow pass, the documented per-platform secret-isolation model is violated once Windows/Linux release jobs land. Fix: change line 96 to `environment: release-macos` and verify secrets are provisioned under that environment.

**Suggested fix**: Change to `environment: release-macos` (or `environment: release-macos` per job once other platforms land) so the workflow matches the documented trust-chain policy, and verify the secrets are provisioned under that environment.

### [medium] Signing/release job restores shared incremental build cache into the artifacts it signs and notarizes

**Category**: security · **Where**: `.github/workflows/release.yml:122` · **Review group**: ci

The release job in .github/workflows/release.yml (lines 121-132) restores target/ and ~/.cargo/* via a broad prefix restore-key (`${{ runner.os }}-cargo-`) before `./scripts/build.sh package` runs `cargo build --workspace --release` and package.sh copies target/release/{vapord,vapor} into the codesigned, notarized Vapor.app. All four other workflows (lint/test/perf/build) write caches under the same key format on main, so the signed release artifacts can link cached .rlib/.o objects produced by any earlier main-scoped CI run, with no integrity or provenance verification (cargo trusts fingerprint files that live inside the same cache). A poisoned cache (e.g. from a malicious transitive build.rs that once ran on main, or a compromised mutable-tag-pinned action in any main workflow) persists into subsequent signed releases even after the offending code is reverted, and ~/.cargo/bin being on PATH means cached binaries could execute inside the job holding the unlocked signing keychain. Release builds are also not reproducible from source at the tag. Exploitation requires prior code execution in a main-branch workflow (PR caches are branch-isolated), making this a supply-chain hardening gap rather than a directly reachable compromise. Fix: drop the cargo/SPM cache-restore steps from the release job (the 60-minute budget absorbs a clean build), or at minimum use a release-scoped exact key with no restore-keys.

**Suggested fix**: Drop the cargo/SPM cache-restore steps from the `release` job (accept the clean full build; the job already has a 60-minute budget), or at minimum scope the key exactly to release builds with no prefix restore-keys.

### [medium] Re-running the release workflow demotes an already-published release back to draft (and never clears the prerelease flag)

**Category**: bug · **Where**: `.github/workflows/release.yml:334` · **Review group**: ci

The 'Create or update GitHub Release' step (.github/workflows/release.yml:334) unconditionally runs `gh release edit "$TAG_NAME" --draft` when the release already exists. The release runbook (docs/operations/release-process.md) explicitly allows re-running the tag workflow, so if the workflow is re-run after the owner has published the release, the published release is silently flipped back to draft: the release, its assets, and the "latest" pointer disappear from public view and existing asset download URLs 404 until an operator notices and re-publishes. The following `gh release upload --clobber` step also replaces the published assets. Secondary (minor): `prerelease_flag=()` for stable versions means the edit path never passes `--prerelease=false`; however, since RELEASE_PRERELEASE is derived deterministically from the tag, this only matters if the flag was changed manually in the GitHub UI. Fix: only pass `--draft` when the existing release is still a draft (e.g. check `gh release view --json isDraft`), or skip/fail loudly for published releases; optionally pass explicit `--prerelease=true|false` to always reconcile the flag.

**Suggested fix**: Only pass `--draft` when the existing release is still a draft (check `gh release view --json isDraft`), or skip the edit for published releases and fail loudly instead. Use explicit `--prerelease=true|false` (gh supports the `=false` form on edit) so the flag is always reconciled to `RELEASE_PRERELEASE`.

### [medium] Identical cargo cache key across lint/test/build/perf/release jobs means only one job's target/ ever gets saved

**Category**: perf · **Where**: `.github/workflows/test.yml:58` · **Review group**: ci

All five workflows (lint, test, build, perf, release) compute the identical cache key `${{ runner.os }}-cargo-${{ hashFiles('**/Cargo.lock') }}` while producing incompatible `target/` contents (clippy check-profile artifacts for lint, debug test/e2e builds for test, `--release` workspace builds for build/perf/release). actions/cache only saves on an exact-key miss and caches are immutable, so whichever concurrent job finishes first permanently owns the key for that Cargo.lock hash on that branch scope; the other jobs restore a mismatched multi-GB target dir, pay the download/extract cost, recompile the workspace from scratch on every run, and never save their own artifacts. The `restore-keys` prefix fallback repeats the same first-writer-wins race on every Cargo.lock change. This works against the §9.5 "under 5 minutes per OS on CI" Tier 1 budget, especially on macOS runners. Additionally, caching `~/.cargo/bin/` (restored after setup-rust-toolchain runs) can overwrite freshly installed rustup/cargo shims with stale cached ones. Fix: add a workflow/job discriminator to the key (e.g. `${{ github.workflow }}`) or switch to SHA-pinned Swatinem/rust-cache, which keys per-job and handles ~/.cargo contents correctly.

**Suggested fix**: Include the workflow/job name in the key (e.g. `${{ runner.os }}-cargo-${{ github.workflow }}-${{ hashFiles('**/Cargo.lock') }}`) or switch to `Swatinem/rust-cache` (SHA-pinned), which handles per-job keying and excludes `~/.cargo/bin` correctly.

### [medium] Crash-loop acknowledge spawns two CLI subprocesses synchronously on the main thread

**Category**: perf · **Where**: `apps/macos/Sources/Vapor/AppShellViewModel.swift:214` · **Review group**: macos-app-shell

acknowledgeCrashLoopPause() (AppShellViewModel.swift:214-218) runs two synchronous `vapor` CLI subprocess round-trips on the @MainActor: line 215 spawns `vapor service acknowledge --json` and line 216 spawns `vapor service status --json`, each via stateQueue.sync → ProcessVaporCLIRunner (process.run() + waitUntilExit()). Clicking "Acknowledge" in the menubar popover (ShellView.swift:111) therefore blocks the main run loop for both subprocess round-trips, contradicting the file's own comments at lines 61-65 and 480-482 that manager-backed reads cause main-thread jank and must stay off main. refreshCrashLoopPauseState() (line 210-212) has the same pattern with one spawn, though it currently has no non-test UI caller. Fix: dispatch both operations onto lifecycleQueue as toggleAutoLaunch() already does, and publish the resulting crashLoopPaused value back on the main actor.

**Suggested fix**: Move both operations onto `lifecycleQueue` like `toggleAutoLaunch()` does, and hop back to the main actor to publish the resulting `crashLoopPaused` value.

### [medium] Quit can leave the daemon running: race between queued auto-launch work on lifecycleQueue and the main-thread quit path

**Category**: bug · **Where**: `apps/macos/Sources/Vapor/AppShellViewModel.swift:239` · **Review group**: macos-app-shell

toggleAutoLaunch()/disableAutoLaunchAndStopNow() (and bootstrapDaemonLifecycleIfNeeded()) enqueue lifecycle operations asynchronously on lifecycleQueue, but handleQuitFromMenuBar() runs stopDaemonForTermination() synchronously on the main actor via AppLifecycleCoordinator, bypassing lifecycleQueue; the two paths order only via nondeterministic contention on DaemonLifecycleManager.stateQueue. If a user enables auto-launch and immediately clicks Quit, quit's `vapor service stop` can win the stateQueue race; the queued installAndEnable then runs `vapor service install` (which starts the daemon via launchd) after the stop, and — provided the subprocess is spawned before the app finishes exiting — the daemon is left running after Quit, violating CLAUDE.md §2.1's requirement that Quit executes the daemon stop path and then terminates. Even in favorable orderings, terminateApplication() can fire while a queued lifecycle subprocess is mid-flight. Fix: serialize the quit-time stop through lifecycleQueue and call terminateApplication() only from that queue's completion hop back to the main actor so quit is always the last lifecycle operation.

**Suggested fix**: Route the quit-time stop through the same `lifecycleQueue` (serializing behind any pending toggles) and call `terminateApplication()` only from that queue's completion hop back to the main actor, so quit is always the last lifecycle operation.

### [medium] Quit path blocks the main thread on unbounded CLI subprocess waits — Quit Vapor can hang forever

**Category**: bug · **Where**: `apps/macos/Sources/Vapor/AppShellViewModel.swift:240` · **Review group**: macos-app-shell

`handleQuitFromMenuBar()` executes entirely on the main actor: (1) `healthMonitor?.stop()` does `queue.sync` against the serial queue running health ticks, so an in-flight `vapor service check` subprocess (unbounded `waitUntilExit`) blocks the main thread; (2) `AppLifecycleCoordinator.handleQuitFromMenuBar()` synchronously calls `stopDaemonForTermination()` -> `stateQueue.sync` -> spawns `vapor service stop --json` and `waitUntilExit()`s with no timeout, and `terminateApplication()` only runs afterward. Additionally, `ProcessVaporCLIRunner.run` calls `waitUntilExit()` before draining the stdout/stderr pipes, so any CLI child writing more than the ~64KB pipe buffer (e.g. a panic backtrace or verbose stderr) deadlocks the wait permanently. Normal-case impact: brief main-thread beachball on every quit while the subprocess and launchctl round-trip complete (the Rust stop path does not wait for graceful daemon exit, so it is usually fast). Failure case: a stalled launchctl, wedged CLI, or oversized pipe output hangs Quit indefinitely and the app never terminates — the user must force-kill Vapor. Fix: run the stop sequence off the main actor with a bounded deadline and call `terminateApplication()` regardless of outcome; in `ProcessVaporCLIRunner`, read pipes concurrently (readability handlers or background reads) before `waitUntilExit()` and add kill-on-timeout.

**Suggested fix**: Run the stop sequence off the main actor with a bounded deadline (e.g. a few seconds), then call `terminateApplication()` regardless of outcome; in `ProcessVaporCLIRunner`, read the pipes concurrently (readability handlers or background reads) before `waitUntilExit()` and add a kill-on-timeout.

### [medium] Main thread blocks on vapor CLI subprocess behind a queue shared with the 30s health tick

**Category**: perf · **Where**: `apps/macos/Sources/VaporCore/DaemonLifecycle.swift:215` · **Review group**: macos-app-core

acknowledgeCrashLoopPause and stopDaemonForTermination do stateQueue.sync { spawn vapor CLI subprocess + waitUntilExit } and are called directly from @MainActor code: AppShellViewModel.acknowledgeCrashLoopPause() (spawns TWO subprocesses back-to-back: acknowledge then status, via ShellView's crash-loop banner button) and AppLifecycleCoordinator.handleQuitFromMenuBar (menubar Quit). isInCrashLoopPause has the same hazard via refreshCrashLoopPauseState(), though that method currently has no production UI call site; the autoLaunchEnabled getter is only read from lifecycleQueue.async blocks and is not a main-thread hazard today. stateQueue is shared with DaemonHealthMonitor's utility-QoS 30s tick (checkDaemonHealth → vapor service check, which can perform a launchctl restart taking seconds), and the quit path additionally blocks in healthMonitor.stop()'s queue.sync behind an in-flight tick. Worst case: a health tick is mid-restart when the user clicks the crash-loop banner or quits → the main thread parks behind the tick's subprocess (GCD priority donation cannot boost the child process) → multi-second beachball. Even uncontended, each UI action synchronously blocks the main thread for a full subprocess round-trip. Fix: make the lifecycle seam async (hop to a background executor, as bootstrapIfNeeded/toggleAutoLaunch already do via lifecycleQueue.async) and have the UI read cached state updated by the health monitor; never call stateQueue.sync from @MainActor code.

**Suggested fix**: Make the lifecycle seam async: expose async methods (or completion handlers) that hop to a background executor, and have the view model read cached state updated by the health monitor instead of synchronously shelling out from the main actor. Never call stateQueue.sync from @MainActor code.

### [medium] Login-item registration failures are swallowed: UI reports 'Start at login' ON while the app will not launch at login

**Category**: bug · **Where**: `apps/macos/Sources/VaporCore/DaemonLifecycle.swift:276` · **Review group**: macos-app-shell

registerLoginItemIfAvailable() (apps/macos/Sources/VaporCore/DaemonLifecycle.swift:276) catches every SMAppService.register() error and only logs a warning; setAutoLaunchEnabled(true) still returns success and AppShellViewModel.toggleAutoLaunch publishes autoLaunchEnabled = true from the Rust-persisted preference, so no error is surfaced. SMAppService.mainApp.register() throws in real conditions — most commonly "Operation not permitted" when the user previously disabled the login item in System Settings, or under MDM restriction (the `.requiresApproval` status cited in the original finding applies to agent/helper services rather than .mainApp). Failure scenario: user enables "Start at login", registration throws, toggle shows ON; after reboot the daemon LaunchAgent starts (sync continues) but the Vapor app/menubar surface never launches, and the user has no indication why. The app never checks service.status after register and never offers SMAppService.openSystemSettingsLoginItems(). Partially mitigated by bootstrapIfNeeded() retrying registration on each manual app launch. CLAUDE.md §6 requires permissioned features to degrade safely when denied; silently reporting success does not.

**Suggested fix**: Propagate a distinct outcome (e.g. `loginItemRequiresApproval`) from `setAutoLaunchEnabled`, check `service.status` in `SMAppServiceLoginItemController` after register, surface it in `AppShellState` with a hint that opens System Settings (`SMAppService.openSystemSettingsLoginItems()`).

### [medium] ProcessVaporCLIRunner reads pipes only after waitUntilExit and has no timeout — latent permanent deadlock

**Category**: bug · **Where**: `apps/macos/Sources/VaporCore/VaporCLIServiceController.swift:56` · **Review group**: macos-app-core

run() calls process.waitUntilExit() before draining outputPipe/errorPipe. If the child writes more than the ~64 KiB pipe buffer to either stream (e.g. a Rust panic with RUST_BACKTRACE=full inherited from the app's environment, or any future verbose/error output — the runner is generic over arguments), the child blocks in write() while the parent blocks in waitUntilExit(): both hang forever. There is also no timeout, so a genuinely hung CLI (launchctl stall, vapor_dir lock contention) has the same effect. Because every lifecycle operation serializes behind DaemonLifecycleManager.stateQueue, one wedged invocation permanently freezes health monitoring, the settings toggles, and Quit Vapor (handleQuitFromMenuBar blocks the main thread forever and the app can no longer terminate cleanly).

**Suggested fix**: Read both pipes concurrently (readabilityHandler or background readDataToEndOfFile on each pipe) BEFORE waitUntilExit, and add a bounded timeout that terminates the child and throws (e.g. process.terminate() after N seconds) so a hung CLI can never wedge the lifecycle queue.

### [medium] PKCE loopback listener accepts exactly one connection with no state parameter, no read timeout, and no request filtering

**Category**: security · **Where**: `core/cli/src/commands/auth.rs:207` · **Review group**: cli-commands

The PKCE loopback flow in run_gdrive_pkce_flow (core/cli/src/commands/auth.rs:207) performs a single listener.accept() with no read timeout, no state parameter, and no request filtering. (1) A first connection that sends no bytes (e.g. Chrome speculative preconnect to loopback) either hangs the CLI forever in read_line or, when the idle socket closes, aborts with "the redirect did not carry an authorization code" while the real redirect is never accepted. (2) authorization_url (core/providers/src/gdrive/oauth.rs:58) carries no state, so any local process hitting http://127.0.0.1:<port>/?code=x consumes the single accept, aborting the legitimate login and causing an attacker-supplied code to be POSTed to Google's token endpoint (PKCE prevents actual token compromise, so this is flow-kill/DoS, not theft; the remote web-page vector is additionally limited by Chrome Private Network Access). (3) On denied consent (?error=access_denied) the code returns Err at line 227 before writing any HTTP response (write at line 229), leaving the browser tab on a connection error. Fix: add a random state parameter validated against the redirect, loop on accept() discarding non-matching requests, set per-connection read timeouts plus an overall flow deadline, and write a proper response page for error= redirects before returning.

**Suggested fix**: Generate a random `state` value, include it in the authorization URL, and loop on accept() (with per-connection read timeouts and an overall flow deadline) discarding any request whose state does not match; respond to error= redirects with a proper page before returning the error.

### [medium] vapor.json read-modify-write has no locking and shares a fixed temp filename with the lifecycle autolaunch store — concurrent writes are lost or fail

**Category**: bug · **Where**: `core/cli/src/commands/config.rs:85` · **Review group**: cli-commands

vapor.json writers do unlocked read-modify-write and share the fixed staging name '<vapor_dir>/vapor.vapor-tmp': config::set (core/cli/src/commands/config.rs:85), JsonFileAutoLaunchSettingStore::write (core/lifecycle/src/auto_launch.rs:157), the daemon IPC handlers set_auto_launch/update_excludes (core/daemon/src/ipc_service.rs:167), and device_id::resolve_or_persist (core/shared/src/device_id.rs:73). Any two of these running concurrently across processes (macOS app shim driving `vapor service`, the long-running daemon, a user CLI invocation) can (a) lose an update — both read version N, each publishes its own full document, and the last rename silently discards the other's key while both report success (e.g. a `vapor config set syncMode pull-only` reverted by a service enable/disable or first-run bootstrap writing autoLaunch back), or (b) collide on the shared temp file — writer A renames a temp file writer B is still writing, publishing a truncated/corrupt vapor.json (which then hard-errors the lifecycle store), or B's rename fails NotFound. Note: steady-state `vapor service bootstrap` is read-only; it writes autoLaunch only on first observation, while enable/disable and the daemon IPC paths write unconditionally. Fix: unique per-writer temp names (tempfile in the same directory) plus an advisory file lock shared by all vapor.json writers, re-reading under the lock before publish.

**Suggested fix**: Use a unique temp name per writer (e.g. tempfile in the same directory / PID+random suffix) and serialize vapor.json writers with an advisory file lock (flock on a sidecar) shared by config.rs and the lifecycle stores, so a set() re-reads under the lock before publishing.

### [medium] vapor auth login gdrive silently ignores the documented stdin-token path and blocks on a browser OAuth flow

**Category**: bug · **Where**: `core/cli/src/main.rs:470` · **Review group**: cli-core

The AuthAction::Login help text (core/cli/src/main.rs:139-144) promises "Omit --token (or pass --token -) to read the token from stdin" and still says the OAuth-PKCE flow "lands later," but for provider gdrive, omitting --token now takes the PKCE branch (main.rs:468-475) instead of reading stdin. A headless script doing `echo "$TOKEN" | vapor auth login gdrive` per the help either fails fast on missing VAPOR_GDRIVE_CLIENT_ID or, when that env var is set, binds a loopback listener, prints an authorization URL to stderr, and blocks indefinitely on listener.accept() (core/cli/src/commands/auth.rs:186-209) waiting for a browser redirect that never comes — an unbounded hang in CI/automation. `--token -` still reads stdin, so a workaround exists, but the primary documented form does the opposite of the help for the MVP provider. Fix: gate the browser flow on stdin being a TTY (fall back to stdin read otherwise) or behind an explicit flag, and at minimum update the stale help text to state the gdrive exception.

**Suggested fix**: Only auto-run the PKCE flow when stdin is a TTY (fall back to the stdin read otherwise), or gate the browser flow behind an explicit flag (e.g. --oauth). At minimum, correct the Login help text to state the gdrive exception so scripts don't follow a false contract.

### [medium] One malformed field value silently discards the user's entire configuration (whole-file serde failure -> all defaults)

**Category**: bug · **Where**: `core/shared/src/config.rs:268` · **Review group**: shared

A single malformed field value in vapor.json (type mismatch like "timelineEventLimit": "500" or "autoLaunch": "true", or range overflow like "cpuPercent": 300 into u8) fails the single whole-file serde_json::from_str in core/shared/src/config.rs:263, so load_from returns pure defaults plus load_issue, discarding every valid field (profiles, provider, sync directories). core/daemon/src/bootstrap.rs:84 only logs the issue and continues: the daemon resolves default scope (~/Vapor <-> /Vapor, filesystem provider), creates the missing default local root (sync_directories.rs:122), and runs against it while the user's configured sync silently stops; load_issue never reaches vapor status or gates sync. Not data loss (default two-way is keep-both) and requires a hand-edited file, and all-defaults-on-corrupt is partially documented design — but treating one bad field as whole-file corruption contradicts the per-field-tolerance intent and silently discards user intent. Fix: decode per-field via serde_json::Value (collecting per-field issues) or refuse to start sync (Paused with reason) when load_issue is set.

**Suggested fix**: Parse to serde_json::Value first and decode each known field individually (collecting per-field issues into load_issue) so one bad field only reverts that field; alternatively make the daemon refuse to start sync work (Paused with reason) when load_issue is set, rather than running against defaults.

### [medium] Unknown/misspelled top-level config keys are silently dropped even though ALL_KEYS exists for validation

**Category**: improvement · **Where**: `core/shared/src/config.rs:280` · **Review group**: shared

RawVaporConfig (core/shared/src/config.rs:280) silently ignores unknown top-level vapor.json keys with no signal: load_from returns load_issue=None, the daemon logs nothing, and `vapor doctor` never inspects config contents. Meanwhile `vapor config set` already validates keys against constants::config::ALL_KEYS, and README.md documents vapor.json keys for hand-editing. A hand-edited typo like "profles": [...] or "syncmode": "pull-only" is dropped, compiled defaults apply, and the daemon syncs a different directory/profile set than the user configured with zero diagnostics. (Mitigating: syncMode typos fail safe to two-way keep-both, so the strict-mirror one-way modes cannot be enabled by typo.) Fix: after a successful parse, diff the raw JSON top-level keys against ALL_KEYS (which includes deviceId, intentionally absent from RawVaporConfig) and surface unrecognized keys as a non-fatal warnings field alongside load_issue, logged by the daemon and reported by `vapor doctor` — preserving forward-compat tolerance while removing the silence.

**Suggested fix**: After a successful parse, deserialize to serde_json::Value as well, diff top-level keys against constants::config::ALL_KEYS, and surface unrecognized keys as a non-fatal warning (a new `warnings` field next to load_issue, logged by the daemon and shown by `vapor doctor`).

### [medium] resolve_or_persist read-modify-write races with other config writers and can silently revert their settings

**Category**: bug · **Where**: `core/shared/src/device_id.rs:47` · **Review group**: shared

resolve_or_persist (core/shared/src/device_id.rs:47) performs an unlocked read-modify-write on vapor.json: it snapshots the file, derives the device id (including a multi-millisecond `hostname` subprocess spawn), and renames its own serialization over the file with no lock and no re-check. Any write by the other unlocked config writers (Swift VaporConfigurationStore.save, `vapor config set`) landing in that window is silently reverted to the daemon's stale snapshot — a lost-update race. The window exists only during the daemon's first-ever start per vapor_dir (deviceId absent), and the macOS app deliberately runs daemon bootstrap concurrently with app-side config writes at first launch, so the exposure is real but narrow (milliseconds, once per install) and recoverable by re-saving the setting. Additionally, `vapor config set` uses the same fixed temp path (`vapor.vapor-tmp`, config.rs:85) as resolve_or_persist, so a concurrent daemon resolve and CLI set can collide on the temp file (one rename installing the other's bytes, or the loser's rename failing NotFound). Note the daemon singleton lock does not protect this file — it only excludes a second daemon. Fix as suggested: advisory lock around the read-modify-write (or re-verify deviceId absence immediately before rename) and a unique temp filename suffix.

**Suggested fix**: Take an advisory lock (e.g. flock on vapor.json or a sibling lock file, as vapord.lock already does for the daemon) around the read-modify-write, or re-read and verify the deviceId key is still absent immediately before rename; use a unique temp filename (PID/random suffix).

### [medium] resolve_or_persist writes vapor.json with default (world-readable) permissions and creates vapor_dir 0755, bypassing the PRIVATE_*_MODE policy

**Category**: security · **Where**: `core/shared/src/device_id.rs:74` · **Review group**: shared

resolve_or_persist (core/shared/src/device_id.rs:74-75) writes vapor.json via fs::write(temp)+fs::rename, creating the file with umask-default mode (typically 0644) instead of PRIVATE_FILE_MODE 0600, violating the documented permission policy (docs/operations/runtime-logging-and-localization.md: "0600 for config/log/state files") and bypassing runtime_paths::ensure_private_file, which logging.rs and state_db.rs already use. Concrete downgrade: the macOS app writes vapor.json at 0600 (VaporConfiguration.swift:262-263 via VaporPaths.ensurePrivateFile), and the first daemon run then rewrites it as 0644 via bootstrap.rs:96 — other local users on a multi-user Unix machine can read the config (sync-root paths, deviceId; no tokens, which live in the secret store). Note: the "creates vapor_dir 0755" part of the original claim is effectively unreachable through device_id.rs in the daemon, because SingletonLock (singleton.rs:68, ensure_private_directory) runs before resolve_or_persist and device_id.rs has no other callers; however the same gap in the CLI (core/cli/src/commands/config.rs:77-87, `vapor config set` uses fs::create_dir_all + fs::write + rename) IS reachable on a fresh VAPOR_DIR and creates both a 0755 vapor_dir and a 0644 vapor.json. Fix: create the parent via runtime_paths::ensure_private_directory and open the temp file with OpenOptionsExt::mode(PRIVATE_FILE_MODE) before rename, sharing one helper with the CLI config write path.

**Suggested fix**: Create the parent via runtime_paths::ensure_private_directory, and write the temp file with OpenOptionsExt::mode(PRIVATE_FILE_MODE) (or chmod 0600 before rename), mirroring the discipline logging.rs/state_db.rs already use. The CLI's config write path (core/cli/src/commands/config.rs fs::write) has the same gap and should share one helper.

### [medium] No log rotation or size cap: vapor.logs / vapord.logs grow without bound for a long-running daemon

**Category**: improvement · **Where**: `core/shared/src/logging.rs:175` · **Review group**: shared

open_log_file (core/shared/src/logging.rs:173-176) opens vapor.logs/vapord.logs in append mode and StructuredLogger writes+flushes every line with no rotation, size cap, or truncation anywhere in the repo (core/cli/src/commands/ipc.rs:265 itself describes the file as "the unrotated log"). vapord is an always-on daemon with ~91 log call sites, defaulting to Debug level under VAPOR_ENV=dev, so vapor_dir/logs/vapord.logs grows without bound over weeks-to-months of storms, retries, and diagnostics — contrary to the product's low-impact invariant (§1, §8.1). The launchd StandardOutPath/StandardErrorPath redirect files (vapord.stdout.log/vapord.stderr.log, wired in core/cli/src/commands/service.rs:381-382 and core/platform/src/service/macos.rs) have the same unbounded-append property. Suggested fix: size-based rotation in StructuredLogger (rename to vapord.logs.1 and reopen past an N-MiB cap defined in constants.rs, keeping K generations) and daemon-startup trimming of the stdout/stderr redirect files.

**Suggested fix**: Add size-based rotation in StructuredLogger (e.g. rename to vapord.logs.1 and reopen past an N-MiB cap defined in constants.rs, keeping K generations), and have the daemon trim the stdout/stderr redirect files at startup.

### [medium] S9 runs rm -rf on a directory derived from an unvalidated log-parsed path

**Category**: bug · **Where**: `scripts/e2e.sh:402` · **Review group**: scripts

scripts/e2e.sh:402 runs `rm -rf "$(dirname "$deep_socket")"` where `$deep_socket` is parsed from a daemon log line with `sed 's/.*socket_path=\([^ ]*\).*/\1/'` and only checked for non-emptiness. The originally claimed triggers are weaker than stated: log-format drift causing sed pass-through produces a garbage relative path starting with the epoch timestamp (rm -rf no-op, verified empirically), and the daemon can never bind directly in $TMPDIR (runtime_paths.rs always interposes a `vapor-<hash>` subdirectory). The real, current-code hazard is the space-unsafe capture group `[^ ]*`: e2e.sh does not control TMPDIR and the daemon's env::temp_dir() honors it, so with a space-containing TMPDIR (e.g. "/Users/alex/tmp dir") the extracted path truncates to "/Users/alex/tmp", dirname yields "/Users/alex", and the script rm -rf's a real parent directory — potentially the user's home. Fix as suggested: require the extracted value to end in `/vapord.sock` and resolve under "${TMPDIR:-/tmp}" before deleting, or remove only the socket's parent by exact known shape (temp_dir/vapor-<hash>), or simply `rm -f` the socket file itself.

**Suggested fix**: Validate before deleting: require `[[ "$deep_socket" == */vapord.sock ]]` (or a vapor-specific directory component) and that the resolved dir is under "${TMPDIR:-/tmp}"; otherwise skip the removal and fail with diagnostics. Alternatively remove only the socket file itself with rm -f.

### [medium] Pre-commit hook runs clean.sh, forcing a full cold rebuild of the entire workspace on every commit

**Category**: perf · **Where**: `scripts/hooks.sh:63` · **Review group**: scripts

The pre-commit hook installed by scripts/hooks.sh (line 63 of the emitted hook) runs scripts/clean.sh first, deleting target/, dist/, .vapor/, apps/macos/.build, and .swiftpm — the entire cargo and Swift incremental caches. Each commit then performs three cold Rust compiles (clippy --all-targets --all-features, cargo test --all-targets --all-features, cargo build --workspace --release) plus a cold swift build -c release with whole-module and cross-module optimization, turning the per-commit gate into a tens-of-minutes operation and incentivizing --no-verify. It also silently rm -rf's .vapor/, destroying any e2e sandbox preserved via e2e.sh --keep/--sandbox (including a running sandbox daemon's runtime dir) and dist/ artifacts — a side effect no doc acknowledges. Caveats: the hook is opt-in ("optional but recommended for agentic workflows", docs/development/runbook.md), and the full-rebuild trade-off itself is explicitly documented as intentional in docs/development/runbook.md (lines 70-75), which already directs contributors wanting a fast loop to run lint/test manually. So the perf cost is a deliberate, documented design choice being disputed; the .vapor/dist destruction is the genuinely unacknowledged defect. Suggested fix (drop clean from the hook, or at minimum stop deleting .vapor/dist in the commit path; keep from-scratch guarantees in CI) is compatible with all CLAUDE.md invariants but reverses a documented owner decision, so it should be confirmed with the project owner.

**Suggested fix**: Remove the clean.sh step from the hook (lint/test/build are already deterministic through the wrapper scripts); if a from-scratch guarantee is wanted, keep it in CI only. At minimum stop deleting `.vapor` and `target` in the commit path.

### [medium] sync_cargo_lock swallows all cargo output, so release prep dies silently after already mutating VERSION/Cargo.toml on main

**Category**: bug · **Where**: `scripts/version.sh:152` · **Review group**: scripts

`sync_cargo_lock` (scripts/version.sh:152) runs `cargo update --workspace --manifest-path "$CARGO_TOML" >/dev/null 2>&1`, discarding stdout and stderr — including the shell's own "command not found" message. Under `set -euo pipefail` (line 2), any cargo failure (cargo missing from PATH, broken registry cache, malformed manifest) aborts the script inside `set_version_and_sync` (called as a plain statement at line 298), after `write_version` and `sync_cargo_from_version` have already rewritten VERSION and Cargo.toml. Verified by repro: the pattern exits 127 with zero output. Result: `./scripts/version.sh set X.Y.Z` on a machine without cargo on PATH (or with a cargo failure) exits non-zero silently, leaving main's worktree dirty with a half-prepared release (VERSION/Cargo.toml bumped, Cargo.lock stale, no commit, no tag) and no diagnostic. The `sync` subcommand (line 431) shares the same silent path. Fix: drop `2>&1` or capture stderr and surface it via `die` on failure, consistent with the script's existing error convention.

**Suggested fix**: Drop the `2>&1` redirect (keep stderr, or capture it and surface via die on failure), e.g. `cargo update --workspace --manifest-path "$CARGO_TOML" >/dev/null || die "cargo update --workspace failed; VERSION/Cargo.toml were already updated — inspect the worktree"`.

### [low] Release output validation checks only 2 of the 3 mandated bundle executables — Contents/Helpers/vapor is unverified

**Category**: improvement · **Where**: `.github/workflows/release.yml:291` · **Review group**: ci

The release workflow's "Validate packaged outputs" step (.github/workflows/release.yml:284–291) asserts executability of Contents/MacOS/Vapor and Contents/MacOS/vapord but omits Contents/Helpers/vapor, the third executable mandated by AGENTS.md §7.2 and the binary the macOS app depends on for all daemon/lifecycle control. The helper is currently guaranteed only by an assertion inside apps/macos/scripts/package.sh itself, so the workflow-level gate is not an independent check for it: a future refactor of package.sh that drops the helper copy or its executable bit would pass release validation and ship a bundle with a broken lifecycle shim. Fix: add `test -x dist/Vapor.app/Contents/Helpers/vapor` to the validation step.

**Suggested fix**: Add `test -x dist/Vapor.app/Contents/Helpers/vapor` (and optionally a `--version` smoke run of all three executables) to the validation step.

### [low] No concurrency groups on PR-triggered workflows — superseded pushes keep burning macOS runners

**Category**: improvement · **Where**: `.github/workflows/test.yml:3` · **Review group**: ci

lint.yml, test.yml, and build.yml (each a 3-OS matrix including 30-45-minute macOS jobs; test.yml's macOS job runs the full Tier 1 suite plus ./scripts/e2e.sh --full) have no `concurrency` block; only release.yml defines one. Pushing several quick fixup commits to a PR leaves the superseded matrix runs (up to 9 stale jobs across the three workflows) running to completion alongside the current ones, delaying feedback on the constrained macOS runner pool and wasting the most expensive CI minutes. Suggested fix: add `concurrency: { group: <name>-${{ github.workflow }}-${{ github.ref }}, cancel-in-progress: ${{ github.ref != 'refs/heads/main' }} }` to each PR workflow so superseded PR runs are cancelled while main pushes are never cancelled. Note: release.yml invokes lint.yml/test.yml via workflow_call with github.ref = refs/tags/v*, so that expression evaluates cancel-in-progress to true for release-invoked runs (grouped per tag, so cancellation only triggers if the same tag runs concurrently — harmless, but not "never cancelled"; key the group on github.run_id for tag refs if strict non-cancellation is desired).

**Suggested fix**: Add to each PR workflow: `concurrency: { group: <name>-${{ github.workflow }}-${{ github.ref }}, cancel-in-progress: ${{ github.ref != 'refs/heads/main' }} }` so superseded PR runs are cancelled while main pushes and workflow_call invocations from release are never cancelled.

### [low] toggleAutoLaunch computes the target value from stale UI state, so rapid double-toggle re-applies instead of reverting

**Category**: bug · **Where**: `apps/macos/Sources/Vapor/AppShellViewModel.swift:245` · **Review group**: macos-app-shell

`nextState = !state.autoLaunchEnabled` is captured at click time, but `state.autoLaunchEnabled` is only updated after the queued CLI operation completes (a full `vapor service install/uninstall` round-trip). The Toggle binding also discards the incoming value (`set: { _ in viewModel.toggleAutoLaunch() }`), so there is no optimistic flip. Failure scenario: starting from OFF, the user clicks the toggle, then clicks again ~200ms later to undo; both invocations read `state.autoLaunchEnabled == false` and both enqueue `setAutoLaunchEnabled(true)` — the second click (intended as a revert to OFF) silently becomes a duplicate enable, and the toggle settles at ON, the opposite of the user's final intent.

**Suggested fix**: Track the pending target (e.g. a `pendingAutoLaunchTarget` optional flipped on each click and used as the base for `!`), or optimistically update `state.autoLaunchEnabled` before enqueuing and roll back on failure; alternatively disable the toggle while an operation is in flight.

### [low] Quit from menubar blocks the main thread on a subprocess with no time bound

**Category**: improvement · **Where**: `apps/macos/Sources/VaporCore/AppLifecycleCoordinator.swift:46` · **Review group**: macos-app-core

handleQuitFromMenuBar is @MainActor and synchronously runs stopDaemonForTermination → stateQueue.sync → `vapor service stop` via ProcessVaporCLIRunner, whose run() is `process.run(); process.waitUntilExit()` with no timeout (and it waits before draining pipes, adding a pipe-buffer deadlock path). If the stop subprocess wedges, or stateQueue is held by an in-flight 30s health tick (`vapor service check`) or an autolaunch toggle, the quit action blocks the main thread indefinitely. Additionally, the view model's preceding healthMonitor.stop() is queue.sync on the tick queue, so an in-flight tick blocks the main thread even before the coordinator runs. Fix: run the stop off the main thread with a bounded deadline (a few seconds), then call terminateApplication() regardless of stop outcome, logging the result — consistent with the existing behavior of terminating even when stop throws.

**Suggested fix**: Run the stop on a background task with a bounded deadline (e.g. a few seconds), then call terminateApplication() regardless of stop success/failure, logging the outcome — quitting must never be blockable by a wedged subprocess.

### [low] Crash-loop state fails open: CLI errors report 'not paused', letting the UI clear a real pause

**Category**: bug · **Where**: `apps/macos/Sources/VaporCore/DaemonLifecycle.swift:223` · **Review group**: macos-app-core

isInCrashLoopPause (DaemonLifecycle.swift:214-226) returns false when `vapor service status` fails, and acknowledgeCrashLoopPause (:228-240) swallows its error. AppShellViewModel.acknowledgeCrashLoopPause() (AppShellViewModel.swift:214-218) chains both, so a transient CLI failure (spawn failure, non-zero exit, malformed JSON) at the moment the user clicks the crash-loop banner silently drops the acknowledgement AND clears state.crashLoopPaused, showing the app as healthy while the daemon remains paused. The wrong state is transient, not permanent: DaemonHealthMonitor ticks every 30s and `vapor service check` returns crash_loop_paused while paused, restoring the banner on the next successful tick (failed ticks keep prior state). The durable pause state in the Rust lifecycle core is unaffected. autoLaunchEnabled has a similar fail-open fallback to the shipped default, but its doc comment declares that behavior intentional. Suggested fix: propagate/surface the error (or an explicit unknown state) from acknowledge and status reads instead of defaulting to the healthy value, matching the keep-previous-state behavior the health monitor already has.

**Suggested fix**: Propagate the error (or return an explicit .unknown state) instead of defaulting to the healthy value; have the view model keep the previous known state and surface a 'could not reach vapor CLI' condition rather than clearing the crash-loop indicator.

### [low] StructuredLogger silently loses all logging after the log file is deleted

**Category**: improvement · **Where**: `apps/macos/Sources/VaporCore/StructuredLogger.swift:126` · **Review group**: macos-app-core

StructuredLogger creates vapor_dir/logs/vapor.logs only in prepareLogFile() at init. If the file is deleted mid-run (user cleanup, ./scripts/clean.sh in dev), logging is silently lost in two modes: (a) a logger instance with a live cached FileHandle keeps writing successfully to the unlinked inode, so the data is unrecoverable; (b) a logger instance without a cached handle (or after a write error drops it) hits the fallback FileHandle(forWritingTo:) at line 126, which cannot create the missing file, so the write path bails and every subsequent line from that instance is dropped until app restart — the file is never recreated. Fix: call VaporPaths.ensurePrivateFile(at:fileManager:) before the fallback open at line 126; optionally stat-check the cached handle's inode against the path periodically to detect deletion/rotation.

**Suggested fix**: In the fallback open path, recreate the file via VaporPaths.ensurePrivateFile before opening; optionally stat-check the cached handle's inode against the path periodically to detect deletion/rotation.

### [low] First-launch default-config write can race the daemon and silently swallows errors

**Category**: bug · **Where**: `apps/macos/Sources/VaporCore/VaporConfiguration.swift:224` · **Review group**: macos-app-core

loadResult() (VaporConfiguration.swift:222-225) does fileExists → `try? save(defaultConfiguration)` with no lock or exclusive create, and `save()` atomically replaces vapor.json with defaults that omit deviceId. This is a real TOCTOU, but it is NOT reachable on first launch as claimed: the LaunchAgent does not exist yet then, and in-app ordering is sequential (loadResult runs before daemon bootstrap), so the daemon's resolve_or_persist later adds deviceId while preserving the app-written keys. The race only arises if vapor.json is deleted while the LaunchAgent (RunAtLoad=true) remains installed and the app then loses a millisecond check-then-write window against the starting daemon. Even then, impact is usually self-healing: deviceId is deterministically derived from the hostname, so the next daemon start regenerates the same value; identity actually changes only in the rare random-fallback case (hostname normalizes to empty) or after a hostname change. Separately, `try?` discards the first-run write error, so a read-only or full disk yields a load result claiming issue: nil for a config that was never persisted — a minor diagnostics gap, since user-initiated saves surface errors and the daemon creates the file itself. Suggested hardening (exclusive create + re-read on collision, and surfacing the save failure via VaporConfigurationLoadIssue) is sound and consistent with the "app must never destroy runtime-owned keys" policy.

**Suggested fix**: Create the initial file exclusively (fail if it appeared meanwhile, then re-read instead of overwriting), and report save failures through VaporConfigurationLoadIssue rather than discarding them with try?.

### [low] Runtime-dir resolution diverges from Rust under XCTest: app and spawned CLI use different vapor_dirs

**Category**: bug · **Where**: `apps/macos/Sources/VaporCore/VaporPaths.swift:34` · **Review group**: macos-app-core

Swift runtime-dir resolution (apps/macos/Sources/VaporCore/VaporPaths.swift:34) treats XCTestConfigurationFilePath as a dev trigger resolving ./.vapor, but the Rust side (core/shared/src/runtime_paths.rs:17) only checks CI and VAPOR_ENV=dev. ProcessVaporCLIRunner inherits the app environment without modification, so its doc comment claim that the spawned CLI "resolves the same runtime directory the app uses" is false under XCTest when VAPOR_DIR/VAPOR_ENV/CI are unset: the app side would use ./.vapor while the subprocess uses ~/.vapor, violating the §9.1 never-touch-~/.vapor test invariant. Currently latent — no existing Swift test spawns the real CLI (all use the fake runner seam) and scripts/test.sh exports VAPOR_DIR and VAPOR_ENV=dev, so the divergence only manifests if a future test exercises the real bundled CLI outside the wrapper scripts. Fix by dropping the XCTest special case (relying on the scripts' env exports per §8.5) or mirroring the trigger into runtime_paths.rs.

**Suggested fix**: Drop the XCTestConfigurationFilePath special case (rely on scripts exporting VAPOR_DIR/VAPOR_ENV=dev as §8.5 mandates), or mirror the same trigger into runtime_paths.rs so both sides resolve identically.

### [low] timelineEventLimit accepts negative and zero values that the daemon silently ignores

**Category**: bug · **Where**: `core/cli/src/commands/config.rs:128` · **Review group**: cli-commands

parse_value_for_key (core/cli/src/commands/config.rs:128-133) accepts any i64 for timelineEventLimit, including 0 and negatives, and persists it; shared config loading (core/shared/src/config.rs:313-315) applies no range validation; and the daemon's set_timeline_limit (core/daemon/src/multi_runtime.rs:144-150) silently discards non-positive values, keeping the default limit of 1000. Result: `vapor config set timelineEventLimit -100` exits 0 and `config get` echoes -100, but the running daemon ignores it — the stored config permanently disagrees with effective behavior, breaking the strict-validation posture this file applies to every other typed key (booleans, provider, syncMode). Fix: reject values < 1 in parse_value_for_key with the same ConfigError::Parse style used for booleans and enums.

**Suggested fix**: Reject values < 1 (and optionally cap an upper bound) in parse_value_for_key with the same ConfigError::Parse style used for booleans and enums.

### [low] Conflict scan silently skips unreadable subdirectories, contradicting the 'never silently incomplete' contract

**Category**: improvement · **Where**: `core/cli/src/commands/conflicts.rs:96` · **Review group**: cli-commands

list_conflicts documents skipped_roots as guaranteeing "an empty result is never silently incomplete" (conflicts.rs lines 50-53; same promise in docs/architecture/conflict-resolution.md), but skipped_roots is only populated when a root fails canonicalize(). scan_root's walk does `let Ok(entries) = fs::read_dir(&directory) else { continue; }` (line 96), so any unreadable directory — including an unreadable-but-existing root, which canonicalizes fine and then fails read_dir — is dropped with zero indication. Concrete failure: a keep-both conflict copy under a subdirectory chmod'ed 000 makes `vapor conflicts list` print "No unresolved conflicts." with an empty skippedRoots in --json, so the preserved copy is invisibly orphaned — the exact outcome skipped_roots exists to prevent. (Note: the macOS app does not yet consume this command; the app-side consequence is prospective per the locked --json contract.) Fix: record unreadable directories (append to skipped_roots or a new skipped_directories field) and surface them in render_list and the JSON report; the JSON shape change is acceptable pre-GA.

**Suggested fix**: Record unreadable directories (e.g. append them to skipped_roots or a skipped_directories field) instead of continuing silently, and surface them in render_list / the JSON report.

### [low] Keep-copy fallback deletes the canonical file on any rename error, not only the Windows exists-collision it was written for

**Category**: improvement · **Where**: `core/cli/src/commands/conflicts.rs:198` · **Review group**: cli-commands

resolve_conflict's KeepSide::Copy path treats every fs::rename error as the Windows 'destination exists' case: whenever rename fails and canonical_path.exists(), it removes the canonical file and retries. On Unix, rename atomically replaces an existing destination, so reaching this branch means some other error (read-only remount mid-operation, immutable flag, EIO). If remove_file then succeeds but the second rename still fails, the command exits with an error having already deleted the canonical file, leaving the surviving content stranded under the ~conflict-… name — a destructive step executed inside an error path whose precondition was never verified. The kept version is not lost (the copy survives), but the operation reports failure after mutating state.

**Suggested fix**: Gate the remove-then-rename fallback on cfg(windows) (or on the specific error kind, e.g. AlreadyExists/PermissionDenied from a dest-exists collision) and return the original rename error unchanged elsewhere.

### [low] vapor logs without --tail loads the entire log file into memory before printing

**Category**: perf · **Where**: `core/cli/src/commands/ipc.rs:258` · **Review group**: cli-core

tail_logs with tail=None (core/cli/src/commands/ipc.rs:258) does fs::read_to_string of the whole vapord.logs into one String, which dispatch_logs (main.rs:453) then prints. The Some(n) branch was explicitly engineered to scan backwards in 64 KiB chunks "so tailing a large unrotated log does not load the whole file into memory", but the default `vapor logs` invocation takes exactly that hit. There is no log rotation anywhere in the runtime, and §8.1 permits debug-heavy logging, so on a long-running install the log can reach hundreds of MB and plain `vapor logs` spikes CLI RSS by the full file size — contrary to the product's low-device-impact posture. (Note: the println! call does not add a second full-size allocation; Display streams the string to stdout, so the spike is one file-size allocation.) Suggested fix: stream the no-tail path via io::copy to a locked stdout, or default --tail to a large-but-bounded line count.

**Suggested fix**: Stream the no-tail path: open the file and io::copy it to a locked stdout (also fixing the extra-allocation println), or default --tail to a large-but-bounded line count.

### [low] --foreground is an accepted no-op flag whose presence implies a background default that does not exist

**Category**: improvement · **Where**: `core/cli/src/commands/run.rs:21` · **Review group**: cli-core

`vapor run --foreground` is an accepted no-op: main.rs defines the flag with no help text and run() (run.rs:45) discards RunOptions entirely; run.rs:18-20 documents it as a reserved no-op marker. The flag's existence conventionally implies plain `vapor run` daemonizes, so a script author could expect detach and block on the tick loop — though the subcommand's own help ("Run the daemon in-process (foreground).") already states foreground behavior, making the hang scenario less likely than the original description implies. Fix by hiding the flag (#[arg(hide = true)] — safe, the repo's own e2e.sh passes it and hidden flags still parse) or by adding flag help text stating it is currently always-on/reserved.

**Suggested fix**: Until a real background mode ships, either hide the flag (#[arg(hide = true)]) or state "currently always runs in the foreground; this flag is reserved" in the subcommand and flag help so callers cannot infer a background default.

### [low] vapor service stop unconditionally reports {"result":"stopped"} even when nothing was installed or running

**Category**: improvement · **Where**: `core/cli/src/commands/service.rs:166` · **Review group**: cli-commands

dispatch(ServiceCommand::Stop) (core/cli/src/commands/service.rs:166-171) hardcodes DaemonLifecycleActionResult::Stopped after stop_daemon_for_termination, and the macOS installer's stop_daemon swallows all launchctl errors as best-effort (core/platform/src/service/macos.rs:227-232). Concrete behavior: `vapor service stop --json` on a machine where the service was never installed returns exit 0 with {"result":"stopped"} (human form: "service: daemon stopped"), indistinguishable from a real stop. Impact correction: the Swift app shim currently discards the stop result (VaporCLIServiceController.stopDaemon() does `_ = try actionCommand("stop")`, used only on the Quit path), and app UI state is derived from `status`/`check` which probe the installer correctly — so today the misleading output affects only direct CLI users and any future consumer that trusts the stop action result. Suggestion stands: probe installer.status() around the stop and return Unchanged (already a decoded wire value in the Swift shim) when the service was not installed or already stopped; apply the same to the stop half of Restart.

**Suggested fix**: Probe installer.status() before/after the stop and return Unchanged (or a distinct wire value) when the service was not installed or already stopped, keeping the JSON contract change coordinated with the Swift shim.

### [low] Support-bundle directory name collides silently: same-millisecond or pre-epoch timestamps merge two bundles into one directory

**Category**: improvement · **Where**: `core/cli/src/commands/support.rs:65` · **Review group**: cli-core

collect_support_bundle (core/cli/src/commands/support.rs:64-65) uses fs::create_dir_all for vapor-support-{timestamp_ms}, which succeeds when the directory already exists, and main.rs:372-375 maps a pre-epoch clock to timestamp 0 via unwrap_or(0). Any repeated timestamp (pre-1970 clock, or concurrent invocations in the same millisecond) silently merges two bundles into one directory: the second run overwrites manifest.json/vapor.json while stale artifacts from the first run (e.g., status.json from a daemon-reachable capture) linger unlisted, so the surviving manifest no longer matches the directory contents — violating its documented self-describing contract. Edge-case only; the bundle is regenerable and no sync data is affected. Fix: fs::create_dir with AlreadyExists disambiguation (counter/random suffix) and propagate an error instead of unwrap_or(0).

**Suggested fix**: Use fs::create_dir for the bundle directory and disambiguate on AlreadyExists (append a counter or random suffix), and propagate an error instead of unwrap_or(0) when SystemTime is before the epoch.

### [low] vapor --version omits the git commit, diverging from vapor version, vapord --version, and the documented contract

**Category**: bug · **Where**: `core/cli/src/main.rs:25` · **Review group**: cli-core

clap's version attribute in core/cli/src/main.rs:25 is set to vapor_daemon::build_info::VERSION only, so `vapor --version` / `vapor -V` prints "vapor 0.x.y" with no commit SHA, while `vapor version` prints "vapor 0.x.y (<sha>)" via version_string() and `vapord --version` includes the SHA. This contradicts the module doc in core/cli/src/commands/version.rs (which claims `--version` prints the same shape as `vapord --version`) and AGENTS.md §7.2 (commit SHA available in --version output). No in-repo script currently relies on `vapor --version`, so impact is limited to manual support/diagnostic workflows. Fix: pass a version string of the form "0.x.y (<sha>)" to clap (note: clap prepends the binary name, so passing the full "vapor 0.x.y (<sha>)" string would duplicate "vapor"; an owned String also requires clap's `string` feature, or emit a combined static from the daemon crate's build.rs).

**Suggested fix**: Set the clap version to the full string, e.g. `version = vapor_cli::version_string()` (clap 4 accepts an owned String), or emit a combined VERSION_WITH_COMMIT constant from the daemon crate's build.rs and use it in both places.

### [low] Support bundle drops all live captures and reports daemonReachable=false if any one of three IPC calls fails

**Category**: improvement · **Where**: `core/cli/src/main.rs:356` · **Review group**: cli-core

dispatch_support_bundle (core/cli/src/main.rs:356) requires all three IPC captures (status, diagnostics, timeline) to succeed before including any of them; the `_ => None` arm discards partial successes and error details. Since each call opens a separate connection (core/cli/src/commands/ipc.rs), a daemon that answers status() but fails timeline() (shutdown between calls, endpoint failure) causes the bundle to omit the successfully captured status/diagnostics JSON and manifest.json to record daemonReachable: false (support.rs sets daemon_reachable = live.is_some()), which is factually wrong and hides context from the maintainer the bundle is meant to help. Capture each endpoint independently, set daemon_reachable from the status call, and record per-endpoint capture errors in the manifest.

**Suggested fix**: Capture each endpoint independently (Option per artifact), set daemon_reachable when at least the status call succeeded, and record per-endpoint capture errors in the manifest so partial failures are visible instead of silent.

### [low] vapor timeline help and empty-state message falsely claim the C8-30 timeline buffer has not shipped

**Category**: bug · **Where**: `core/cli/src/main.rs:421` · **Review group**: cli-core

The `vapor timeline` subcommand help (core/cli/src/main.rs:77-78) and its empty-state message (main.rs:421-423) still claim the C8-30 in-memory timeline buffer has not shipped, but C8-30 landed in Wave 8: core/daemon/src/timeline.rs implements the bounded buffer, bootstrap.rs wires DaemonIpcService::with_timeline, and docs/tasks/core.md marks C8-30 done. Running `vapor timeline` against an idle daemon with no recorded events prints "(timeline is empty — Wave 7 ships the IPC seam; in-memory buffer lands with C8-30)", falsely telling users/support the feature does not exist instead of "no events recorded yet". Fix: update the doc comment and replace the empty-state string with e.g. "(no timeline events recorded yet)".

**Suggested fix**: Update the Timeline subcommand doc comment and change the empty-state message to something like "(no timeline events recorded yet)".

### [low] println!-based output panics on closed stdout (broken pipe) — vapor logs | head exits 101 with a panic message ✅ FIXED in this PR

**Category**: bug · **Where**: `core/cli/src/main.rs:453` · **Review group**: cli-core

The vapor CLI never handles SIGPIPE/EPIPE: Rust std sets SIGPIPE to SIG_IGN before main, so println!/print! panic when the downstream pipe reader exits early. dispatch_logs (core/cli/src/main.rs:453) is the most exposed path because `vapor logs` without --tail prints the whole vapord.logs file (fs::read_to_string in core/cli/src/commands/ipc.rs:258) in one println!; any output over the ~64 KB pipe buffer piped to `head -n 1`, `grep -m1`, or a script that closes the pipe yields "thread 'main' panicked ... failed printing to stdout: Broken pipe (os error 32)" on stderr and exit code 101 instead of a clean 0 (reproduced on rustc 1.93). Every other dispatch_* command (status --json, timeline, diagnostics, doctor) uses the same macros and is affected when its output exceeds the pipe buffer. Fix: write through a locked io::stdout() with writeln!, treat ErrorKind::BrokenPipe as success and other write errors as exit 1 — or reset SIGPIPE to SIG_DFL on unix at the top of main().

**Suggested fix**: Write output through a locked io::stdout() with writeln!, treat ErrorKind::BrokenPipe as clean success (exit 0), and map other write errors to exit 1. Alternatively restore SIGPIPE to SIG_DFL on unix at the top of main().

### [low] Explicit --token accepts empty/whitespace values that the stdin path rejects, storing a useless credential

**Category**: bug · **Where**: `core/cli/src/main.rs:537` · **Review group**: cli-core

resolve_auth_token's Some(value) branch (core/cli/src/main.rs:537-543) returns the explicit --token argv value verbatim with no validation, while the stdin path (lines 531-534) trims and rejects empty input with "no token provided on stdin". login_into (core/cli/src/commands/auth.rs:115-125) validates profile and provider but not the token, and SecretStore::set accepts empty values. Concrete failure: `vapor auth login gdrive --token "$TOKEN"` with $TOKEN unset/empty (or whitespace) succeeds, stores an empty string in the secret store, and `vapor auth status` reports gdrive as bound (bound = get().is_ok()). Note: the exact command in the original finding used `onedrive`, which is rejected as an unknown provider (supported set is filesystem/gdrive); and because the current secret store is process-local (NativeSecretStore::is_persistent() == false pre-C4-5), the stored empty token does not yet survive the CLI process — the confusing persistent "bound but broken" state fully materializes once the macOS Keychain bridge lands. Fix: trim the explicit --token value and reject empty results with the same "no token provided" error before calling login_into.

**Suggested fix**: Trim the explicit --token value and reject empty results with the same error as the stdin path ("no token provided") before calling login_into.

### [low] Doc/code mismatch: config.rs claims resource-limit/idle-boost clamping happens at load and is surfaced via load_issue; the loader does neither

**Category**: improvement · **Where**: `core/shared/src/config.rs:57` · **Review group**: shared

Doc/code mismatch in core/shared/src/config.rs (lines 57-63): the VaporConfig field docs state resource_limits values are "clamped into 1..=100 at load with a classified warning surfaced through load_issue" and idle_boost values are "clamped up at load", but load_from/into_config perform no clamping and load_issue is only ever set for file read/parse failures. Clamping actually happens at budget-resolve time in the daemon (core/daemon/src/resource_budget.rs, EffectiveBudgetConfig::resolve via clamp_limits) with a logged warning, not load_issue. Consumers of config::load_from other than the daemon therefore see unclamped values (e.g. cpuPercent: 0) despite the documented contract. Fix: either correct the doc comments to say clamping happens at daemon budget-resolve time with a log warning, or implement clamping in into_config with a warnings channel — and make code and comment agree.

**Suggested fix**: Fix the doc comments to say clamping happens at budget-resolve time in the daemon (with a logged warning), or actually clamp in into_config and report via a warnings channel — pick one and make code and comment agree.

### [low] Log redaction misses JSON-shaped secrets ("access_token": "...") — tokens can reach log files verbatim

**Category**: security · **Where**: `core/shared/src/logging.rs:236` · **Review group**: shared

redact_inline_secrets (core/shared/src/logging.rs:236) only matches equals-sign markers ("access_token=", "token=", "client_secret=", ...) and header-colon forms ("authorization:", "x-api-key:"); JSON colon-quote form ("access_token": "ya29...") matches nothing — "token_type": "Bearer" also misses the "bearer " marker because the word is quote-delimited. Any log message containing a JSON-shaped token body would therefore reach vapor_dir/logs unredacted, defeating the AGENTS.md §6 redaction safety net. Mitigating context: no current code path logs such a body — the gdrive token exchange (oauth.rs token_request) deliberately parses the response and logs only the OAuth error code/status, never the raw body, and the one place body text does flow into logged errors (classify_api_failure, truncated Drive API error bodies) does not carry tokens. This is a defense-in-depth gap, not a live leak: one careless future log line in provider/HTTP code would silently leak tokens. Fix as suggested: extend the markers to cover JSON key forms (e.g. match key names followed by [=:"] instead of requiring '=').

**Suggested fix**: Extend INLINE_SECRET_MARKERS to cover JSON key forms (e.g. "access_token\"", "refresh_token\"", "id_token\"", "client_secret\"" or a generic regex like (access|refresh|id)_token\s*["=:] ), or redact by matching the key name anywhere followed by a delimiter ([=:"]) rather than requiring '=' specifically.

### [low] Any CI env value (including CI=false or empty) silently redirects vapor_dir to cwd-relative ./.vapor, even when VAPOR_ENV=prod

**Category**: bug · **Where**: `core/shared/src/runtime_paths.rs:17` · **Review group**: shared

vapor_directory() (core/shared/src/runtime_paths.rs:17) uses env::var_os("CI").is_some(), so CI=false, CI="", or any leaked CI variable triggers the dev/CI branch, and it is OR'd ahead of the VAPOR_ENV check so even an explicit VAPOR_ENV=prod cannot override it. Because that branch resolves against env::current_dir(), the resulting vapor_dir differs per working directory: a user who exports CI=false in their shell (a common way to disable other tools' CI behavior) runs `vapor status` from two directories and gets two disjoint ./.vapor trees — config, device id, and daemon state fragment, and the CLI reports the daemon as not running while it runs against a different ./.vapor; a daemon inheriting CI with cwd=/ would target /.vapor. Note: AGENTS.md §8.5 does document a CI heuristic ("and in test/CI contexts"), so the resolution order is not undocumented as originally claimed — but the doc defines neither presence-vs-truthiness semantics nor that CI outranks VAPOR_ENV=prod. The Swift mirror (apps/macos/Sources/VaporCore/VaporPaths.swift:35, environment["CI"] != nil) has identical behavior, so a fix (honor CI only when truthy, e.g. "true"/"1", and let an explicit VAPOR_ENV value win) must update both the Rust source of truth and the Swift mirror per AGENTS.md §8.6, plus the §8.5 wording.

**Suggested fix**: Only honor CI when it is truthy (e.g. "true"/"1"), and make an explicit VAPOR_ENV value win over the CI heuristic (check VAPOR_ENV first: dev -> ./.vapor, prod -> ~/.vapor, unset+CI-truthy -> ./.vapor).

### [low] Lexical `..` normalization and lack of canonicalization mis-resolve symlinked VAPOR_DIR spellings

**Category**: improvement · **Where**: `core/shared/src/runtime_paths.rs:283` · **Review group**: shared

normalize_absolute_path pops ParentDir components lexically. If a component is a symlink, this resolves to a different directory than the kernel would: VAPOR_DIR=/home/alex/link/../data where link -> /srv/x resolves to /home/alex/data in Vapor but /srv/data for every shell/tool that touches the same spelling — Vapor silently uses a different runtime dir than the user expects. Relatedly, because nothing canonicalizes vapor_dir, two equivalent spellings (symlinked vs real, e.g. launchd plist carrying one and the user's shell the other) produce different fnv1a64 hashes in resolve_ipc_socket_location; when the path exceeds MAX_SOCKET_PATH_BYTES the daemon and CLI relocate to different <tmp>/vapor-<hash>/ sockets and the CLI reports the daemon as not running even though it is.

**Suggested fix**: After lexical normalization, attempt fs::canonicalize on the deepest existing ancestor (falling back to the lexical result) so equivalent spellings converge before the socket-path hash is computed; at minimum document that VAPOR_DIR must use one consistent spelling across surfaces.

### [low] cleanup uses pkill -f with the repo path as an unescaped regex

**Category**: improvement · **Where**: `scripts/e2e.sh:175` · **Review group**: scripts

`pkill -f "$ROOT_DIR/target/debug/vapord"` (scripts/e2e.sh:175) interprets the repo path as an extended regex. A checkout path containing ERE metacharacters defeats the match two ways: balanced metachars (e.g. `vapor(2)`, `budapest[wip]`) form a valid regex that no longer matches the literal cmdline, so pkill silently finds nothing; unbalanced metachars (e.g. a lone `[`) make the pattern uncompilable and pkill errors out, hidden by `2>/dev/null || true`. Either way, if the earlier `service uninstall` / `launchctl bootout` teardown steps failed, a crashed-launchd straggler vapord survives the --full cleanup and keeps running against a deleted sandbox. (The `.` characters in any path also make the match looser than intended.) Fix: escape the pattern before passing it to pkill -f, or match by executable path via `pgrep -x vapord` and compare each pid's executable to "$ROOT_DIR/target/debug/vapord".

**Suggested fix**: Match the literal executable path instead: iterate `pgrep -x vapord` and compare each process's executable path, or escape the pattern (`printf '%s' "$ROOT_DIR/target/debug/vapord" | sed 's/[][(){}.*+?^$|\\]/\\&/g'`) before passing it to pkill -f.

### [low] S5 hangs forever (no timeout) if the singleton lock regresses

**Category**: bug · **Where**: `scripts/e2e.sh:348` · **Review group**: scripts

S5 (scripts/e2e.sh:348) captures the second daemon via an unbounded command substitution: `second_output="$("$VAPOR_BIN" run --foreground 2>&1)"`. The scenario exists to catch a singleton-lock regression, but that exact regression makes the second daemon proceed past bootstrap (the shared SQLite state DB opens fine under busy_timeout, and bind_listener unlinks and steals the existing IPC socket rather than failing), so `run --foreground` blocks in the runtime loop forever. The command substitution then never returns: locally the harness hangs indefinitely with no FAIL and no diagnostics; on CI it burns until the 45-minute job timeout. Every other external observation in the script goes through the bounded `wait_until` helper, making this the suite's only unbounded wait. Fix by backgrounding the second daemon and using `wait_until` for its exit, or wrapping in a timeout (with a perl/python fallback since macOS lacks coreutils `timeout`), then asserting the non-zero exit and refusal message.

**Suggested fix**: Bound the wait, e.g. run the second daemon in the background and `wait_until 15` for it to exit, or use `timeout 15 "$VAPOR_BIN" run --foreground` (with a perl/python fallback since macOS lacks coreutils timeout by default), then assert the non-zero exit and refusal message.

### [low] e2e harness silently aborts and deletes the failure sandbox when a glob/grep assignment matches nothing

**Category**: bug · **Where**: `scripts/e2e.sh:529` · **Review group**: scripts

Under `set -euo pipefail`, the assignments `S15_COPY="$(compgen -G ... | head -n 1)"` (scripts/e2e.sh:529) and `deep_socket="$(grep ... | tail -n 1 | sed ...)"` (lines 397-398) abort the script with exit 1 if the pipeline matches nothing, before the guarded `[[ -n ... ]] || fail` checks on lines 530/399 can run — making those guards dead code. Because only fail() sets FAILED=1, the EXIT trap would then `rm -rf "$E2E_ROOT"` (line 180), silently deleting the sandbox the harness promises to preserve on failure, with no FAIL message or diagnostics. Note: neither empty-match path is reachable today — a cloud-only S11 conflict copy is caught loudly at line 523 because `vapor conflicts list` scans only local roots, and the S9 relocation log line is deterministically written before the daemon becomes reachable — so this is a latent harness-robustness defect that would only fire after future drift (log message reworded, conflicts-list semantics change). Fix: append `|| true` inside both command substitutions, matching the existing warning_count idiom on line 375, so the intended fail-with-diagnostics paths actually execute.

**Suggested fix**: Make these assignments failure-tolerant so the existing checks fire, e.g. `S15_COPY="$(compgen -G ... | head -n 1 || true)"` and `deep_socket="$(grep ... | tail -n 1 | sed ... || true)"`, mirroring the `|| true` already used for warning_count on line 375.

### [low] lint.sh runs the identical Swift lint command twice per invocation

**Category**: perf · **Where**: `scripts/lint.sh:18` · **Review group**: scripts

On macOS, scripts/lint.sh runs the identical Swift lint twice: line 13 runs scripts/swift/lint.sh (`swift format lint --recursive apps/macos`, swift/lint.sh:27), then line 18 runs `scripts/format.sh check`, whose Darwin branch calls scripts/swift/format.sh in check mode, executing the same `swift format lint --recursive apps/macos` (swift/format.sh:34). Every lint invocation (local and CI) pays the full-tree Swift lint twice for no added coverage. Note the trailing `format.sh check` also supplies the only Rust `cargo fmt --check` (rust/lint.sh runs clippy only), so the fix should replace it with `scripts/rust/format.sh check` or make swift/lint.sh delegate to `swift/format.sh check` rather than deleting the format check outright.

**Suggested fix**: Either drop the trailing `format.sh check` from lint.sh in favor of `rust/format.sh check` only, or make swift/lint.sh delegate to swift/format.sh check so the pass runs once.

### [low] version.sh fixture tests inherit the developer's global git config and fail on gpgsign/hooksPath machines

**Category**: bug · **Where**: `scripts/tests/version.sh:90` · **Review group**: scripts

make_repo's `git -C "$repo" commit` (line 90) — and the release commits created inside the fixtures by version.sh itself — run with the contributor's global/system git config. Concrete failures: `commit.gpgsign = true` without a usable signing key makes every fixture commit fail, so `./scripts/test.sh` (Tier 1, a required PR gate) fails on that machine for reasons unrelated to the code; a global `core.hooksPath` pointing at hooks that reject these commits does the same. The script already isolates GIT_DIR-family env vars and author identity but not config-driven behavior.

**Suggested fix**: Isolate config for the fixtures, e.g. `export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null` next to the existing unset block (git >= 2.32), or pass `-c commit.gpgsign=false -c core.hooksPath=/dev/null` on fixture git invocations. Note version.sh's own `git commit` inside the fixture also needs the env-based isolation.


### Addendum — additional verified findings (batch 2)

| Sev | Category | Location | Finding |
|---|---|---|---|
| medium | improvement | `.gitignore:55` | Gitignored VaporCore Resources directory makes a fresh clone fail `swift build` outright |
| medium | bug | `README.md:77` | README claims VAPOR_* env vars override every vapor.json key, but only 6 of 15 keys have env counterparts |
| medium | bug | `apps/macos/Sources/Vapor/AppShellViewModel.swift:479` | Config-change manager swap defeats lifecycle-operation serialization and strands the health monitor on a stale manager |
| low | improvement | `.agents/skills/vapor-debug/SKILL.md:58` | vapor-debug skill misstates timeline availability and omits two durable-DB tables |
| low | improvement | `.agents/skills/vapor-e2e/SKILL.md:107` | vapor-e2e skill's 'Known limits' describes pre-Wave-8 behavior that no longer exists |
| low | bug | `.gitignore:66` | Dead gitignore negation: !.cursor/environment.json can never re-include the file |
| low | improvement | `Cargo.toml:13` | No [workspace.dependencies] inheritance: pinned '=' versions hand-duplicated across seven manifests |
| low | improvement | `README.md:14` | README Install states Windows/Linux are 'in flight', but the roadmap classifies them as deferred/optional and not committed |

### [medium] Gitignored VaporCore Resources directory makes a fresh clone fail `swift build` outright

**Category**: improvement · **Where**: `.gitignore:55`

The entire apps/macos/Sources/VaporCore/Resources/locales/ mirror is gitignored and only materialized by scripts/locales.sh, so in a fresh checkout the Resources directory referenced by Package.swift's .process("Resources") does not exist at all. Reproduced in this worktree: `swift build --target VaporCore` fails with "error: couldn't build ... Vapor_VaporCore.bundle/Resources because of missing inputs: .../Sources/VaporCore/Resources". Concrete failure: anyone opening apps/macos in Xcode or running swift build/SourceKit-LSP indexing before running a wrapper script gets a hard, confusing build error, and IDE indexing stays broken until the out-of-band sync runs.

**Suggested fix**: Narrow the ignore to the generated JSON files (e.g. `apps/macos/Sources/VaporCore/Resources/locales/*.json`) and track a placeholder (`!.../locales/.gitkeep`) so the Resources tree always exists, or generate the mirror via a SwiftPM build-tool plugin so plain `swift build` is self-sufficient.

### [medium] README claims VAPOR_* env vars override every vapor.json key, but only 6 of 15 keys have env counterparts

**Category**: bug · **Where**: `README.md:77`

README.md:77 states that matching VAPOR_* env vars override any vapor.json key, but only 6 of the 15 documented keys (useGitIgnore, useVaporIgnore, localSyncDirectory, cloudSyncDirectory, preIgnoreRules, postIgnoreRules) have env counterparts; config.rs applies no env layer, so env vars like VAPOR_SYNC_MODE or VAPOR_PROVIDER are silently ignored. This is a documentation-vs-code mismatch (the runtime falls back to safe defaults), best fixed by documenting exactly which keys have env overrides — or intentionally adding the missing overrides, noting that an env-driven syncMode override would conflict with the AGENTS.md rule that one-way modes must never be enabled silently.

**Suggested fix**: State explicitly which keys have env-var counterparts (the six filtering/directory keys plus VAPOR_DIR/VAPOR_ENV/VAPOR_LOG_LEVEL/VAPOR_GDRIVE_*), or add the missing env overrides to the loader if per-key override is the intended contract.

### [medium] Config-change manager swap defeats lifecycle-operation serialization and strands the health monitor on a stale manager

**Category**: bug · **Where**: `apps/macos/Sources/Vapor/AppShellViewModel.swift:479`

The production `lifecycleManagerFactory` (`{ _ in AppShellViewModel.makeDefaultLifecycleManager() }`) builds a brand-new `DaemonLifecycleManager` — with a brand-new private `stateQueue` — every time `refreshDaemonLifecycleManagerForCurrentConfiguration()` runs (any `setUseGitIgnore`/`setUseVaporIgnore`/`saveIgnoreRuleSettings` save). `DaemonLifecycleManager` documents that it 'owns serialization (one lifecycle operation at a time)', but that guarantee is per-instance: `DaemonHealthMonitor` captures the original manager once at creation (line 200) and keeps it forever, so after the first config save the periodic `vapor service check` and user-initiated `vapor service install/uninstall/stop` run on different stateQueues and can execute concurrently. Failure scenario: user toggles a gitignore setting (manager swapped), later clicks 'Disable auto-launch and stop now' while a health tick is in flight -> `vapor service uninstall` and `vapor service check` CLI processes interleave; the check can observe the daemon vanishing mid-uninstall, classify it as an unexpected exit, and restart it or register a spurious crash toward crash-loop pause — daemon running again right after the user stopped it.

**Suggested fix**: Since the factory deliberately ignores configuration, stop recreating the manager on config saves (keep one instance for the app's lifetime), or if recreation is ever needed, hand the new manager to the health monitor and drain in-flight operations first.

### [low] vapor-debug skill misstates timeline availability and omits two durable-DB tables

**Category**: improvement · **Where**: `.agents/skills/vapor-debug/SKILL.md:58`

Two factual doc/code mismatches that steer a debugging agent wrong: (1) line 58 says `vapor timeline --json` is 'empty until C8-30 lands; don't be surprised' — C8-30 shipped (core/daemon/src/timeline.rs, docs/tasks/core.md:456 marked [x]), so an agent will dismiss a genuinely empty/broken timeline as expected instead of flagging it. (2) The state-DB table list at line 45 ('queue_intents, failed_intents, state_entries, schema_meta') omits `sync_index` and `tombstones` (created in core/daemon/src/state_db.rs:1074 and 1082), so an agent debugging deletion/replay or remote-index issues per this skill will not know the relevant tables exist.

**Suggested fix**: Drop the C8-30 caveat (timeline now returns real activity events) and extend the table list at line 45 with `sync_index` and `tombstones`.

### [low] vapor-e2e skill's 'Known limits' describes pre-Wave-8 behavior that no longer exists

**Category**: improvement · **Where**: `.agents/skills/vapor-e2e/SKILL.md:107`

The skill states 'The stub provider has no cloud side: the suite proves pipeline convergence ... not byte replication. Replication assertions arrive with the Wave 8 filesystem reference provider' (lines 107-110), 'Today's default provider is the inert filesystem stub' (line 43), and 'vapor timeline returns an empty list until C8-30 lands' (line 112). All three are stale: Wave 8 landed (commit 59e16fa), the default provider is now the real FilesystemProvider (core/providers/src/lib.rs:366; the stub is explicitly documented there as superseded), scripts/e2e.sh S10 already asserts byte-for-byte local→cloud→local replication (e2e.sh:406,421), and the timeline is implemented (core/daemon/src/timeline.rs, C8-30 marked done in docs/tasks/core.md:456). Concrete failure: an agent following this skill will skip replication verification ('not byte replication' claim) and treat a genuinely broken/empty `vapor timeline` as expected, missing real regressions.

**Suggested fix**: Rewrite the 'Known limits (today)' section and line 43: default provider is the filesystem reference provider with real byte replication (asserted by S10), and `vapor timeline` returns real activity events; remove the C8-30 caveat.

### [low] Dead gitignore negation: !.cursor/environment.json can never re-include the file

**Category**: bug · **Where**: `.gitignore:66`

Line 65 ignores the `.cursor` directory itself; per gitignore semantics, files inside an excluded directory cannot be re-included, so the `!.cursor/environment.json` rule on line 66 is dead. Verified: created .cursor/environment.json and `git check-ignore -v` attributes the ignore to the `.cursor` rule (line 65) — the file stays ignored. Concrete failure: a contributor commits a shared Cursor environment config believing the negation covers it; git silently skips it and the config never lands in the repo.

**Suggested fix**: Replace the pair with `.cursor/*` followed by `!.cursor/environment.json` (excluding directory contents rather than the directory allows the negation to work).

### [low] No [workspace.dependencies] inheritance: pinned '=' versions hand-duplicated across seven manifests

**Category**: improvement · **Where**: `Cargo.toml:13`

Shared external deps are hand-duplicated with exact `=` pins across all seven crate manifests (serde =1.0.228 in 5, serde_json =1.0.145 in 6, tempfile =3.27.0 in all 7, notify/md-5/sha2 in 2 each; rusqlite =0.38.0 appears twice within core/daemon/Cargo.toml for the Windows bundled override) instead of a root [workspace.dependencies] table. Bumping a pin in one manifest but missing another produces conflicting exact requirements that break workspace resolution at build time — a loud but avoidable maintenance failure, mirroring the duplicated-literal hazard CLAUDE.md §8.6 centralizes elsewhere.

**Suggested fix**: Add a [workspace.dependencies] table in the root Cargo.toml with the single pinned version per crate (and shared feature sets), and switch member manifests to `dep.workspace = true`; the daemon's Windows-only bundled-rusqlite override can stay as `features = ["bundled"]` layered on the workspace entry.

### [low] README Install states Windows/Linux are 'in flight', but the roadmap classifies them as deferred/optional and not committed

**Category**: improvement · **Where**: `README.md:14`

README says 'Windows / Linux: in flight. The CLI (vapor) will ship for Windows and Linux before the GUI apps do', and Features lists cross-platform parity under 'In flight and coming next'. docs/tasks/README.md (the roadmap source of truth) says the opposite: Waves 12+ are 'Deferred / optional', 'not because Windows/Linux is a committed deliverable', and 'Work in this bucket only starts if and when the project owner explicitly decides to ship a non-macOS surface'. The AGENTS.md README policy requires Features to stay 'aligned with real product status' and to 'not overpromise'; a user reading the current README expects Windows/Linux builds that are not planned to start.

**Suggested fix**: Soften the Install/Features wording to match the roadmap (e.g., 'planned; the portable runtime already compiles on Windows/Linux, apps/CLI distribution will follow if/when those surfaces open').


---

## 4. Comment cleanup pass

Applied directly to the working tree (validated with `./scripts/format.sh`,
`./scripts/lint.sh`, `./scripts/test.sh` — all green, 0 test failures).

- **Task-list references removed everywhere.** All `C*-*` / `M*-*` / `L*-*` task ids,
  `Wave N` / `Phase N` mentions, and `docs/tasks/*.md` pointers were removed from code
  comments across `core/*`, `apps/macos`, and `scripts/*` (≈300 comment lines in 66
  files). Sentences were reworded so the surviving comment still reads naturally and
  keeps its technical meaning (e.g. "(C8-17 deterministic race resolution)" →
  "(deterministic race resolution)"; "until Wave 12 lands" → "until Windows becomes a
  shipping surface"). References to `docs/plans/*` and `docs/architecture/*` design docs
  were intentionally kept — they are stable documentation, not task lists.
- **`unimplemented!()` / error message strings** that leaked wave ids
  (e.g. "not implemented yet (Wave 12 / C6-2)") were trimmed to the plain message.
- **Stale comments corrected** where the code had moved on: the `vapor auth login` help
  text now describes the real behavior (gdrive OAuth-PKCE flow when `--token` is
  omitted) instead of "the OAuth-PKCE flow lands later"; the `vapor timeline` help no
  longer claims the timeline is empty pending a future buffer; `AuthCommand::Login`
  docs no longer describe a pre-OAuth world; secret-store comments no longer promise a
  specific wave for the Keychain bridge.
- **Historical narration deleted or shortened**: "Replaces the Phase 2.5 timed
  simulator", "retired with M2-2 / C4-7", "Closes `core.md` C4-2" style archaeology is
  gone; where the retirement fact mattered (single-implementation guarantees) the fact
  was kept without the task id.

Note: review findings above were located **before** this cleanup, so cited line numbers
can drift by a few lines in files the cleanup touched.

---

## Verification caveats

Every finding above was confirmed by an adversarial verifier. The two batches below
did not reach a confident "confirmed" verdict and are recorded for transparency;
treat them as plausible leads, not established facts.

### Uncertain (verifier could not fully decide)

- `core/daemon/src/self_write_cache.rs:228` — **SelfWriteCache keeps one record per key, so a later write erases a pending delete echo (and vice versa), letting the daemon's own remote delete replay as a remote deletion** (claimed high bug). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist, and exhaustive searches of /tmp, $TMPDIR, the repo, and conductor directories found no copy (REVIEW.md in the repo contains no finding referencing self_write_cache.rs:228 either). Without the claim text I cannot confirm or refute it. Best-effort independent inspection of core/daemon/src/self_write_cache.rs:226-238
- `core/daemon/src/self_write_cache.rs:63` — **Self-write cache TTL (30s) is shorter than the Throttled remote-poll cadence (60s), so upload echoes expire before the feed observes them and get re-downloaded** (claimed medium bug). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist (jq exits with "Could not open file"), no variant exists in /tmp, $TMPDIR, or /var/folders, and the repo's REVIEW.md contains no finding anchored at core/daemon/src/self_write_cache.rs (its self-write-related findings target runtime.rs and executor.rs instead). I independently audited the code at the cited location
- `core/daemon/src/sync_directories.rs:121` — **Sync-root creation is a side effect of scope resolution, so read-only paths and disabled profiles create directories on disk** (claimed low improvement). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist (verified via jq error and directory listings of /tmp, /private/tmp, and $TMPDIR), and no copy exists elsewhere in the workspace, so the specific claim at core/daemon/src/sync_directories.rs:121 is unknown and cannot be traced. Independent inspection of the anchored code (resolve_local_directory, lines 108-151) sho
- `core/daemon/src/throttle.rs:140` — **Suspended relaxation dwell (1s) equals the throttle sample interval, so hysteresis out of Suspended never engages and the state flaps 0↔full caps at 1 Hz** (claimed medium bug). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist, and no copy of the finding was recoverable anywhere (searched /tmp, TMPDIR, the claude task-output dirs, and the repo — REVIEW.md contains no throttle.rs finding at all). Without the claim text there is no failure scenario to trace. Independent inspection of core/daemon/src/throttle.rs:140 (the dwell/hysteresis ch
- `core/daemon/src/workgate.rs:244` — **Hash concurrency is silently capped at read_tokens (2), making IDLE_DRAIN_HASH_WORKERS=4 unreachable and dropping to 1 while a reconcile runs** (claimed low perf). Verifier could not decide: The input file /tmp/unverified_final.json does not exist (verified: /tmp contains only claude-501, powerlog, warp_service; no copy found in TMPDIR, the repo, or nearby directories), so the actual text of finding index 36 could not be loaded and its specific failure scenario cannot be traced. Independent inspection of core/daemon/src/workgate.rs:244 (the read-token check in ensure_hash_capacity) sh
- `core/lifecycle/src/durable.rs:175` — **No cross-process locking on lifecycle.json: concurrent vapor service invocations double-count one exit, lose crash registrations, and collide on the shared temp filename** (claimed medium bug). Verifier could not decide: The finding could not be loaded: /tmp/unverified_final.json does not exist (jq exits with "Could not open file"), and no copy exists elsewhere in /tmp or the repo (REVIEW.md contains no entry for core/lifecycle/src/durable.rs). Without the claim text there is nothing to confirm or refute step-by-step. For context, I read /Users/alex/conductor/workspaces/vapor/budapest/core/lifecycle/src/durable.rs
- `core/lifecycle/src/durable.rs:176` — **Atomic-rename writes never fsync, and an empty/truncated lifecycle.json silently resets state — un-pausing a crash-looping daemon after power loss** (claimed medium bug). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist (verified with jq and a filesystem search of /tmp, $TMPDIR, and the workspace), and REVIEW.md contains no entry for core/lifecycle/src/durable.rs, so there is no claim text to trace. Independent analysis of the cited line (durable.rs:175-177, JsonFileLifecycleStateStore::save) shows the only defensible finding ther
- `core/lifecycle/src/manager.rs:192` — **Durable restore derives elapsed time from the wall clock, so a forward clock correction wipes crash history and backoff** (claimed low bug). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist (verified with jq, ls, and filesystem-wide searches of /tmp, $TMPDIR, /var/folders, and the workspace), and the repo's REVIEW.md contains no finding for core/lifecycle/src/manager.rs at all, so the claimed defect text is unknown and cannot be confirmed or refuted as stated. Independent inspection of the anchor code
- `core/lifecycle/src/manager.rs:255` — **Toggling autolaunch off resets the crash-loop guard, so an off/on toggle clears a pause without user acknowledgement** (claimed low improvement). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist (verified with jq, ls, find across /tmp, $TMPDIR, /private/tmp, /var/folders, and mdfind), and no lifecycle-manager finding at core/lifecycle/src/manager.rs:255 appears in the repo's REVIEW.md to reconstruct it from. The code at that line is `inner.crash_loop_guard.reset()` inside the auto-launch disable path of se
- `core/lifecycle/src/manager.rs:273` — **register_unexpected_daemon_exit does not set awaiting_restart, so a subsequent check double-counts the same exit** (claimed low bug). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist (verified with jq, ls, find, and mdfind), and the repo's REVIEW.md contains no finding for core/lifecycle/src/manager.rs, so the claim text is unrecoverable. Independent inspection of manager.rs:273 (register_unexpected_daemon_exit) shows the only non-test behavior worth flagging — it registers a crash without sett
- `core/lifecycle/src/manager.rs:370` — **check_daemon_health persists the registered crash only after start_daemon succeeds, so a failing restart never escalates to backoff/pause across CLI invocations** (claimed high bug). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist (jq exits with "Could not open file"), no copy exists anywhere under /tmp, $TMPDIR, or the repo, and the repo's REVIEW.md contains no finding for core/lifecycle/src/manager.rs. I read manager.rs around line 370 — it is `self.installer.start_daemon()?;` in the CrashLoopDecision::NoDelay branch of check_daemon_health
- `core/platform/src/fs_watch/fake.rs:25` — **InMemoryFsWatcher accepts missing roots and skips canonicalization, hiding native start failures and path-prefix behavior from tests** (claimed low improvement). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist (jq exits with 'Could not open file'), and searches of /tmp, $TMPDIR, /var/folders, and the repo (including REVIEW.md, which contains no fake.rs entry) found no copy. Verification requires the claimed defect and failure scenario, which are unrecoverable. For context, core/platform/src/fs_watch/fake.rs:25 is InMemor
- `core/platform/src/secrets.rs:134` — **NativeSecretStore is process-local in-memory on macOS, so the daemon can never see tokens stored by `vapor auth login`** (claimed critical bug). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist and no copy exists anywhere on disk (searched /tmp, $TMPDIR, workspace, task outputs, Spotlight), and the repo's REVIEW.md contains no secrets.rs finding. Direct verification of the cited line shows: (a) a real but minor doc/code mismatch — the NativeSecretStore doc (secrets.rs:125-127) claims construction returns 
- `core/platform/src/secrets.rs:139` — **Google Drive tokens cannot persist or cross processes: NativeSecretStore is still process-local in-memory on macOS** (claimed medium improvement). Verifier could not decide: The finding payload (/tmp/unverified_final.json) does not exist, so the exact claim could not be loaded; verification proceeded against the anchored code instead. At core/platform/src/secrets.rs:139, for_current_user() unconditionally returns Ok(Self::default()) on every OS, which does contradict its own struct doc (lines 125–127: "returns SecretStoreError::Unsupported on platforms whose real brid
- `core/platform/src/service/fake.rs:67` — **Fake-vs-native drift: fake install leaves service Stopped, native install starts it, so lifecycle SIGKILLs the freshly booted daemon on every install** (claimed medium bug). Verifier could not decide: The finding could not be loaded: /tmp/unverified_final.json does not exist (jq exits 2), no substitute findings file exists in /tmp, $TMPDIR, or the repo, and REVIEW.md (the only review artifact present) never mentions service/fake.rs or InMemoryServiceInstaller, so the exact claim at index 51 is unrecoverable and cannot be confirmed or refuted as written. Assessing the code at the cited location 
- `core/providers/src/gdrive/oauth.rs:151` — **Unparsable token-endpoint 4xx bodies are classified Transient, producing indefinite refresh retries** (claimed low bug). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist (searched /tmp, /private/tmp, /var/folders, and the repo), so there is no claim text to trace. Independent inspection of core/providers/src/gdrive/oauth.rs:151 shows token_request classifying any JSON-unparsable token-endpoint body as ProviderError::transient regardless of HTTP status; the plausible finding ("perma
- `core/providers/src/gdrive/oauth.rs:160` — **The 'OAuth token request rejected' warning always logs [REDACTED] instead of the error code** (claimed low bug). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist, and filesystem-wide searches (/tmp, /private/tmp, /var/folders, $TMPDIR, the Conductor workspace tree, Spotlight) found no copy of the file, so the claim text for index 54 is unknown. I read /Users/alex/conductor/workspaces/vapor/budapest/core/providers/src/gdrive/oauth.rs in full: line 160 is the `("oauth_error",
- `core/providers/src/gdrive/oauth.rs:163` — **Token-endpoint 4xx rate-limit errors are classified as permanent Authentication failures** (claimed medium bug). Verifier could not decide: The assigned finding could not be loaded: /tmp/unverified_final.json does not exist (jq: "Could not open file"), no similar findings file exists in /tmp, $TMPDIR, or the repo, and REVIEW.md contains no finding anchored at core/providers/src/gdrive/oauth.rs:163 (its oauth findings target lines 43, 58/60, 92-98, 138). I read oauth.rs in full: line 163 is the token-endpoint error-classification branc

### Unverified (verification agent never ran — API quota)

- `docs/architecture/data-flow.md:95` — **data-flow documents live vapor.json config reload mid-ramp, but the daemon reads config only at bootstrap** (claimed low bug). §Ceiling transitions items 3–4 describe runtime behavior on 'config reload': lowering resourceLimits mid-ramp 'immediately clamps', changing boost*Percent 'snaps ... on the next tick (no restart, no glitch)', and idleBoost.enabled flipping false 'via config reload' triggers a graceful ramp-down. In code, vapor.json is loaded exactly once at daemon startup (core/daemon/src/bootstrap.rs:83 config::l
- `docs/architecture/ipc-contracts.md:114` — **ipc-contracts documents streamed/chunked endpoints, but Timeline and Diagnostics are single bounded responses** (claimed low bug). 'Streamed endpoints (diagnostics timeline, activity events) use chunked frames; each frame is bounded independently' — and the test matrix (line 191) requires 'a single oversized timeline frame is rejected without tearing down the stream'. In core/ipc/src/protocol.rs the Timeline and Diagnostics methods return one TimelineResponse/DiagnosticsResponse frame (with a `truncated` flag on diagnostics);
- `docs/architecture/ipc-contracts.md:141` — **ipc-contracts Controls group lists a 'config reload trigger' method that has no IPC method** (claimed low bug). The Controls contract group lists 'Pause / Resume, FlushNow, excludes update, auto-launch toggle, config reload trigger'. The Method enum in core/ipc/src/protocol.rs (Status, Pause, Resume, FlushNow, Reconcile, Timeline, Diagnostics, SetAutoLaunch, UpdateExcludes) has no config-reload method, and the schema-v2 history section of the same doc (which claims v2 is 'finalisation') does not include one
- `docs/architecture/ipc-contracts.md:152` — **ipc-contracts stage list documents a DeferredReconcile stage the daemon never emits, and miscounts 'four queue-state values'** (claimed low bug). The doc says the diagnostics `stage` enum is ExecutionStage 'plus four queue-state values' and lists `DeferredReconcile` as one of them. In code, ExecutionStage (core/daemon/src/executor.rs:45) has 7 variants and the diagnostics mapping in core/daemon/src/runtime.rs:996 emits only "Retrying" or "Queued" for non-executor intents — a storm-compacted reconcile_subtree intent with a future available_a
- `docs/architecture/ipc-contracts.md:34` — **ipc-contracts references constant SCHEMA_VERSION_MIN; actual name is SCHEMA_VERSION_MIN_SUPPORTED** (claimed low bug). The Schema history section names the source-of-truth constants as `SCHEMA_VERSION_CURRENT, SCHEMA_VERSION_MIN` in core/shared/src/constants.rs::ipc. The actual constant is `SCHEMA_VERSION_MIN_SUPPORTED` (constants.rs:255). Failure scenario: a contributor following the doc greps for SCHEMA_VERSION_MIN, finds no exact match (or writes a new constant with the doc's name), causing drift in the version
- `docs/architecture/ipc-contracts.md:57` — **ipc-contracts handshake error shape omits the local_version field of IncompatibleVersion** (claimed low bug). The handshake section documents failure as `IncompatibleVersion { peer_version, required_min }`. The wire struct in core/ipc/src/protocol.rs:46-50 has three fields: peer_version, required_min, and local_version. Failure scenario: a client implemented strictly from the doc (e.g., the future Windows named-pipe client) defines a two-field struct; with strict parsing it fails to decode the error, and 
- `docs/architecture/ipc-contracts.md:71` — **ipc-contracts documents unknown-field debug logging (ipc.unknown_field) that does not exist** (claimed low bug). The forward-compatibility section states 'Unknown fields added by a newer peer are logged at debug level (`ipc.unknown_field` with the field name) and discarded.' In code, unknown fields are silently ignored by serde (no #[serde(deny_unknown_fields)], and grep finds no `ipc.unknown_field` or equivalent logging anywhere in core/ipc or core/daemon). Failure scenario: an operator debugging a version-
- `docs/architecture/sync-modes.md:86` — **sync-modes doc still says the SyncScope syncMode field 'will land with C8-59' (it has landed), and points the SyncMode enum at the wrong file** (claimed low improvement). '§Where it lives in the pipeline' says syncMode 'will be carried on the sync scope (core/daemon/src/sync_directories.rs SyncScope — the field lands with C8-59) and, once profiles land, on the per-profile resolved settings.' Both have landed: SyncScope has `pub sync_mode: SyncMode` (sync_directories.rs:16) and profiles ship with per-profile sync_mode override (config.rs ProfileConfig). Additionally
- `docs/product/status-and-goals.md:6` — **status-and-goals still claims the inert FilesystemStubProvider is the default and Google Drive is future work** (claimed low improvement). Line 6 says 'Default provider is currently the inert FilesystemStubProvider; the real provider_filesystem ships in the Phase C8 bidirectional runtime shell and GoogleDriveProvider becomes selectable later in C8.' Wave 8 landed (PR #5): select_provider_for_profile in core/providers/src/lib.rs:360 wires the real FilesystemProvider for the default 'filesystem' value and 'gdrive' is selectable; the st
- `docs/tasks/README.md:161` — **tasks README Wave 6 describes the IPC framing as 'JSON-RPC 2.0', contradicting the shipped contract** (claimed low improvement). Wave 6 (marked complete) lists 'C5-1 … C5-5 — transport decision …, JSON-RPC 2.0 framing, server-side implementation …'. The shipped wire format is explicitly not JSON-RPC: ipc-contracts.md lines 17-19 state the envelopes are 'Vapor-specific tagged enums …, not JSON-RPC', and core/ipc/src/protocol.rs implements {kind, payload}/{outcome, value} envelopes. A contributor scanning the roadmap orchestr
- `core/daemon/src/executor.rs:1379` — **Planner and download-apply paths hash entire files synchronously in one call, bypassing the slice-budget and throttle discipline** (claimed medium perf). The module goes to great lengths to chunk hashing (StreamingFileHash, HASH_STAGE_STEP_BYTES, allow_hashing gate), yet three paths hash whole files in a single blocking call with no budget or throttle gate: deletion_loses_to_local_state (line 1379, runs inside the Planner stage for every two-way ApplyRemoteDelete — despite plan_intent's own doc saying 'no hashing'), preserve_diverged_local_before_a
- `core/daemon/src/runtime.rs:1547` — **Durable-enqueue failure in flush_scheduler_to_durable_queue permanently wedges all claimed intents in Running state, silently stalling sync for those paths** (claimed high bug). flush_scheduler_to_durable_queue() first claims EVERY pending scheduler intent via claim_next() (setting each record to ScheduledIntentState::Running), then persists the batch with `self.state_db.enqueue_intents_coalesced(&batch)?`. If that DB call returns a transient error (SQLITE_BUSY, disk full, I/O error), the `?` aborts the tick before any complete_running() runs. Tick errors are non-fatal: m

