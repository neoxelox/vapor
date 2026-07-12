# Vapor Repository Review

Full-repo review covering bugs, improvements (with emphasis on sync performance), and
security issues, plus a comment-cleanup pass over the codebase.

- **Scope**: all of `core/*`, `apps/macos`, `scripts/`, `.github/workflows`, `docs/`, skills.
- **Method**: multi-agent deep review per subsystem (25 review groups); every finding
  was independently adversarially re-verified against the code before being recorded.
  Refuted candidates were discarded.
- **Severity scale**: `critical` (data loss / security compromise), `high` (real bug users
  will hit), `medium` (bug in edge cases or meaningful perf/robustness gap), `low`
  (minor issue or polish).
- **Status (2026-07-12)**: all 169 confirmed findings have been implemented. 157 were
  resolved in PR #6 / PR #8 and their entries removed from this document; the 12
  below (the architectural/performance remainder) were resolved in the follow-up
  change set and are kept for reference with their resolutions.

## Resolved in the follow-up change set

| Sev | Category | Location | Finding |
|---|---|---|---|
| medium | perf | `core/daemon/src/executor.rs:650` | All provider I/O runs synchronously on the single tick thread: 'concurrency' caps are time slicing, and provider RTT stalls the whole runtime |
| medium | perf | `core/daemon/src/executor.rs:336` | One stage transition per tick adds ~1.25 s of fixed pipeline latency to every file |
| medium | perf | `core/daemon/src/runtime.rs:1539` | Strict FIFO lease order lets a whole-scope reconcile backlog starve fresh user edits; priority classes only apply within one flush batch |
| medium | perf | `core/daemon/src/runtime.rs:1668` | Per-path serialization conflict breaks the whole admission batch: one in-flight long transfer starves all new work admissions |
| medium | improvement | `core/daemon/Cargo.toml:10` | Daemon bypasses the core/platform FsWatcher trait with its own direct notify watcher |
| medium | improvement | `core/daemon/src/fs_events.rs:11` | Daemon local fs-watch bypasses the platform FsWatcher trait and duplicates the FSEvents mapping (already drifted) |
| medium | bug | `apps/macos/Sources/VaporCore/DaemonLifecycle.swift:276` | Login-item registration failures are swallowed: UI reports 'Start at login' ON while the app will not launch at login |
| low | perf | `core/daemon/src/multi_runtime.rs:405` | tick_all runs per-profile SQLite queries and publishes a full status snapshot on every tick, even fully idle |
| low | perf | `core/daemon/src/storm.rs:96` | Storm detector clones the full event path into every ancestor directory's window on each event |
| low | perf | `core/daemon/src/auto_tune.rs:105` | Auto-tuner regression check compares absolute queue depth, which is confounded by ingest — step increases are always rolled back exactly when a deep queue needs them |
| low | bug | `core/ipc/src/client.rs:114` | Client connect() is not covered by the deadline — the timeout is applied only after the blocking connect returns |
| low | improvement | `core/shared/src/runtime_paths.rs:283` | Lexical `..` normalization and lack of canonicalization mis-resolve symlinked VAPOR_DIR spellings |

---

### [medium] All provider I/O runs synchronously on the single tick thread: 'concurrency' caps are time slicing, and provider RTT stalls the whole runtime  ✅ DONE

**Category**: perf · **Where**: `core/daemon/src/executor.rs:650` · **Review group**: sync-pipeline-e2e

All provider I/O — session.step (up to 8 MiB per blocking ureq HTTP call in the gdrive provider), begin_upload/begin_download, delete, and plan_upload's provider().stat/content_hash (executor.rs:1172/1202) — executes inline in StagedExecutor::advance on the single runtime tick thread, one step per active session per tick. upload_concurrency=4 therefore time-slices four sessions on one thread with no parallel network transfer: aggregate throughput is bounded by one synchronous chain, and a tick with 4 active sessions plus a planner stat lasts the sum of their round trips (~0.5-1 s+ at 100 ms RTT). During such a tick, debounce stabilization, tick-side fs-event draining (ingest itself continues on the watcher thread), remote polling, pause/control application, and fresh IPC status snapshots are all delayed. This is the structural ceiling on cloud sync throughput and runtime responsiveness once a real network provider (gdrive) is active.

