# Vapor — Full Repository Review

Reviewer: Claude (automated deep review)
Date: 2026-07-03
Scope: all of `core/*`, `apps/macos`, `scripts/`, `.github/workflows`, `docs/` (specs, plans, architecture), cross-checked against `AGENTS.md`.

> **Status update (same day):** a follow-up hardening pass addressed the
> findings below. Fixed: F-DAEMON-1…5, F-DAEMON-8…18 (except the F-DAEMON-6
> executor scaffolding, which is Phase-3 work by design, and F-DAEMON-7,
> mitigated by mapping rename halves/pairs to delete+create), F-IPC-1…4,
> F-DB-1, F-PROV-1, F-LIFE-1/2, F-CLI-1…4, F-APP-1/2/5 (partially — see
> below), F-OPS-1…4, and F-DOC-1…5. Deliberately deferred (documented as
> planned instead of fixed): the app-side IPC client + daemon health tick
> (F-APP-3) and durable cross-process crash-loop counters (F-APP-4) — both
> are feature work tracked for the M-waves, and the policy doc no longer
> claims them as shipped. F-PLAT-1 (Keychain-backed secret store) remains
> Wave-5/C4-5 work; the CLI's honest `is_persistent` warnings stay. See
> `CHANGELOG.md` [Unreleased] for the change-by-change record.
>
> **Owner decisions (post-review):** the pre-commit hook keeps the full
> `clean → lint → test → build` pipeline by design (the hook *is* the
> agentic feedback loop; only the worktree-resolution fix from F-OPS-3 was
> kept). The Tier-2 perf gate runs **only** on release — no nightly
> schedule — and the testing/CI docs now say so (supersedes part of
> F-OPS-1). The root README intentionally documents the full designed
> configuration surface docs-first, including Wave-8 keys, rather than
> only runtime-recognized keys (supersedes part of F-DOC-2/3).

Severity legend:

- **P0 — Bug (user-visible / correctness)**: behavior is wrong today, or a stated guarantee is violated.
- **P1 — Bug (latent / robustness)**: wrong under realistic-but-less-common conditions, or will bite as soon as the next planned wave lands on top of it.
- **P2 — Design / spec inconsistency**: code and docs (or two surfaces) disagree; needs a decision.
- **P3 — Improvement / performance / hygiene**: works, but could be meaningfully better.

Line numbers refer to the state of branch `nairobi` at the time of review.

---

## 0) Executive summary

The codebase is unusually disciplined for a pre-GA project: clocks are injectable, monotonic-vs-wall-clock is handled carefully, permissions are tightened, redaction exists at every persistence boundary, and the test suite genuinely covers behavior. Most of what follows is fixable in-place.

The most important findings:

1. **`vapor pause` doesn't pause anything** — `RunState::Paused` is never consulted by the tick loop; the daemon keeps leasing, hashing, uploading and reconciling while reporting "Paused" (F-DAEMON-1).
2. **Reconcile pause duplicates durable intents** — every checkpoint-pause creates one extra durable row for the same root; under a startup barrier with an active user this compounds every tick (F-DAEMON-2).
3. **Throttle dwell delays *escalation*, not just relaxation** — a machine that enters low-power mode or critical thermal pressure keeps full-speed caps for up to 5 s (F-DAEMON-3).
4. **Crash-loop backoff schedules diverge between Rust and Swift** — the promised parity test file does not exist, and the two implementations disagree by exactly one crash (F-LIFE-1).
5. **`vapor config set` writes keys the daemon never reads** — the daemon resolves sync scope and ignore rules from environment variables only, and the LaunchAgent is installed with an empty environment; only `autoLaunch` round-trips (F-CLI-1).
6. **No single-instance guard for the daemon** — two daemons can run against the same `vapor_dir`; the second silently steals the IPC socket; the doc comment claiming WAL-lock detection is false (F-DAEMON-4).
7. **Overflow-dropped fs events are counted but never repaired** — no reconcile is scheduled when the ingest buffer drops events, which is a "never lose intent state" violation once real sync lands (F-DAEMON-5).

---

## 1) Daemon core (`core/daemon`)

### F-DAEMON-1 (P0): Pause is cosmetic — the runtime does no less work while "Paused"

`runtime.rs:359-372` handles the IPC pause request by calling `set_run_state(RunState::Paused, …)`, which only mutates the `StatusSnapshot`. Nothing in the tick path consults `run_state`:

- `tick_with_inputs` (`runtime.rs:175-232`) still releases deferred reconciles, drains pending intents, stabilizes events, flushes to the durable queue, advances the staged executor, and leases new work.
- Neither `process_ready_queue` nor `StagedExecutor::advance` nor the workgate check `RunState`.

So `vapor pause` returns `accepted: true`, `vapor status` shows `Paused`, and the daemon continues doing exactly what it did before. Today the executor is a stub so the harm is invisible, but the CLI command and the IPC contract (`Pause — stops admitting new work`, `core/cli/src/main.rs:69`) are already shipped and already lie.

**Fix**: gate at minimum `process_ready_queue` and `staged_executor.advance`'s new-stage acquisition on `run_state == Running` (keep ingest/debounce running so intent state is never lost, per the product invariants). Add an integration test: pause → tick with ready intents → assert `leased_intents == 0`.

### F-DAEMON-2 (P1): Checkpoint-paused reconciles are double-tracked and multiply in the durable queue

When a running reconcile is paused, two things happen to the *same* logical intent:

1. `ReconcileController::checkpoint` → `requeue_claimed_reconcile` (`reconcile.rs:188-197`) re-inserts the root as *pending in the in-memory scheduler*.
2. The runtime then calls `requeue_runtime_intent` (`runtime.rs:202-208`), re-pending the *existing durable row*.

On the next tick, `flush_scheduler_to_durable_queue` (`runtime.rs:494-512`) claims **every** pending scheduler intent — including the re-queued reconcile — and calls `state_db.enqueue_intent`, which never dedupes (only `enqueue_startup_reconcile_intent` dedupes). Result: a brand-new durable row for the same root on every pause. Each duplicated row is later leased, run, and completed independently, so a reconcile that pauses N times runs ~N extra full times.

