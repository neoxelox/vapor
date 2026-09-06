# daemon

Rust daemon for low-impact background sync execution. Binary name: `vapord`.
Consumed by every Vapor app surface (macOS, future Windows/Linux, CLI) via
the IPC contract in `docs/architecture/ipc-contracts.md`.

Responsibilities:

- Fs-watch ingestion via `core/platform/fs_watch` (FSEvents on macOS,
  `ReadDirectoryChangesW` on Windows, inotify/fanotify on Linux) with
  canonical watch-root enforcement, traversal/symlink escape rejection, and
  a 250 ms debounce/coalescing loop.
- Storm detection with deferred reconcile markers for noisy subtrees.
- Throttle controller with a 1 s sample policy, strict
  planner/hash/upload/reconcile work permits, and a keyed latest-wins
  scheduler.
- Composed runtime loop that advances watcher ingest, debounce, durable
  queueing, work permits, and reconcile progression on each daemon tick.
- Idle-biased reconcile control that runs in interruptible slices and
  clears compaction boundaries after success.
- SQLite durable queue/state with startup lease recovery, conservative
  whole-scope restart reconstruction, retry backoff, durable failed
  intents, and state metadata.
- Validated runtime paths plus restrictive local permissions for logs/state
  artifacts (where the OS supports them) and non-panicking log fallback.
- Bounded durable diagnostics/state fields with corruption guards for
  attempt counters, timestamps, and oversized stored values.
- Durable state carries forward migrations from the last two schema
  versions (`docs/architecture/state-schema-migrations.md`); anything
  older or newer is rejected, and a corrupt file is quarantined next to
  the DB before a fresh one is created.
- Provider choice is injected through the provider trait boundary at
  runtime startup; core engine state stays provider-neutral.
- Staged planner/hash/upload execution uses work permits to keep multiple
  durable intents moving concurrently within throttle limits.
- IPC status/control endpoints exposed per
  `docs/architecture/ipc-contracts.md`.

The daemon owns heavy compute and must remain pressure-aware. OS-specific
code lives behind `core/platform` traits, never sprinkled through the
engine.

Testing expectations (heavy coverage required): every non-trivial module
ships with unit and integration tests; property tests are planned once
`proptest` is adopted (tracked in `docs/tasks/core.md`). Policy:
`docs/architecture/testing-strategy.md`.

Logging:

- Runtime root is `VAPOR_DIR` (`~/.vapor` by default, `./.vapor` under repo
  scripts/tests).
- Structured daemon logs are appended to `<vapor_dir>/logs/vapord.logs`.
- Reserved state/db location: `<vapor_dir>/state/vapor.sqlite`.
- Log line format: `{timestamp} [{level}] ({component}): {message}. key=value ...`
- Runtime log level override uses `VAPOR_LOG_LEVEL` (`debug`, `info`, `warning`, `error`).
- Default level is `debug` when `VAPOR_ENV=dev`, otherwise `info`.