**Suggested fix**: Move transfer sessions (and planner remote stats) onto worker threads or async tasks, keeping the workgate permits as the concurrency limiter and the tick loop as the orchestrator that harvests completions. At minimum, keep provider network calls off the same thread that drains fs events and runs debounce.

### [medium] One stage transition per tick adds ~1.25 s of fixed pipeline latency to every file  ✅ DONE

**Category**: perf · **Where**: `core/daemon/src/executor.rs:336` · **Review group**: sync-pipeline-e2e

advance_one performs at most one transition per tick, and several of those transitions do no budgeted work at all: WaitingForHash merely acquires a permit and opens the file (no hash step until the NEXT tick); WaitingForUpload merely acquires a permit and calls begin_upload (first byte moves a tick later); the tick that leases an intent doesn't run its planner because advance() executes before process_ready_queue in tick_with_inputs (runtime.rs:468/573). A small code-file save therefore costs: lease tick + planner tick + hash-open tick + hash tick + upload-begin tick + upload tick = ~6 ticks = ~1.5 s at the 250 ms cadence, on top of the 1.2 s debounce — roughly doubling the post-debounce latency for the common small-file case. Downloads pay ~3-4 ticks similarly on the remote->local path.

**Suggested fix**: Chain cheap transitions within one advance: after acquiring a permit, immediately perform the first budgeted step (open+first hash chunk; begin_upload+first transfer step), and run one advance pass over newly started intents after process_ready_queue in the same tick. The per-step byte budgets already bound the work; only the artificial tick boundaries between zero-cost transitions need removing.

### [medium] Strict FIFO lease order lets a whole-scope reconcile backlog starve fresh user edits; priority classes only apply within one flush batch  ✅ DONE

**Category**: perf · **Where**: `core/daemon/src/runtime.rs:1539` · **Review group**: runtime

flush_scheduler_to_durable_queue sorts by intent_priority_rank only *within one tick's batch* (the sort merely orders id assignment inside a single coalesced enqueue), while state_db leasing is strictly `ORDER BY available_at_ms ASC, id ASC` with no priority column (state_db.rs lease_ready_batch). Mechanism of the latency problem: a startup/cursor-expiry/user-requested whole-scope reconcile of a large tree enqueues thousands of Download/Upload intents at earlier (available_at, id) pairs; a file the user edits afterwards gets a later pair and is leased only after the entire reconcile backlog drains at planner-cap width. With tens of thousands of backlog intents and upload concurrency of 4, the user's active edit can wait hours to sync, defeating the priority-class intent ('key config and code paths enqueue before lockfile noise') for any work that spans more than one flush batch.

**Suggested fix**: Persist the priority rank as a durable column and lease with `ORDER BY priority_rank, available_at_ms, id` (or interleave: reserve one planner slot per tick for the newest non-reconcile intent so fresh edits make progress while backlog drains).

### [medium] Per-path serialization conflict breaks the whole admission batch: one in-flight long transfer starves all new work admissions  ✅ DONE

**Category**: perf · **Where**: `core/daemon/src/runtime.rs:1668` · **Review group**: sync-pipeline-e2e

In process_ready_queue (runtime.rs:1659-1668), when try_start_staged_intent returns false the intent is requeued +1s and the loop breaks, abandoning the rest of the leased batch. try_start (executor.rs) returns false for executor-full and no-planner-permit (global; break is right) but also for the per-path active_paths serialization check (not global). A re-save of a file with a long in-flight transfer creates a fresh pending row (enqueue_intents_coalesced only coalesces pending rows), which repeatedly fails the per-path check. Because lease_ready_batch orders by (available_at_ms, id) and the whole abandoned batch is requeued to the same now+1s, any intent that lands in the same lease batch behind the blocked duplicate (higher id) is pinned behind it in lockstep for the entire remaining transfer duration — e.g., a debounce-flushed burst of saves colliding with the duplicate's ready tick gets stuck for minutes — plus one lease+requeue durable write pair per pinned intent per second. Intents arriving in the ~3/4 of 250 ms ticks where the duplicate is not yet ready (requeue delay is 1 s) sort earlier and are admitted normally, so this is partial, deterministic starvation of colliding batches rather than near-total starvation of all admissions.