Worst case: during the startup reconstruction barrier with an active user, the root reconcile is started and checkpoint-paused every 250 ms tick; each pause adds one duplicate row (~4 rows/second for up to the 60 s barrier deadline), leaving hundreds of duplicate whole-root reconcile intents to execute serially afterwards, each an fsync'd insert.

**Fix**: pick one owner for a paused reconcile. Simplest: don't re-insert into the scheduler on pause (the durable row is the source of truth and is re-upserted into the scheduler when re-leased at `runtime.rs:547-551`), or make `flush_scheduler_to_durable_queue` skip `ReconcileSubtree` intents that already have a pending/leased durable row for the same path (a `WHERE path_text = ? AND state IN (…)` dedupe like the startup variant).

### F-DAEMON-3 (P1): Throttle min-dwell blocks *upshifts* to more conservative states

`ThrottleController::evaluate` (`throttle.rs:189-201`) holds `prev_state` whenever the dwell window for `prev_state` hasn't elapsed — regardless of direction. Consequences with the shipped constants:

- Machine is `Light` (dwell 5 s). User enables Low Power Mode → controller *should* go `Suspended` immediately; instead it holds `Light` for up to 5 s and keeps hashing/uploading.
- Same for `ThermalPressure::Critical` or a 90 % CPU spike arriving inside a `Light`/`Throttled` dwell.

The constants' own doc comments (`constants.rs:181-192`) describe dwell as an anti-*downshift* oscillation guard ("staying suspended for an extra second is never harmful"), and the product invariant is "defer under pressure". Escalation should bypass dwell; only relaxation (toward `IdleDrain`) should dwell.

**Fix**: in the hysteresis branch, hold only when `throttle_state_rank(selected.state) < throttle_state_rank(prev_state)` fails — i.e. `next = max_by_rank(selected, dwell-held prev)`. Add a test: enter `Light`, 1 s later feed `low_power_mode: true`, assert `Suspended` immediately.

### F-DAEMON-4 (P1): No single-instance enforcement; second daemon steals the socket; doc comment claims otherwise

- `core/cli/src/commands/run.rs:4-7` claims: *"Exits non-zero if another daemon is already attached to this `VAPOR_DIR` (detected via the SQLite database's WAL lock)."* That detection does not exist. SQLite in WAL mode with `busy_timeout` happily allows multiple connections; `DurableStateDb::open` takes no exclusive lock.
- `bind_listener` (`core/ipc/src/transport.rs:88-98`) unconditionally deletes an existing socket file before binding. If a daemon is already running, a second daemon silently unlinks the live daemon's socket and takes over IPC, while both daemons process the same queue (double leases are prevented per-process only by the `state = pending` check, but both loops interleave and both write to the DB).
- The `ListenerHandle::drop` then removes the *new* daemon's socket when the old process exits — a stale-drop race that leaves the surviving daemon unreachable.

**Fix**: take an exclusive advisory lock (e.g. `flock` on `<vapor_dir>/vapord.lock`, or `PRAGMA locking_mode = EXCLUSIVE` on the DB) at startup and exit with a clear error when held. Only remove a pre-existing socket after connecting to it fails (stale check). Fix or delete the misleading doc comment.

### F-DAEMON-5 (P1): Dropped ingest events are counted but never repaired

`BoundedFsEventRecorder::record_event` (`event_intents.rs:789-804`) drops events once the incoming buffer holds `max_pending_paths` (20 000) records and only increments `dropped_incoming_events`. Nothing ever reads that counter to schedule a catch-up reconcile; the changes represented by those events are simply lost until the next daemon restart (startup reconcile) or a manual `vapor reconcile`.

This is precisely the case the storm/compaction machinery exists for — but the safety net has a hole at the outermost layer. Under the "never lose intent state" core guarantee this is a bug even pre-GA.

**Fix**: on the first drop (or when `dropped > 0` during `drain_incoming_into_maps`), enqueue a watch-root `ReconcileSubtree` intent (deferred like a storm reconcile) and reset the counter.

### F-DAEMON-6 (P2): The staged executor is time-based simulation, not real work

`StagedExecutor` (`executor.rs`) advances Planner → Hash → Upload stages purely by elapsed wall time (`stage_duration` = one tick, 250 ms) and then `complete_leased`s the intent. No file is read, no hash computed, no provider call made; `FilesystemStubProvider` is inert. Intents therefore "complete" and are **deleted from the durable queue** without any effect.

This is documented scaffolding, but two consequences are worth recording:

- Any durability/e2e claim ("local→remote propagation" in the test matrix) currently exercises timing plumbing only.
- When real work lands, the current shape — permits held across ticks, stage completion checked once per tick — quantizes all work to tick granularity; a 1-file upload takes ≥ 3 ticks (750 ms) of latency even when idle. Consider event-driven completion callbacks rather than tick-polling when P3 lands.

### F-DAEMON-7 (P2): Rename events lose their pairing — `Rename` intents are unactionable as stored

`map_event_kind` (`fs_events.rs:308-318`) maps `Modify(Name(_))` to `FsEventKind::Renamed` per path, and notify's rename events (which carry `[from, to]` path pairs and rename cookies) are split into independent per-path records (`record_callback_result` iterates `event.paths` individually). By the time a `PendingIntentKind::Rename` reaches the durable queue it is a single path with no counterpart:

- For the *source* path (which no longer exists) an upload-style pipeline can do nothing.
- For the *destination* path the correct action is an upload, and `requires_hash` already treats Rename like Upload — so the Rename kind adds no information today.

When the real executor lands, server-side rename (a capability `GDRIVE_MVP` advertises) is impossible without the from→to pairing. Either capture the pairing at the callback (notify exposes `RenameMode::From`/`To` and tracker ids) and store both paths in the intent, or drop `Rename` down to Delete+Upload explicitly and remove the dead `supports_server_side_rename` path until it's real.

### F-DAEMON-8 (P2): Directory storm thresholds roll up to every ancestor, making whole-root compaction far too easy

