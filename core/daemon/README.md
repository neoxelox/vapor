# daemon

Rust daemon for low-impact background sync execution.

Responsibilities:

- FSEvents ingest and coalescing
- throttle controller and scheduler
- durable queue/state and retries
- reconcile and provider execution
- XPC status/control endpoints

The daemon owns heavy compute and must remain pressure-aware.

Logging:

- Runtime root is `VAPOR_DIR` (`~/.vapor` by default, `./.vapor` under repo scripts/tests).
- Structured daemon logs are appended to `<vapor_dir>/logs/vapord.logs`.
- Reserved state/db location: `<vapor_dir>/state/vapor.sqlite`.
- Log line format: `{timestamp} [{level}] ({component}): {message}. key=value ...`
- Runtime log level override uses `VAPOR_LOG_LEVEL` (`debug`, `info`, `warning`, `error`).
- Default level is `debug` in normal builds and `warning` in package builds.
