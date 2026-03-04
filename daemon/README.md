# daemon

Rust daemon for low-impact background sync execution.

Responsibilities:

- FSEvents ingest and coalescing
- throttle controller and scheduler
- durable queue/state and retries
- reconcile and provider execution
- XPC status/control endpoints

The daemon owns heavy compute and must remain pressure-aware.
