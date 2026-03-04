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

- Structured daemon logs are appended to `~/Library/Logs/Vapor/vapord.log`.
- Test script logs: `.vapor/logs/vapord.logs`.
- Log line format: `{timestamp} [{level}] ({component}): {message}. key=value ...`
- Runtime log level override uses `VAPOR_LOG_LEVEL` (`debug`, `info`, `warning`, `error`).
- Default level is `debug` in normal builds and `warning` in package builds.