`StormDetector::observe_event` (`storm.rs:82-132`) records each event in a window for **every ancestor directory** up to the watch root. The watch-root window therefore accumulates *all* events in the tree, and `directory_event_count_threshold` (600 events / 2 s) or `directory_unique_paths_threshold` (200 paths / 2 s) will trip **at the watch root** for any moderately parallel workload spread across unrelated subdirectories (e.g. a build touching 600 files anywhere). The deepest triggering directory wins, but when no single subdirectory is hot, the root is the one that trips — compacting the *entire* watch scope into one deferred reconcile and discarding all granular pending events.

Meanwhile the dedicated `global_pending_event_count_threshold` is 5 000 — clearly intended to be the "whole root" bar. The per-directory thresholds were presumably meant to apply per directory (or at least per subtree with depth weighting), not cumulatively at every ancestor.

**Fix option**: count an event only in its immediate parent's window (and let `deepest_full_subtree_for_path`-style logic escalate), or scale thresholds by depth, or exempt the watch root from the per-directory thresholds so only the explicit global threshold can compact the root.

### F-DAEMON-9 (P2): Deferred reconciles can be starved indefinitely by a busy subtree

Every event absorbed by a compacted subtree calls `refresh_compacted_subtree_reconcile` → `schedule_deferred_reconcile` (`event_intents.rs:568-645`), which pushes `available_at` forward (`existing.available_at.max(observed_at + 30 s)`). A subtree that never goes quiet for 30 s never reconciles, and because the compaction boundary swallows all events, no sync happens for that subtree at all — indefinitely. "Defer until quiet" is a sound default, but there is no upper bound. Consider a max-deferral cap (e.g. force release after N minutes regardless of continued churn, throttle permitting).

### F-DAEMON-10 (P1): One inconsistent DB row kills the whole daemon

`StagedExecutor::advance` returns `Err(InvalidIntentState)` when `complete_leased` affects 0 rows (`executor.rs:191-196`), and `requeue_runtime_intent` does the same (`runtime.rs:634-650`). These propagate out of `tick`, and `run_forever` (`runtime.rs:137-147`) exits the loop on the first `Err`, i.e. `vapord` terminates (exit 1 in `main.rs:124-130`). A single unexpected row state — after e.g. a manual DB edit, a bug elsewhere, or a future concurrent writer — takes down background sync entirely (and with `KeepAlive=false`, launchd will not restart it).

**Fix**: classify tick errors. Row-level inconsistencies should log + drop that intent (or move it to `failed_intents`) and continue; only structural failures (schema mismatch, I/O) should abort the process.

### F-DAEMON-11 (P3): Fixed 250 ms tick loop wakes the daemon forever, even when fully idle

`run_forever` sleeps `tick_interval` (250 ms) unconditionally. An "invisible-first, low-impact" daemon that wakes 4×/s for life (plus a 1 s throttle sample and a status publish per tick) will show up in powermetrics idle-wakeup counts. There is no event-driven wakeup: fs events, IPC control requests and retry timers all wait for the next poll.

**Improvement**: drive the loop with a condvar/channel signalled by (a) the fs-event recorder, (b) `RuntimeControl`, and (c) the earliest pending timer (`available_at`, debounce deadline), with the 250 ms tick as an upper bound only while work is pending. When queue depth is 0 and no debounce is pending, sleep until signalled.

### F-DAEMON-12 (P3): Per-intent SQLite writes are fsync-bound; batch them

- `flush_scheduler_to_durable_queue` does one `INSERT` (implicit transaction → fsync with `synchronous=FULL`) per intent; the guardrail test itself documents ~3 s for 150 intents on CI (`runtime.rs:946-951`).
- `lease_ready_batch` issues one `UPDATE` + one `SELECT` per intent inside its transaction (`state_db.rs:306-319`); a single `UPDATE … WHERE id IN (…) RETURNING` (or one prepared statement reused) would do.
- Blocked intents are re-leased and re-queued (2 writes) every 250 ms tick while waiting (`runtime.rs:562-585`), e.g. a reconcile waiting for IdleDrain burns ~8 writes/s indefinitely. Requeue with a coarser delay (1–5 s or the next throttle re-evaluation) instead of `now + tick_interval`.
- With WAL, `PRAGMA synchronous = NORMAL` is the standard durability/perf point (loses at most the last transaction on power loss, never corrupts); given at-least-once semantics plus the startup reconcile, `FULL` is arguably paying fsync cost for a guarantee the design doesn't need. Worth a deliberate decision either way (`state_db.rs:657-665`).

### F-DAEMON-13 (P3): `recover_leased` resets `attempt_count` for stale leases

`state_db.rs:495-524`: stale leases (older than 15 min) are recovered with `attempt_count = 0`, erasing backoff history — an intent that failed 9 times before the crash restarts its exponential schedule from scratch after every daemon restart. Fresh leases keep their count, so the two recovery paths are inconsistent. Also note `recover_leased` runs only at startup: a lease that goes stale *during* a run (e.g. an executor bug drops an execution) is never recovered until the next restart. Consider a periodic in-run recovery sweep.

### F-DAEMON-14 (P3): `enqueue_intent` never dedupes per path

Beyond the reconcile case (F-DAEMON-2): a path modified on tick N (flushed durably) and modified again on tick N+3 produces two durable Upload rows that both run. Harmless for idempotent uploads but doubles work under sustained editing. The scheduler already coalesces in-memory; the durable layer could do a `pending`-state upsert per (path, kind) like `enqueue_startup_reconcile_intent` does.

### F-DAEMON-15 (P3): Ignore rules are loaded once and never refreshed

`EventPathFilter::for_watch_root` walks the tree and compiles all `.gitignore`/`.vaporignore` rules at watcher start (`path_filter.rs:121-171`; called from `fs_events.rs:122-125`). Edits to any ignore file after startup are not observed — the filter is immutable behind an `Arc` inside the watcher callback. Users editing `.vaporignore` will see no effect until daemon restart, with no hint. (The docs/testing-strategy list "config reload mid-work" as an integration scenario; nothing implements reload.)

Also perf: `should_ignore` runs every compiled matcher for every rule on every event, inside the fs-watch callback thread. Fine for default rule counts, but a large monorepo with hundreds of nested `.gitignore` files could make the callback measurably slower — consider the `ignore` crate's tree-aware matcher or grouping globs into a `GlobSet` per action-run.