**Suggested fix**: Distinguish the per-path-busy case from permit/capacity exhaustion (e.g., have try_start return an enum, or check active_paths in the runtime before calling) and `continue` instead of `break` when only that one path is blocked. Optionally skip leasing rows whose path is currently active.

### [medium] Daemon bypasses the core/platform FsWatcher trait with its own direct notify watcher  ✅ DONE

**Category**: improvement · **Where**: `core/daemon/Cargo.toml:10`

vapor-daemon keeps its own notify = "=8.2.0" dependency and fs_events.rs:230 constructs notify::recommended_watcher directly for the production local-watch path, bypassing the core/platform FsWatcher trait (NativeFsWatcher) that the filesystem provider already uses for the remote root (feed.rs:25). The two notify→event translations have already drifted: the daemon splits paired renames (RenameMode::Both) into Removed(from)+Created(to) and records watcher errors, while the platform macOS impl maps all name-modify events uniformly to Renamed and silently drops errors. This conflicts with the CLAUDE.md §2/§8 intent (platform-sensitive watch code behind core/platform traits, contract-tested per OS) and leaves the platform-trait contract suite not covering the shipping local-watch path.

**Suggested fix**: Route the daemon's local watcher through vapor_platform::fs_watch (keeping the callback-discipline half in fs_events), drop the direct notify dependency from vapor-daemon, and let the trait contract suite cover the shared translation logic; if the split is intentional, document it in AGENTS.md as an explicit exception.

### [medium] Daemon local fs-watch bypasses the platform FsWatcher trait and duplicates the FSEvents mapping (already drifted)  ✅ DONE

**Category**: improvement · **Where**: `core/daemon/src/fs_events.rs:11`

core/daemon/src/fs_events.rs bypasses the vapor_platform::fs_watch trait: it imports notify directly and duplicates the watcher plumbing (root validation, canonicalization, OS-event→kind mapping) that NativeFsWatcher already provides and that the filesystem-provider changes feed consumes. The two mappings have already drifted — the daemon maps RenameMode::From→Removed / To→Created and splits paired renames (fs_events.rs:453-454), while the platform impl maps every ModifyKind::Name change to Renamed (fs_watch/macos.rs:107). AGENTS §2/§8 require engine code to consume platform traits, not OS-event APIs, and any FSEvents/notify fix must currently be made twice. Note: the divergent rename classification does not currently cause sync asymmetry, because feed.rs::normalize_watch_event ignores the event kind and re-derives Created/Removed by stat-ing the path — the drift is a latent, not active, correctness risk.

**Suggested fix**: Make core/daemon consume vapor_platform::fs_watch (extending WatchEventKind with the From/To rename distinction the daemon needs), delete the duplicate notify plumbing from fs_events.rs, and keep exactly one OS-event→kind mapping with shared tests.

### [medium] Login-item registration failures are swallowed: UI reports 'Start at login' ON while the app will not launch at login  ✅ DONE

**Category**: bug · **Where**: `apps/macos/Sources/VaporCore/DaemonLifecycle.swift:276` · **Review group**: macos-app-shell

