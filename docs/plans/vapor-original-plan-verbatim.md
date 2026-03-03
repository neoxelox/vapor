# vapor — macOS invisible background sync with boot auto-launch (Swift UI + Rust daemon)

## 0) Mission

`vapor` is an always-on macOS background sync service that keeps a chosen local folder (default `~/Drive/`) backed up to the cloud (default Google Drive). It prioritizes **not degrading MacBook performance** over strict real-time. Sync is **best-effort** and **eventually consistent**, becoming more “real-time” only when the system is idle.

By default, `vapor` **launches automatically at boot/login** and runs continuously in the background; the user can turn this off.

---

# 1) Principles (ordered by priority)

1. **Do no harm**: minimal CPU/disk/network; never compete with user workloads.
2. **Opportunistic sync**: do work when idle; defer under load (eventual consistency).
3. **Correctness + durability**: no data loss; crash-safe queue/state; resume after restart.
4. **Best-effort freshness**: seconds when possible, minutes when busy.
5. **Provider-extensible**: Google Drive first; easy to add S3/R2 via a provider interface.

---

# 2) Requirements & budgets (hard constraints)

## 2.1 Resource budgets

* **Idle**: near-zero CPU most of the time; tiny periodic wakeups.
* **Under load / active coding**: keep work to minimal bookkeeping; avoid scans/hashing.
* **Burst allowed only when idle**: controlled, bounded bursts; always yield quickly.

## 2.2 Pressure-aware throttling (mandatory)

Continuously adapt using:

* on battery / low power mode
* thermal pressure
* system CPU load + vapor CPU
* disk I/O contention proxy (own read wait/latency)
* network conditions
* user activity (“active coding”)

When pressure is high: defer uploads → eventual consistency.

---

# 3) Boot auto-launch (default ON)

## 3.1 Default behavior

* On install and first run, `vapor` registers its daemon to **start automatically at user login** (the practical “boot” model for per-user sync).
* Default setting: **Auto-launch enabled**.
* UI offers a toggle: “Start vapor at login” (ON/OFF).

## 3.2 Implementation approach

Use macOS-native mechanisms:

* **LaunchAgent** (recommended) for per-user, login-time start:

  * installs a `~/Library/LaunchAgents/<bundle-id>.plist`
  * configured as `RunAtLoad=true`, `KeepAlive=true` (with careful KeepAlive rules to avoid restart loops)
* Optionally use **SMAppService** (Login Item) from the Swift app for modern login item management (preferred UX), but still backed by a LaunchAgent for the daemon.

### Auto-launch toggle semantics

* ON: install/enable LaunchAgent (or SMAppService login item) and start daemon
* OFF: disable/uninstall LaunchAgent (or disable login item), and optionally offer “Stop vapor now” separately

### Failure safety

* If daemon crashes repeatedly:

  * exponential delay before relaunch
  * UI shows “vapor paused due to repeated failures”
  * writes crash reason to logs

---

# 4) Architecture

## 4.1 Components

1. **Swift UI app**

* Onboarding: choose local root; authenticate provider (Google default)
* Settings: excludes, resource policy, auto-launch toggle, provider selection
* Menubar status: Idle / Queued / Syncing (low) / Throttled / Suspended / Error
* Control: Pause/Resume, “Flush now”, diagnostics
* Stores secrets in Keychain
* Manages auto-launch configuration (default ON)

2. **Rust daemon (LaunchAgent)**

* Core engine:

  * FSEvents watch
  * conservative debounce + coalescing
  * keyed superseding scheduler
  * deferred storm reconcile
  * bounded planning/hashing
  * bounded uploading with retries/backoff
  * durable DB state
  * auto-tuning (impact-first)
* Exposes status/control API to Swift via XPC

3. **Provider modules (Rust)**

* `provider_gdrive` (default)
* `provider_s3` (S3/R2-compatible)
* Shared provider trait + capability flags

---

# 5) Provider extensibility layer

## 5.1 Capabilities