### F-DAEMON-16 (P3): Storm-window pruning is O(all windows) on every event

`StormDetector::observe_event` calls `prune_inactive_windows` on **every** event (`storm.rs:115-130`), which iterates all directory windows and retains each one's path map. During a burst with many active directories this is quadratic-ish in the callback-adjacent path (it runs on the runtime thread inside `with_mut_state`, so it stalls the tick). Prune lazily (only windows touched) or on a coarse timer.

### F-DAEMON-17 (P3): `vapord` always runs with static "idle" metrics

`main.rs` builds the runtime with `StaticMetricsSampler::default()` (via `DaemonRuntime::start`), which reports *idle, plugged-in, cool, no user activity* forever. The whole throttle controller is therefore inert in production today: the daemon believes it is always in `IdleDrain`. `core/platform::NativePlatformMetricsSampler` exists but also forwards to a static snapshot, and it is not wired into `vapord` anyway. Expected pre-Wave-4-follow-up, but worth calling out: none of the low-impact behavior is actually active in a shipped binary, and `ThrottleInputsSnapshot` (platform) is missing the `thermal_pressure` / `disk_pressure` fields that `ThrottleInputs` (daemon) has despite the comment claiming they are "structurally identical" (`core/platform/src/metrics.rs:14-29`).

### F-DAEMON-18 (P3): Misc smaller items

- `evaluate_startup_barrier` uses wall-clock `SystemTime` for the 60 s barrier deadline (`runtime.rs:419-441`) while every other timing decision was deliberately moved to the monotonic clock (C2-3). A backwards NTP step extends the barrier arbitrarily.
- `peek_next_ready_kind` only inspects the single head row (`runtime.rs:529-534`); a reconcile queued behind one upload doesn't get the +1 lease bonus. Cosmetic given requeue, but the batch-limit heuristic is easy to confuse — a comment or a kind-aware query would help.
- `PendingIntentFlags`/`FsEventKind` derive `Clone` and are passed by `&` then `.clone()`d in hot paths (`event_intents.rs:326-329`); `FsEventKind` is a fieldless enum — make it `Copy` and drop the clones.
- `scheduler::upsert_intent_with_metadata` replaces `record.kind` wholesale ("latest wins", `scheduler.rs:224`). Delete→Create sequences correctly become Upload, but note a `ReconcileSubtree` pending intent for a path can be downgraded to `Upload` if a plain event for the same path arrives (possible for the subtree root itself, e.g. a `mkdir`/attribute event on the compacted root) — the compacted boundary then waits on `compacted_subtree_requires_follow_up_reconcile`, which checks `intent_map`, *not* the scheduler, so the boundary is never cleared until another reconcile is scheduled. Low likelihood, but the kind-overwrite rule deserves an explicit carve-out for reconcile intents.
- `sync_directories`: an env var set to the empty string disables local sync (`resolve_path("") → None → daemon idle`) while an empty *cloud* value falls back to the default — inconsistent treatment of "explicitly empty" (`sync_directories.rs:48-104`).
- Tests in `path_filter.rs` / `sync_directories.rs` hand-roll temp dirs with PID+timestamp names instead of `tempfile::TempDir` used everywhere else (and mandated by `docs/architecture/testing-strategy.md`).

---

## 2) IPC (`core/ipc`, `core/daemon/src/ipc_*`)

### F-IPC-1 (P2): Mid-frame client death is classified as a clean disconnect

`read_frame` maps *any* `UnexpectedEof` to `FrameError::UnexpectedEof`, and `serve_connection` treats that as a clean close (`server.rs:151-157`) — including EOF halfway through a length prefix or payload. Harmless today, but it means protocol-level truncation is indistinguishable from polite hangup in logs/diagnostics. Distinguish "EOF at frame boundary" (clean) from "EOF mid-frame" (error) by tracking whether any prefix bytes were read.

### F-IPC-2 (P2): `ErrorBody::PayloadTooLarge` is defined but never sent

An oversized client frame kills the connection via `FrameError::OversizedFrame` (the serve loop propagates it as an error) instead of answering with the documented `PayloadTooLarge` error response (`protocol.rs:141-142`). Either send the error then close, or remove the variant from the contract.

### F-IPC-3 (P3): Socket permission race + parent dir perms

`bind_listener` binds first, then `set_permissions(0o600)` (`transport.rs:98-106`). Between those two calls the socket has umask-default perms (typically 0755 — connectable by any local user). Also `ipc_server::spawn` creates the socket's parent with `fs::create_dir_all` (`ipc_server.rs:56-58`) rather than `ensure_private_directory`, so a fresh `vapor_dir` created via this path is world-readable until something else tightens it. Bind inside a pre-created 0700 directory (that alone closes the race) or `chmod` before `listen` via a socket builder.

### F-IPC-4 (P3): Unbounded connection threads, no server-side read timeout

`ipc_server.rs:65-85` spawns a thread per connection with no cap and no timeout; a local process that connects and stays silent parks a thread forever. Same-uid only (after F-IPC-3 is fixed), so not a security issue, but a leak. Consider a small thread pool or per-connection read deadline.

- Nit: `write_response_or_log` (`server.rs:193`) doesn't log anything — rename or log.
- Nit: `StatusResponse.daemon_id` doc comment says "Daemon-side schema-version stamp" (`protocol.rs:162-166`) — it's an identity string, not a version; the actual value is `vapord/<version>` from one call site and `vapord/<schema>` from another (`server.rs:146` vs `ipc_service.rs:100`) — pick one format.
- Nit: client per-syscall timeout means a peer dribbling 1 byte per 2.9 s can stall the CLI arbitrarily long despite the 3 s "deadline" (`client.rs:100-117`); acceptable for a local trusted daemon, but the L3-7 comment overstates the guarantee.

---

## 3) Durable state (`core/daemon/src/state_db.rs`)

(Also see F-DAEMON-2/4/12/13/14.)

### F-DB-1 (P3): Schema-version check happens *after* `CREATE TABLE IF NOT EXISTS`