registerLoginItemIfAvailable() (apps/macos/Sources/VaporCore/DaemonLifecycle.swift:276) catches every SMAppService.register() error and only logs a warning; setAutoLaunchEnabled(true) still returns success and AppShellViewModel.toggleAutoLaunch publishes autoLaunchEnabled = true from the Rust-persisted preference, so no error is surfaced. SMAppService.mainApp.register() throws in real conditions — most commonly "Operation not permitted" when the user previously disabled the login item in System Settings, or under MDM restriction. Failure scenario: user enables "Start at login", registration throws, toggle shows ON; after reboot the daemon LaunchAgent starts (sync continues) but the Vapor app/menubar surface never launches, and the user has no indication why. The app never checks service.status after register and never offers SMAppService.openSystemSettingsLoginItems(). Partially mitigated by bootstrapIfNeeded() retrying registration on each manual app launch. CLAUDE.md §6 requires permissioned features to degrade safely when denied; silently reporting success does not.

**Suggested fix**: Propagate a distinct outcome (e.g. `loginItemRequiresApproval`) from `setAutoLaunchEnabled`, check `service.status` in `SMAppServiceLoginItemController` after register, surface it in `AppShellState` with a hint that opens System Settings (`SMAppService.openSystemSettingsLoginItems()`).

### [low] tick_all runs per-profile SQLite queries and publishes a full status snapshot on every tick, even fully idle  ✅ DONE

**Category**: perf · **Where**: `core/daemon/src/multi_runtime.rs:405` · **Review group**: runtime-shell

Every tick_all (250 ms busy / 1 s idle cadence, forever) runs auto-tune queue_depth() SQL per profile including suspended slots (multi_runtime.rs:397-401), then unconditionally publishes aggregate_status (line 405-407), which re-runs queue_depth() + failed_depth() + intent_diagnostics() (a SELECT of up to ~100 rows) per profile plus snapshot allocations (lines 509-551) — ~4N SQLite statements per tick while fully idle, with the auto-tune depths recomputed instead of reused. Note the 1 Hz idle wakeups occur anyway from the tick loop's wait_timeout, so this is redundant per-wakeup work (SQLite churn and allocations growing linearly with profile count), not additional wakeups; publish itself is only an in-memory mutex swap.

**Suggested fix**: Publish only when state changed (dirty flag set by tick reports / control requests / throttle transitions) or at a lower fixed cadence when idle; reuse the queue_depth values computed for auto-tune inside aggregate_status instead of re-querying; skip suspended slots in the auto-tune aggregation.

### [low] Storm detector clones the full event path into every ancestor directory's window on each event  ✅ DONE

**Category**: perf · **Where**: `core/daemon/src/storm.rs:96` · **Review group**: ingest

observe_event builds a Vec<PathBuf> of all ancestors per event (directory_roots_for_path allocates one PathBuf per level), clones each ancestor again for the directory_windows entry lookup, and for each ancestor inserts a full clone of the event path into that window's path_last_seen plus runs a retain scan over the window map — roughly 2×depth PathBuf clones and depth map-scans per event on the runtime drain path. However, retention is bounded: once a directory window reaches the 200-unique-path or 600-event threshold, storm compaction absorbs subsequent events before they reach the detector, so per-window memory is capped near the thresholds and the worst case (tens of thousands of clones per burst) occurs only when a burst is spread across many directories that each stay below threshold.

**Suggested fix**: Store interned path IDs or relative-suffix hashes in path_last_seen instead of full PathBufs, reuse a scratch buffer for ancestor iteration (iterate Path::ancestors directly rather than collecting a Vec), and consider only tracking unique-path counts at the immediate parent while deriving ancestor rollups from child window aggregates.

### [low] Auto-tuner regression check compares absolute queue depth, which is confounded by ingest — step increases are always rolled back exactly when a deep queue needs them  ✅ DONE

**Category**: perf · **Where**: `core/daemon/src/auto_tune.rs:105`

The auto-tuner's regression check (auto_tune.rs:105) compares absolute durable queue depth before/after a step increase, but depth is dominated by exogenous ingest. During a sustained large ingest where enqueue outpaces drain, every +25% increase is followed by a deeper queue and is rolled back, degenerating into a 3-cycle oscillation (increase/rollback/cooldown-hold) that keeps the step near base for most of the ingest — the exact scenario the growth path targets. Symmetrically, during net drain an increase always sticks even if it hurt. The signal should measure drain rate/throughput (or depth delta net of ingest), not absolute depth.

