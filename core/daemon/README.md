# daemon

Rust daemon for low-impact background sync execution.

Responsibilities:

- FSEvents ingest with canonical watch-root enforcement, traversal/symlink escape rejection, and a 250ms debounce/coalescing loop
- storm detection with deferred reconcile markers for noisy subtrees
- throttle controller with a 1s sample policy, strict planner/hash/upload/reconcile work permits, and a keyed latest-wins scheduler
- a composed runtime loop that advances watcher ingest, debounce, durable queueing, work permits, and reconcile progression on each daemon tick
- idle-biased reconcile control that runs in interruptible slices and clears compaction boundaries after success
- SQLite durable queue/state with startup lease recovery, conservative whole-scope restart reconstruction, retry backoff, durable failed intents, and state metadata
- validated runtime paths plus restrictive local permissions for logs/state artifacts and non-panicking log fallback
- bounded durable diagnostics/state fields with corruption guards for attempt counters, timestamps, and oversized stored values
- pre-GA durable state keeps only the current schema path and rejects older on-disk schemas instead of carrying migration shims
- provider choice is injected through the provider trait boundary at runtime startup instead of being hardcoded in daemon core state
- staged planner/hash/upload execution now uses work permits to keep multiple durable intents moving concurrently within throttle limits
- reconcile and provider execution
- XPC status/control endpoints

The daemon owns heavy compute and must remain pressure-aware.

Logging:

- Runtime root is `VAPOR_DIR` (`~/.vapor` by default, `./.vapor` under repo scripts/tests).
- Structured daemon logs are appended to `<vapor_dir>/logs/vapord.logs`.
- Reserved state/db location: `<vapor_dir>/state/vapor.sqlite`.
- Log line format: `{timestamp} [{level}] ({component}): {message}. key=value ...`
- Runtime log level override uses `VAPOR_LOG_LEVEL` (`debug`, `info`, `warning`, `error`).
- Default level is `debug` when `VAPOR_ENV=dev`, otherwise `info`.