`migrate_schema` executes the v3 DDL batch before reading `schema_meta` (`state_db.rs:667-722`). Against a v1/v2 file the DDL happens to be compatible enough not to error, and the transaction rolls back on mismatch, so there is no corruption — but the ordering is fragile: any future DDL that conflicts with an old layout will produce a confusing SQLite error instead of the clean `SchemaVersionMismatch`. Read the version first, then create.

### F-DB-2 (P3): WAL sidecar files are not permission-tightened

`ensure_private_file` runs on `vapor.sqlite` only; `-wal`/`-shm` inherit the DB file's mode via SQLite so this is *probably* fine on macOS, but a startup `chmod` sweep of `state/` (or `ensure_private_directory` on the state dir, which is already 0700 — verify it is actually created via `ensure_private_file`'s parent path, it is) would make the intent explicit. Low priority.

---

## 4) Providers (`core/providers`)

### F-PROV-1 (P2): `Provider` trait shape won't survive contact with reality

The trait is `name/capabilities/poll_allowed/ensure_cloud_sync_directory` with `Result<(), String>` errors. Points to settle before P3 (Google Drive) lands:

- `String` errors conflict with AGENTS.md §8 "explicit error enums; classify transient vs permanent" — the retry machinery (`RetryFailureKind`) already exists and the provider is where failures originate; the trait should speak that taxonomy natively.
- All methods are synchronous while the engine is thread+tick based; decide sync-blocking-in-worker vs async now, because it shapes the executor rewrite (F-DAEMON-6).
- `ensure_cloud_sync_directory`'s default impl logs "Ensuring cloud sync directory" and returns `Ok(())` — a *default trait impl that silently no-ops a safety-relevant operation* is a footgun; make it required or make the default return `Unsupported`.
- `GoogleDriveProvider` exists only to carry a name and capabilities; it is dead code that shows up in `vapor auth` as a valid provider (`SUPPORTED_PROVIDERS`) though nothing can use it.

---

## 5) Platform layer (`core/platform`)

### F-PLAT-1 (P2): Secret storage on the shipping OS is process-local, contradicting AGENTS.md §1/§6

`NativeSecretStore` on macOS is an in-memory map (`secrets.rs:122-163`). AGENTS.md declares secret storage part of the feature-parity invariant set that "must be delivered via the matching core/platform trait implementation" on every shipping OS, and macOS ships today. The CLI mitigates honestly (`is_persistent` + warnings), and Wave 5/C4-5 tracks it — but as written the repo is in violation of its own non-negotiable, and `vapor auth login` is a no-op across restarts. Worth either shipping the Keychain bridge before any auth-consuming feature, or explicitly amending AGENTS.md's wording for the pre-Wave-5 window.

### F-PLAT-2 (P3): `launchctl` status parsing is heuristic

`status()` greps `launchctl print` for `"state = running"` / `"pid = "` (`service/macos.rs:221-238`). Output format of `launchctl print` is explicitly not API-stable per Apple. Fine pre-GA; consider `launchctl list <label>` (parsable) or `SMAppService` status via the app. Also `install_and_enable` ignores `bootout` failures silently — right call — but `enable` before `bootstrap` will fail spuriously on macOS versions where `enable` requires the service to be known; you already handle ordering correctly per current macOS, just noting the fragility that motivated the policy doc.

### F-PLAT-3 (P3): `ServiceStatus::CrashLoopPaused` is unreachable

No installer ever returns it (`service/mod.rs:45-56`); the crash-loop state lives in `DaemonLifecycleManager`, which `vapor service status` does not consult — so the CLI can never display the paused-for-crash-loop state the enum promises. Wire `dispatch(Status)` through the manager (`is_in_crash_loop_pause`) or drop the variant.

---

## 6) Lifecycle (`core/lifecycle`) and Swift parity

### F-LIFE-1 (P0-parity): Rust and Swift crash-loop schedules disagree by one crash; promised parity tests don't exist

- Rust (`crash_loop.rs:106-126`): crash count `<= delay_starts_after_failures` → `NoDelay`; the schedule for the default policy is *crash 1 → NoDelay, crash 2 → 2 s, crash 3 → 4 s, crash 4 → 8 s, crash 5 → Paused*.
- Swift (`DaemonLifecycle.swift:140-158`): `exponent = count - delayStartsAfterFailures; exponent >= 0 → backoff(base·2^exponent)`; the same default policy gives *crash 1 → 2 s, crash 2 → 4 s, crash 3 → 8 s, crash 4 → 16 s, crash 5 → Paused*.

Both modules claim "verbatim port … same exponential schedule", and `crash_loop.rs:8-10` cites parity tests at `core/lifecycle/tests/crash_loop_parity.rs` — **that file does not exist** (`core/lifecycle` has no `tests/` directory). The Swift unit tests (`DaemonLifecycleManagerTests.swift:78-79, 138-139`) lock in the Swift schedule; the Rust unit tests lock in the different Rust schedule. Since both surfaces currently manage the same daemon on macOS, the effective backoff depends on which surface observed the crash.