**Suggested fix**: Judge regressions on drain rate instead of depth: track completed intents (or bytes transferred) per cycle, or compare depth delta against the ingest counter, and roll back only when throughput fell after the increase.

### [low] Client connect() is not covered by the deadline — the timeout is applied only after the blocking connect returns  ✅ DONE

**Category**: bug · **Where**: `core/ipc/src/client.rs:114` · **Review group**: ipc

`connect_with_timeout` (core/ipc/src/client.rs:114) applies the read/write deadline only after `connect_to_socket` — a plain blocking `UnixStream::connect` (core/ipc/src/transport.rs:121) — has returned, so the connect phase is unbounded. If the daemon's `vapor-ipc` accept thread dies while `IpcServerHandle` keeps the listener fd open (core/daemon/src/ipc_server.rs), clients keep landing in the kernel accept backlog (std backlog = 128); each such connection is still bounded by the 3 s handshake read timeout, but once the backlog fills, subsequent `connect()` calls on Linux block indefinitely per AF_UNIX blocking-connect semantics, and the CLI hangs forever — violating the "never hang" guarantee documented at client.rs:20-31. macOS fails fast with ECONNREFUSED on a full backlog, masking the bug on the current shipping OS; Linux is a planned but not-yet-shipping surface.

**Suggested fix**: Perform a non-blocking connect with a poll/select deadline (or connect on a helper thread joined with the timeout) so the connect phase is bounded by the same deadline as reads/writes.

### [low] Lexical `..` normalization and lack of canonicalization mis-resolve symlinked VAPOR_DIR spellings  ✅ DONE

**Category**: improvement · **Where**: `core/shared/src/runtime_paths.rs:283` · **Review group**: shared

