# daemon

Rust daemon for low-impact background sync execution.

Responsibilities:

- FSEvents ingest plus a 250ms debounce/coalescing loop with conservative per-path quiet windows
- storm detection with deferred reconcile markers for noisy subtrees
- throttle controller with a 1s sample policy, strict planner/hash/upload/reconcile work permits, and a keyed latest-wins scheduler
- a composed runtime loop that advances watcher ingest, debounce, durable queueing, work permits, and reconcile progression on each daemon tick
- idle-biased reconcile control that runs in interruptible slices and clears compaction boundaries after success
- SQLite durable queue/state with startup lease recovery, conservative whole-scope restart reconstruction, retry backoff, durable failed intents, and state metadata
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