* hierarchical paths (Drive) vs prefix keys (S3/R2)
* server-side rename (Drive) vs copy+delete (S3)
* change feed support (Drive) vs none (S3)
* trash support (Drive) vs optional prefix trash (S3)

## 5.2 Provider trait (conceptual)

* init/auth refresh hooks
* ensure remote root
* put/update/get/delete/move
* list subtree (paginated)
* optional poll changes (Drive)
* error mapping (transient/rate-limited/auth/permanent)

Core engine must not assume Drive semantics.

---

# 6) Node-dev performance defaults (invisible-first)

## 6.1 Default excludes (ON)

**Always**

* `**/.git/**`, `**/.DS_Store`, tmp/swap files, caches

**Node**

* `**/node_modules/**`
* `**/.pnpm-store/**`
* `**/.yarn/cache/**`, `**/.yarn/unplugged/**`
* `**/.npm/**`
* `.next/ .nuxt/ .svelte-kit/`
* `dist/ build/ out/`
* `.turbo/ .vite/ .parcel-cache/`
* `coverage/ storybook-static/`
* `.tsbuildinfo .eslintcache`
* logs (toggle, default ON)
* `.env.local` (toggle, default ON)

Add `.vaporignore` support per project.

## 6.2 Conservative debounce defaults (eventual consistency bias)

* code/text: **1200ms**
* key configs: **900ms**
* lockfiles: **2500ms**
* large files: **4000ms**
  Bounds:
* min 500ms
* max 8000ms (when highly throttled)
  Tick: **250ms**

---

# 7) macOS file watching (low overhead)

* FSEvents recursive watch on local root
* Callback does minimal work:

  * normalize path
  * exclude check
  * record event in `event_map`

No DB, hashing, scanning, or network in callback.

---

# 8) Debounce/coalesce (CPU-saving core)

## 8.1 In-memory maps

* `event_map[path] = {first_event_at, last_event_at, flags, burst_count, project_root}`
* `dir_burst_map[dir] = rolling counters`
* `recent_deletes` (TTL) for atomic save heuristics
* `self_write_cache` (TTL) for loop prevention if bidirectional

## 8.2 Stabilization loop

Every 250ms:

* if `now - last_event_at >= quiet_ms(path)`:

  * emit `StabilizedEvent(path, flags)`
  * remove from map

Atomic saves: best-effort inode/time correlation; otherwise defer reconcile.

---

# 9) Keyed superseding scheduler (latest wins)

* One intent per path:

  * modify/create → UPLOAD
  * delete → DELETE (supersedes)
  * rename/move → RENAME (merge)
* If new changes arrive while job running: mark dirty; re-check at end.
* Persist durable jobs only when executing or waiting; keep in-memory intents compact.

---

# 10) The Throttle Controller (the #1 priority)

## 10.1 Inputs (sample every 1–2s)

* on battery / low power
* thermal pressure
* system CPU load + vapor CPU
* disk read latency proxy from worker semaphore waits
* network throughput and error rate
* user activity (optional event tap) or heuristic

## 10.2 Throttle states

1. **IdleDrain** (idle/plugged/cool) — drain backlog carefully
2. **Light** (normal) — low concurrency
3. **Throttled** (active / moderate load) — minimal work
4. **Suspended** (high load / thermal / battery) — no uploads

## 10.3 Default caps per state

**IdleDrain**

* hash/planner workers: 4
* read tokens: 2
* upload concurrency: 4
* allow deferred reconcile

**Light**

* workers: 2–3
* read tokens: 1–2
* upload: 2

**Throttled**

* workers: 1
* read tokens: 1
* upload: 1
* no reconcile scans

**Suspended**

* no hashing/uploads
* only coalesce events
* persist minimal queue updates periodically

---

# 11) Execution pipeline (bounded, low compute)

## 11.1 Planner stage

* stat file
* determine CREATE/UPDATE/DELETE/RENAME
* avoid hashing by default (backup semantics allow updating without strong dedupe)