normalize_absolute_path pops ParentDir components lexically. If a component is a symlink, this resolves to a different directory than the kernel would: VAPOR_DIR=/home/alex/link/../data where link -> /srv/x resolves to /home/alex/data in Vapor but /srv/data for every shell/tool that touches the same spelling — Vapor silently uses a different runtime dir than the user expects. Relatedly, because nothing canonicalizes vapor_dir, two equivalent spellings (symlinked vs real, e.g. launchd plist carrying one and the user's shell the other) produce different fnv1a64 hashes in resolve_ipc_socket_location; when the path exceeds MAX_SOCKET_PATH_BYTES the daemon and CLI relocate to different <tmp>/vapor-<hash>/ sockets and the CLI reports the daemon as not running even though it is.

**Suggested fix**: After lexical normalization, attempt fs::canonicalize on the deepest existing ancestor (falling back to the lexical result) so equivalent spellings converge before the socket-path hash is computed; at minimum document that VAPOR_DIR must use one consistent spelling across surfaces.

---

## Verification caveats

Every finding above was confirmed by an adversarial verifier. The leads below never
reached a confident "confirmed" verdict (verifier input was lost, or the verification
agent never ran); treat them as plausible leads, not established facts. Several have
since been independently fixed as part of other findings.

### Uncertain (verifier could not fully decide)

- `core/daemon/src/self_write_cache.rs:228` — SelfWriteCache keeps one record per key, so a later write erases a pending delete echo (and vice versa) (claimed high bug).
- `core/daemon/src/sync_directories.rs:121` — Sync-root creation is a side effect of scope resolution, so read-only paths and disabled profiles create directories on disk (claimed low improvement).
- `core/daemon/src/throttle.rs:140` — Suspended relaxation dwell (1s) equals the throttle sample interval, so hysteresis out of Suspended never engages (claimed medium bug).
- `core/daemon/src/workgate.rs:244` — Hash concurrency silently capped at read_tokens (2), making IDLE_DRAIN_HASH_WORKERS=4 unreachable (claimed low perf).
- `core/lifecycle/src/durable.rs:175` — No cross-process locking on lifecycle.json: concurrent vapor service invocations double-count one exit / collide on the shared temp filename (claimed medium bug).
- `core/lifecycle/src/durable.rs:176` — Atomic-rename writes never fsync; an empty/truncated lifecycle.json silently resets state after power loss (claimed medium bug).
- `core/lifecycle/src/manager.rs:192` — Durable restore derives elapsed time from the wall clock; forward clock correction wipes crash history and backoff (claimed low bug).
- `core/lifecycle/src/manager.rs:255` — Toggling autolaunch off resets the crash-loop guard, clearing a pause without user acknowledgement (claimed low improvement).
- `core/lifecycle/src/manager.rs:273` — register_unexpected_daemon_exit does not set awaiting_restart, so a subsequent check double-counts the same exit (claimed low bug).
- `core/lifecycle/src/manager.rs:370` — check_daemon_health persists the registered crash only after start_daemon succeeds, so a failing restart never escalates to backoff/pause across CLI invocations (claimed high bug).
- `core/platform/src/fs_watch/fake.rs:25` — InMemoryFsWatcher accepts missing roots and skips canonicalization, hiding native start failures from tests (claimed low improvement).
- `core/platform/src/secrets.rs:134` / `:139` — NativeSecretStore claimed process-local in-memory on macOS (claimed critical/medium; superseded by the shipped Keychain implementation — re-verify against current code).
- `core/platform/src/service/fake.rs:67` — Fake-vs-native drift: fake install leaves service Stopped, native install starts it (claimed medium bug).
- `core/providers/src/gdrive/oauth.rs:151` — Unparsable token-endpoint 4xx bodies classified Transient, producing indefinite refresh retries (claimed low bug).
- `core/providers/src/gdrive/oauth.rs:160` — The 'OAuth token request rejected' warning always logs [REDACTED] instead of the error code (claimed low bug).
- `core/providers/src/gdrive/oauth.rs:163` — Token-endpoint 4xx rate-limit errors classified as permanent Authentication failures (claimed medium bug).

### Unverified (verification agent never ran — API quota)

- `docs/architecture/data-flow.md:95` — documents live vapor.json config reload mid-ramp, but the daemon reads config only at bootstrap (claimed low bug).
- `docs/architecture/ipc-contracts.md:114` — documents streamed/chunked endpoints, but Timeline and Diagnostics are single bounded responses (claimed low bug).
- `docs/architecture/ipc-contracts.md:141` — Controls group lists a 'config reload trigger' method that has no IPC method (claimed low bug).
- `docs/architecture/ipc-contracts.md:152` — stage list documents a DeferredReconcile stage the daemon never emits, and miscounts 'four queue-state values' (claimed low bug).
- `docs/architecture/ipc-contracts.md:34` — references constant SCHEMA_VERSION_MIN; actual name is SCHEMA_VERSION_MIN_SUPPORTED (claimed low bug).
- `docs/architecture/ipc-contracts.md:57` — handshake error shape omits the local_version field of IncompatibleVersion (claimed low bug).
- `docs/architecture/ipc-contracts.md:71` — documents unknown-field debug logging (ipc.unknown_field) that does not exist (claimed low bug).
- `docs/architecture/sync-modes.md:86` — says the SyncScope syncMode field 'will land' (it has landed) and points the SyncMode enum at the wrong file (claimed low improvement).
- `docs/product/status-and-goals.md:6` — still claims the inert FilesystemStubProvider is the default and Google Drive is future work (claimed low improvement).
- `docs/tasks/README.md:161` — describes the IPC framing as 'JSON-RPC 2.0', contradicting the shipped contract (claimed low improvement).
- `core/daemon/src/executor.rs:1379` — Planner and download-apply paths hash entire files synchronously in one call, bypassing the slice-budget and throttle discipline (claimed medium perf).
- `core/daemon/src/runtime.rs:1547` — Durable-enqueue failure in flush_scheduler_to_durable_queue permanently wedges all claimed intents in Running state (claimed high bug).