**Fix**: decide the canonical schedule (the Rust doc comment's "first crash is free" reading matches the field name better), align the other implementation, and actually add the cross-language parity test the comment advertises.

### F-LIFE-2 (P3): `JsonFileAutoLaunchSettingStore` hand-rolled JSON has sharp edges

- `parse_auto_launch` matches the literal `"autoLaunch"` anywhere — including inside a string value of an unrelated key.
- `upsert_auto_launch` on a file with no `}` silently produces invalid JSON (`rfind('}').unwrap_or(len)` path).
- Concurrent writers (Swift app vs CLI, both do read-modify-write-rename with different serializers) can drop each other's key updates; there is no file lock. Same applies to `core/cli::config::set` vs the Swift store.

Given `serde_json` is already a workspace dependency (used by the CLI config command for exactly this file), the hand-rolled parser buys nothing — use `serde_json` here too, and consider a `.lock` or O_EXCL temp-name scheme for cross-process writes.

---

## 7) CLI (`core/cli`)

### F-CLI-1 (P0): Most `vapor config` keys are written but never consumed by the daemon

The daemon resolves its sync scope and filter options **exclusively from environment variables** (`sync_directories::resolve_from_process_environment`, `EventPathFilterOptions::from_process_environment`). It never reads `vapor.json`. And `vapor service install` builds the LaunchAgent with `environment: vec![]` (`core/cli/src/commands/service.rs:146-153`), so the daemon launched by launchd sees no `VAPOR_*` variables at all and always runs with compiled defaults (`~/Vapor` ↔ `/Vapor`, default ignore rules).

Net effect: `vapor config set localSyncDirectory /x`, `cloudSyncDirectory`, `useGitIgnore`, `useVaporIgnore`, `preIgnoreRules`, `postIgnoreRules`, `languageCode`, `timelineEventLimit` are all accepted, validated, persisted — and ignored by the runtime. Only `autoLaunch` has a consumer (the lifecycle manager). The README documents these keys as *the* configuration surface.

**Fix**: have the daemon load `vapor.json` at startup (env as override, per §8.5's spirit), or have `vapor service install` translate the config file into plist `EnvironmentVariables`. The former is strictly better (config changes take effect on restart without reinstall).

### F-CLI-2 (P2): `vapor service install` writes no stdout/stderr paths for the daemon

`ServiceDescriptor { stdout_path: None, stderr_path: None }` (`service.rs:146-153`), while `docs/operations/macos/launchagent-policy.md` specifies daemon stdout/stderr should land under vapor logs. Panics and pre-logger errors from `vapord` go to `/dev/null`. The Swift `LaunchAgentController` should be compared for the same gap (see §8).

### F-CLI-3 (P3): `vapor auth login --token` puts secrets in argv

Process arguments are visible via `ps`/Activity Monitor and typically land in shell history. Even as a pre-OAuth placeholder, accept the token via stdin (`--token -`) or an env var, and document the argv variant as unsafe. (`main.rs:99-104`.)

### F-CLI-4 (P3): Misc

- `vapor logs` reads the entire log file into memory (`ipc.rs:171-183`); with no rotation (F-SHARED-2) this can be huge. Stream the tail (seek from end).
- `vapor status --json` serializes the wire `StatusResponse` verbatim — fine, but snapshot tests (`insta`) promised by §9.2 for every `--json` command don't exist yet.
- Error message in `main.rs:274` points at `core/tasks/core.md` — path is `docs/tasks/core.md`.
- `dispatch_service` on macOS ignores `--user` (fine, it's the default) but accepts `vapor service --user install` and `vapor service install` identically while `--system` errors — matches the commit message intent; just note the flag is currently decorative.
- `locate_daemon_binary` PATH fallback will happily install a LaunchAgent pointing at any `vapord` on PATH — including one in a writable location like `~/bin`. Acceptable for a dev tool; the packaged app flow (bundled sibling) avoids it. A warning when the resolved binary is not the CLI's sibling would be cheap.

---

## 8) macOS app (`apps/macos`)

### F-APP-1 (P0-parity): The CLI and the app write *different* LaunchAgent plists for the same label

Two independent installers manage `~/Library/LaunchAgents/sh.arn.vapor.daemon.plist`:

- The **Swift app** (`AppShellViewModel.makeDefaultLifecycleManager`, `AppShellViewModel.swift:496-544`) writes: `WorkingDirectory`, `StandardOutPath`/`StandardErrorPath` under vapor logs, and **eight** `EnvironmentVariables` (`PATH`, `VAPOR_DIR`, `VAPOR_USE_GITIGNORE`, `VAPOR_USE_VAPORIGNORE`, `VAPOR_LOCAL_SYNC_DIRECTORY`, `VAPOR_CLOUD_SYNC_DIRECTORY`, `VAPOR_PRE_IGNORE_RULES`, `VAPOR_POST_IGNORE_RULES`) — this is how the app's settings actually reach the daemon.
- The **Rust CLI** (`core/cli/src/commands/service.rs:139-158`) writes the same plist with **no environment, no stdout/stderr paths, no working directory**, and a `vapord` binary resolved from the CLI's sibling dir or `PATH`.

Running `vapor service install` after the app has configured things silently strips the user's entire sync configuration from the daemon's launch environment (the daemon reverts to `~/Vapor` ↔ `/Vapor` with default filters) and can repoint the LaunchAgent at a different `vapord` binary. The reverse direction (app bootstrap after CLI install) silently re-adds them. Nothing detects or reports the disagreement.

And **both** disagree with the policy doc (`docs/operations/macos/launchagent-policy.md:35-43`), which mandates `StandardOutPath`/`StandardErrorPath` plus environment pass-through of "`VAPOR_DIR` and `VAPOR_ENV` only — all other runtime behavior is code-defined or read from `vapor.json`". The doc's model (daemon reads `vapor.json`) is the one that was never built (F-CLI-1); the Swift app worked around it by shoving config through env vars, violating the policy; the CLI matches the "env: VAPOR_DIR-ish only" policy but broke because the config-file half doesn't exist.

**Fix**: one plist writer (the Rust `NativeServiceInstaller`, per the Wave 5/M2-1 plan), one canonical descriptor, and the daemon reading `vapor.json` so the plist needs at most `VAPOR_DIR`/`VAPOR_ENV`. Until then, at minimum make the CLI's descriptor match the app's.

### F-APP-2 (P2): Menubar-first startup relies on a runtime activation-policy flip, not scene configuration

Spec §2.1 (AGENTS.md) requires login/startup to be menubar-first with *no automatic main-window presentation*. The implementation (`VaporApp.swift:11-18` + `prepareMenubarOnlyStartupSurface` → `setDockVisible(false)`) only switches `NSApplication.activationPolicy` to `.accessory` on an async main-queue hop after launch; the SwiftUI `Window` scene still restores/presents by default, and the packaged `Info.plist` (`apps/macos/scripts/package.sh:125-156`) has no `LSUIElement`, so the Dock icon appears until the async flip lands. Consider `LSUIElement = true` in the packaged Info.plist (declarative, no flash) plus `defaultLaunchBehavior(.suppressed)` on the `Window` scene, keeping the runtime policy flip only for the open/close transitions. Window-close detection via `ContentView.onDisappear` is also fragile — it fires for occlusion-style disappearance, not strictly window close. (UI behavior is owner-verified per §9.3; flagging for that manual pass.)

### F-APP-3 (P2): The app surface never talks to the daemon

`AppShellState.syncState` / `providerName` are process-local placeholders (`providerName` is the constant `"Filesystem (stub)"`); the app has no IPC client, so the main window, menubar, and diagnostics all display a status that is not derived from the actual daemon (which may be stopped, crash-looping under the CLI's counter, or Paused). The `vapor_ipc::Client` exists and works — wiring it into the app's health tick would also let the app detect unexpected daemon exits, which today **nothing does**: `registerUnexpectedDaemonExit` has no production caller in the app (only the bootstrap/toggle paths run), so the crash-loop guard cannot fire in practice. The health tick promised in `launchagent-policy.md:63-67` ("the app's lifecycle coordinator detects daemon absence on its next health tick") is not implemented.

### F-APP-4 (P2): Crash-loop state is per-process and volatile, contrary to the policy doc

`launchagent-policy.md:73-76` states "the daemon's durable state carries a `last_crash_at_ms` and `consecutive_crashes` counter persisted via the state DB so backoff survives app restarts." Neither the Swift nor the Rust `CrashLoopGuard` persists anything; both count crashes in process memory, and the app and CLI each own an independent guard. Quitting and relaunching the app resets the crash count to zero, so a permanently-crashing daemon never reaches `CrashLoopPaused` if the user (or login) restarts the app in between. Also unimplemented from the same doc: "the failure window resets after a successful run" — no code observes successful runs.

### F-APP-5 (P3): Smaller app items

- `VaporConfigurationStore.loadResult` silently writes a default `vapor.json` on first read (`VaporConfiguration.swift:122-126`) — a *read* API with a write side effect; racy if the CLI writes concurrently (no cross-process lock; see F-LIFE-2).
- Swift `StructuredLogger` fsyncs (`synchronize()`) on every log line (`StructuredLogger.swift:144`) — expensive for an app logger; the Rust side only flushes.
- `handleQuitFromMenuBar` runs `launchctl kill` synchronously on the main thread (`AppLifecycleCoordinator.swift:44-56`).
- `VaporConstants` defines `Defaults.timelineEventLimit = 1000` and `Defaults.languageCode` with **no Rust counterpart** in `core/shared/src/constants.rs`, inverting the §8.6 "Rust is source of truth, Swift mirrors" rule for those keys.
- `VAPOR_DIR` handling diverges across surfaces: Swift expands `~` (`VaporPaths.normalizedDirectoryURL` uses `expandingTildeInPath`), Rust treats `~/x` as a relative path and joins it to CWD (`runtime_paths.rs:140-152`), producing a literal `./~/x` directory. Align on one rule (Rust should reject or expand `~`).

---

## 9) Scripts, packaging, CI

### F-OPS-1 (P2): Release job publishes a *draft* and `perf.sh` isn't a perf gate

- The release workflow creates/updates the GitHub release with `--draft` and never publishes (`release.yml`). README's release runbook step 7 says "Review the draft … then publish" — so this is intentional, but the workflow name/logs don't say so; a one-line echo ("draft created; publish manually") would prevent confusion.
- `scripts/perf.sh` (the "release perf gate") greps the budget doc for marker strings and re-runs the ordinary unit-test suites under 10/15-minute wall-clock ceilings. It measures nothing SLO-related; a 10× throughput regression that doesn't slow the test suite passes. Fine as a placeholder, but it gates releases while providing near-zero signal — track a real Tier-2 harness (the docs also promise scheduled nightly Tier-2 runs; no workflow has a `schedule:` trigger).

### F-OPS-2 (P3): SwiftPM CI cache path is wrong

All four workflows cache `path: .build` at the repo root, but the Swift package builds at `apps/macos/.build` (`swift build --package-path apps/macos`). The cache saves/restores an empty directory; every macOS CI run rebuilds SwiftPM deps from scratch. Change to `apps/macos/.build`.

### F-OPS-3 (P3): Pre-commit hook runs `clean → lint → test → build`

`scripts/hooks.sh` installs a hook that starts by deleting `target/`, `.build`, and `dist/` — guaranteeing a full cold rebuild of the entire Rust workspace + Swift package on **every commit** (many minutes). It also deletes `.vapor` (local runtime state) as a side effect. Recommend dropping `clean.sh` from the hook (incremental lint+test is the point of a hook) or making it opt-in.

- `scripts/clean.sh` lists `"$ROOT_DIR/vapor/logs"` — a path that doesn't exist in this repo layout; stale entry.
- `scripts/version.sh sync_cargo_lock` runs `cargo generate-lockfile`, which **re-resolves every third-party dependency to the newest compatible version** — a release-prep commit can silently bump the whole dependency tree. Use `cargo update --workspace` (workspace-members-only) instead.
- `scripts/test.sh` never invokes `scripts/cli/test.sh`; it's covered by the workspace `cargo test`, so either delete the redundant script or have the wrapper call it.
- Rust CI cache key has no `restore-keys`, so any `Cargo.lock` change is a full cold cache miss.

### F-OPS-4 (P3): `codesign --force --deep` with a single entitlements file

`apps/macos/scripts/package.sh:186-201` signs the whole bundle with `--deep`, applying the *same* entitlements to `Vapor` and `vapord`. Apple documents `--deep` as unsuited for production signing (nested code should be signed inside-out with per-binary options/entitlements), and the app vs daemon will need different entitlements as soon as hardened-runtime exceptions or App Groups appear. Sign `Contents/MacOS/vapord` explicitly first, then the app bundle.

---

## 10) Docs vs. reality

The docs are unusually detailed and mostly honest about what is placeholder vs. real (`data-flow.md`'s "current caveat" note is exemplary). The drift that remains:

### F-DOC-1 (P1): Canonical crash-loop schedule — three sources, two behaviors

`docs/operations/macos/launchagent-policy.md:66-70` specifies `2s → 4s → 8s → 16s → paused on 5th crash`, which matches the **Swift** implementation and its tests. The **Rust** port implements `NoDelay → 2s → 4s → 8s → paused` and its doc-comment claims to be a verbatim port. See F-LIFE-1 — the Rust side is the outlier and would fail the doc's own M1-6 validation scenario.

### F-DOC-2 (P2): Root README documents configuration that doesn't exist

The README **Configuration** table (README.md:62-77) lists `syncMode`, `resourceLimits`, and `idleBoost` with defaults, presented as current (`"All persisted user configuration lives in <vapor_dir>/vapor.json"`). None of these keys exist in `constants::config::ALL_KEYS`, the Swift `VaporConfiguration`, or the CLI (`vapor config set syncMode two-way` fails with *unknown key*). AGENTS.md's own README policy requires the Configuration section to reflect real config behavior. Either mark these rows "planned (Wave 8)" or move them to the design docs. Related: `sync-modes.md:86` says `syncMode` "is carried on the sync scope (`sync_directories.rs` `SyncScope`)" — present tense for a field that doesn't exist.

### F-DOC-3 (P2): README stale in several places

- **Features "Available now"** includes "Low-impact by design: Vapor defers heavy work under pressure" and "Sudden bursts … stay contained" — the throttle inputs are hardcoded to idle (F-DAEMON-17) and no actual sync work exists to contain; per the README-accuracy policy these belong under "coming next" until a real sampler and provider land.
- The Providers link `core/providers/provider_filesystem.rs` is a dead link (file doesn't exist; several docs reference it as a future Phase C8 artifact — the README shouldn't link it as if present).
- The Development structure list marks `core/platform`, `core/lifecycle`, `core/cli` as "(planned)" — all three crates exist and ship code.

### F-DOC-4 (P2): IPC contract doc vs implementation

- `ipc-contracts.md:16` says "the wire format is JSON-RPC 2.0" — the implementation is a custom `{kind, payload}` envelope with none of JSON-RPC's `jsonrpc`/`id`/`method` fields. Either implement JSON-RPC or fix the doc (the code's own "JSON-RPC 2.0-*style*" hedge suggests the doc should say "JSON, length-prefixed").
- `ipc-contracts.md:85-89`: "Every IPC payload carries a declared `payload_bytes` hint" — no such field exists anywhere.
- The documented `PayloadTooLarge` error response is never sent (F-IPC-2); oversized frames tear the connection down instead.
- Handshake field names differ (`app_schema_version` vs implemented `schema_version`); harmless but confusing for a doc labeled authoritative.
- `docs/tasks/README.md` marks Wave 7 "complete" including L3 `pause`/`resume` — given F-DAEMON-1 (pause is a no-op), that wave's definition of done ("Behavior works in happy and failure paths") isn't actually met.

### F-DOC-5 (P3): Smaller doc items

- `crash_loop.rs` module docs cite `core/lifecycle/tests/crash_loop_parity.rs`; no `tests/` directory exists in the crate (F-LIFE-1).
- `storm.rs` thresholds are described in `data-flow.md:8` as "per-directory" — the implementation counts each event in *every ancestor* window (F-DAEMON-8), which is materially different at the watch root.
- `docs/architecture/testing-strategy.md` mandates `tempfile::TempDir` per test; two daemon modules hand-roll temp dirs (noted in F-DAEMON-18).
- Promised-but-missing test classes from §9.2: property tests (`proptest` is not a dependency anywhere), `insta` snapshot tests for `vapor … --json`, and platform-trait contract suites parameterized over fake+native. All are listed as requirements in AGENTS.md §9.2 for shipped functionality (the CLI's `--json` exists today). Worth tracking explicitly so the contract doesn't silently erode.
- `.env.example` checks out — it covers all nine runtime `VAPOR_*` variables plus the packaging and perf ones. No action needed; noted for completeness.

---

## 11) Verification performed

- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: **clean** (exit 0).
- `cargo test --workspace --all-targets --all-features`: **all tests pass** (exit 0, all `test result: ok`).
- Swift tests were not run in this pass; findings on the Swift side are from code reading and its existing test expectations (which lock in the Swift-side behaviors cited above, e.g. the crash-loop schedule in `DaemonLifecycleManagerTests.swift:78-79,138-139`).

Note the meta-point: **every finding in this review coexists with a green build, green clippy, and a green test suite.** The bugs found (pause no-op, reconcile duplication, dwell-blocks-escalation, schedule parity) are all things the current tests structurally cannot catch — either because both sides of a divergence have their own self-consistent tests (crash loop), or because no test asserts the *absence* of work (pause), or because no cross-check exists (durable-queue depth after a paused reconcile). The §9 test contract would benefit from a "cross-surface parity" and a "negative-space" (assert nothing happened) test category.

## 12) What's genuinely good (keep doing this)

Not a finding, but worth recording so refactors don't regress it:

- The monotonic-vs-wall-clock discipline (`Clock` seam, `ManualClock` with independent axes) is textbook, and the tests that rewind the wall clock are exactly the right tests.
- Redaction at every boundary (log lines, persisted `last_error`, metadata keys) with shared marker lists on both languages.
- The bounded-everything posture: event maps, subtree caps, frame sizes, state value lengths, attempt counts — every unbounded-growth vector has a cap and a test.
- Permit-ID wraparound handling, `enqueue_startup_reconcile` dedupe, skew-matrix tests, and the workgate's downshift-keeps-running-work semantics are all subtle and all correct.
- Wrapper scripts + `VAPOR_DIR=./.vapor` defaulting keeps tests hermetic; no test touches `~/.vapor`.

---

## Appendix: prioritized fix order (suggested)

1. F-DAEMON-1 (pause no-op) — small, ships a real behavior for an already-shipped command.
2. F-LIFE-1 + F-DOC-1 (crash-loop schedule parity + the missing parity test) — decide canonical schedule, align, test.
3. F-CLI-1 + F-APP-1 (config plumbing: daemon reads `vapor.json`; single plist writer) — this unblocks the whole "config actually works" story and resolves the policy-doc contradiction.
4. F-DAEMON-2 (reconcile duplication) — quick dedupe guard, prevents queue pollution.
5. F-DAEMON-3 (dwell blocks escalation) — one-line rank comparison + test.
6. F-DAEMON-4 (single-instance lock) — small, prevents a class of confusing states.
7. F-DAEMON-5 (dropped-events reconcile) — closes the last ingest-loss hole.
8. Everything P2/P3 opportunistically, with F-OPS-2 (CI cache path) as the cheapest win.