## 11.2 Hashing stage (rare)

* used only when:

  * strict integrity mode enabled, OR
  * conflict detection requires it, OR
  * idle and beneficial for large files
    Default: minimal hashing.

## 11.3 Uploader stage

* concurrency and request rate are always gated by ThrottleState
* Google Drive: resumable uploads for large files
* durable retries/backoff

---

# 12) Storm detection + deferred reconcile

## 12.1 Storm triggers

* per dir: 200 unique paths / 2s OR 600 events / 2s
* global: 5000 pending paths in event_map

On storm:

* stop per-path processing under that dir
* schedule `RECONCILE_SUBTREE(dir)` **deferred**
* run reconcile only in IdleDrain (or when backlog would never converge)

Reconcile must be interruptible and obey throttle caps.

---

# 13) Google Drive provider (default)

* Remote root: `vapor - <DeviceName>`
* Folder ID cache
* Upload:

  * multipart small
  * resumable large
* Delete: soft-delete (trash) by default, record tombstone
* Optional bidirectional: changes feed polling (low frequency, only when not Suspended)

---

# 14) S3/R2 provider (extensible)

* Root: bucket + prefix
* PUT object for upload
* rename: copy+delete
* delete: delete or move to trash prefix
* one-way local→remote recommended for invisibility

---

# 15) Reliability: retries, rate limits, eventual consistency

* Durable job queue with at-least-once semantics
* Exponential backoff with jitter
* Rate-limit detection reduces concurrency automatically
* Eventual consistency guarantee:

  * stabilized files get uploaded when resources permit
  * under sustained load, vapor may defer indefinitely but must never lose intent state

---

# 16) Auto-tuning (impact-first)

## 16.1 Metrics (local-only, bounded)

60s aggregates:

* event rates, stabilization ratio
* queue depths
* e2e latency p50/p95
* CPU avg/p95
* read wait time
* uploads throughput and errors/rate limits
* time spent in each throttle state

## 16.2 Tuning priorities

1. reduce CPU and I/O impact
2. avoid rate limits
3. only then reduce latency

## 16.3 Tunables

* debounce per extension/project
* thresholds for throttle transitions
* concurrency caps (within safe max)
* storm thresholds
* reconcile scheduling aggressiveness

Cadence: 60–120s, one small change per cycle.

---

# 17) Optional features (included but default safe)

* Active coding detection (permission) with fallback heuristic
* “Flush now” button (temporary IdleDrain boost)
* Folder priority classes: High/Normal/Archive
* Bidirectional sync (OFF by default)
* Mass-change/ransomware guard: pause uploads and alert on suspicious patterns
* Detailed diagnostics panel (CPU budget, throttle reasons, queue)

---

# 18) UI/daemon API (XPC)

Expose:

* running state + throttle state + reason
* queue depths
* last sync times
* provider auth status
* auto-launch status (enabled/disabled) + last boot-start result
* controls: pause/resume, flush now, toggle auto-launch, change excludes

---

# 19) Milestones (priority order)

1. Boot auto-launch + daemon skeleton + status UI
2. Low-impact engine: FSEvents + excludes + conservative debounce + keyed intents + throttle controller
3. Google Drive provider: one-way eventual sync
4. Storm deferral + deferred reconcile (idle-only)
5. Auto-tuning (impact-first)
6. Provider extensibility + S3/R2 module
7. Optional bidirectional + safeguards

---

# 20) Default configuration summary (vapor, invisible-first, auto-launch ON)

* Auto-launch at login: **ON**
* Provider: Google Drive
* Excludes: Node-heavy defaults ON
* Debounce: conservative (1200/900/2500/4000ms)
* Throttle controller: ON, heavy bias toward Throttled/Suspended under load
* Upload concurrency: usually 1–2; 4 only when idle
* Hashing: minimal by default
* Reconcile: deferred; idle-only
* Auto-tuning: ON, impact-first
* Bidirectional: OFF by default
