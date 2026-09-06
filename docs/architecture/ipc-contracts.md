# IPC Contracts

This document defines the transport-agnostic contract surface between every
Vapor app (`apps/macos`, future `apps/windows`, future `apps/linux`) or CLI
(`vapor`) and the Rust daemon (`vapord`).

The *transport* is platform-specific and covered by:

- `docs/architecture/macos/ipc-transport.md` — macOS (Unix domain socket;
  optional NSXPC wrapping).
- `docs/architecture/windows/ipc-transport.md` — Windows (named pipe).
- `docs/architecture/linux/ipc-transport.md` — Linux (Unix domain socket).

The contracts defined in this document are transport-agnostic: the same
schema, handshake, and skew matrix apply to every transport. The wire
format is JSON with a `u32` little-endian length prefix per frame; the
request/response envelopes are Vapor-specific tagged enums
(`{"kind": …, "payload": …}` / `{"outcome": …, "value": …}`), not
JSON-RPC.

Historical note: this document was previously titled "XPC Contracts". The
contract surface has always been wire-agnostic; renaming it removes the
macOS-only connotation.

## Contract versioning rules

Every request and response payload carries an explicit `schema_version: u32`
field at the top level. The concrete version handshake and skew tolerance
rules below are pre-GA; backward-compat guarantees tighten at GA.

### Schema history

Source of truth: `core/shared/src/constants.rs::ipc`
(`SCHEMA_VERSION_CURRENT`, `SCHEMA_VERSION_MIN`) and the payload types in
`core/ipc/src/protocol.rs`.

- **v1** — Wave 7 baseline: `Hello`/`HelloAck`, `Status`, `Pause`,
  `Resume`, `FlushNow`, `Reconcile`, `Timeline`, `Logs` seam.
- **v2** (current; min supported v1) — Wave 8 finalisation, all additive
  (every new field is serde-defaulted so a v1 peer's frames still parse):
  `StatusResponse` gains sync-scope/provider/sync-mode fields, per-profile
  summaries (`ProfileStatus`), effective resource ceilings + idle-boost
  state (`ResourceBudgetStatus`), and cumulative conflict/mirror counters;
  new `Diagnostics` method returns per-intent "why stuck" rows
  (`DiagnosticsResponse` / `IntentDiagnostic`); new `SetAutoLaunch` and
  `UpdateExcludes` control methods persist config through the daemon; and
  `TimelineEntry` carries `profile_id`.
- **v2, later additions** (still additive, serde-defaulted):
  `StatusResponse.decisions_pending` and `ProfileStatus.decisions_pending`
  count the open decisions (`data-flow.md` §Decisions). The decisions
  themselves are not served over IPC: `vapor decisions` reads and answers
  them in the profile state DB directly, so the app can list and answer
  them with the daemon stopped.

### Handshake

- On connection, the app sends a
  `Hello { schema_version, supported_min_version, client_id }` request.
  The daemon responds with
  `HelloAck { schema_version, supported_min_version, server_id }`.
- If `app_schema_version < daemon.supported_min_version` or
  `daemon_schema_version < app.supported_min_version`, the handshake fails
  with `IncompatibleVersion { peer_version, required_min }`. Both sides log
  a structured diagnostic (`ipc.handshake.incompatible` with both versions)
  and the app surfaces an actionable "Please update Vapor" state. The
  daemon does not accept further requests from that session.
- If both sides are within their supported window, the session proceeds. No
  per-request version negotiation is required after the handshake.

### Field omission tolerance (forward compatibility)

- **Unknown fields on the receiving side are ignored, not rejected.**
  Daemon-N receiving a request serialized by app-(N-1) may see fewer fields
  than the current N schema defines; missing fields take type-specific
  defaults: numeric = `0`, boolean = `false`, string = `""`, optional =
  `None`, array = `[]`. Unknown fields added by a newer peer are logged at
  debug level (`ipc.unknown_field` with the field name) and discarded.
- **Response field omission is symmetric.** App-(N-1) receiving a response
  from daemon-N applies the same defaulting rules for fields it does not
  know about.
- **Additive-only is preferred.** Within a schema version, breaking changes
  (field removal, type change, semantic reinterpretation of an existing
  field) require a new `schema_version` bump and a new `supported_min_version`
  floor on the side that can no longer interpret the old shape.

### Removal policy

- Removing a field requires a two-step migration: first mark the field
  `deprecated = true` in the schema and ignore it on the receiving side
  while still accepting it on the sending side; second, at the next
  `schema_version` bump, remove it and raise `supported_min_version` to the
  bump point.
- Pre-GA exception: the project owner may fast-track field removals without
  the two-step migration when both app and daemon ship in the same release
  artifact. The release notes must call out the breaking shape change.

### App/daemon skew matrix

Pre-GA supported skew is `|app_schema_version - daemon_schema_version| <= 1`.
Concretely:

- `app-N` ↔ `daemon-N`: fully supported.
- `app-N` ↔ `daemon-(N-1)`: supported if
  `N-1 >= app.supported_min_version`. App omits N-only fields on send;
  daemon returns N-1 responses that the app defaults missing fields on.
- `app-(N-1)` ↔ `daemon-N`: supported if
  `N-1 >= daemon.supported_min_version`. Daemon defaults missing fields on
  receive; app ignores N-only fields in response.
- `|N - M| >= 2`: not supported; handshake returns `IncompatibleVersion`.

### Payload size bounds

- Every frame declares its length in the `u32` prefix and is bounded at
  the framing layer at `MAX_PAYLOAD_BYTES`
  (`core/shared/src/constants.rs::ipc`, default `4 * 1024 * 1024`). An
  oversized declaration is answered with the `PayloadTooLarge` error
  response (carrying the declared size) before any allocation or
  deserialization attempt, then the session is closed.
- Streamed endpoints (diagnostics timeline, activity events) use chunked
  frames; each frame is bounded independently.
- Server-side connection hygiene: at most
  `MAX_CONCURRENT_CONNECTIONS` concurrent sessions are served (excess
  connections are dropped at accept), and a session idle longer than
  `CONNECTION_IDLE_TIMEOUT_MILLIS` is reaped.

## Contract groups

### Status

- Running state, throttle state, throttle reason, queue depth (total +
  per-profile), last sync markers, effective resource ceilings
  (cpu/memory/bandwidth), current utilization, idle-boost state and reason,
  open decisions (total + per-profile).

### Provider / auth

- Auth state, token-refresh health, provider capability flags, last auth
  error classification (per profile).

### Lifecycle

- Auto-launch enabled state, last launch result, crash-loop pause state,
  daemon uptime, startup-reconstruction-barrier state.

### Controls

- `Pause` / `Resume`, `FlushNow`, excludes update, auto-launch toggle,
  config reload trigger.

### Diagnostics (per-intent)

- For each pending durable intent the daemon exposes: `intent_id`,
  `profile_id`, `path`, `action`, current `stage`, elapsed-in-stage,
  attempt count, last-error classification, and a human-readable
  `blocker_reason` (e.g., "Throttle Suspended: no uploads allowed", "Hash
  worker cap 2/2 in use", "Rate-limit slowdown until {iso8601}"). This is
  the data source for the diagnostics "why is this intent stuck" surface.
- The `stage` enum mirrors the staged-executor's own `ExecutionStage` plus
  four queue-state values that exist outside any executor stage. The
  complete set:
  - `Queued` — intent is in the durable pending queue, waiting to be leased.
  - `Held` — intent is parked behind an open decision; the blocker names
    the decision number. Not leased until the decision is answered.
  - `Planner` — leased and currently in the planner stage of the staged
    executor.
  - `WaitingForHash` — finished planner, blocked acquiring a hash permit
    (workgate cap exhausted or throttle disallows hashing).
  - `Hash` — currently hashing.
  - `WaitingForUpload` — finished hashing (or skipped hashing for
    non-content-bearing actions), blocked acquiring an upload permit.
  - `Upload` — currently uploading.
  - `WaitingForDownload` — remote-apply intent waiting for a download
    permit.
  - `Download` — currently downloading remote content.
  - `Retrying` — leased intent that hit a transient/rate-limited failure
    and was requeued with backoff; resurfaces as `Queued` once
    `available_at` elapses.
  - `DeferredReconcile` — storm-compacted subtree marker waiting on its
    `available_at`.
- Diagnostics also expose the `BoundedFsEventRecorder`
  `dropped_incoming_event_count` so users can detect prolonged callback-vs-
  runtime backpressure (a non-zero value means raw fs-watch callbacks pushed
  faster than the runtime drained, and some events were dropped at the
  queue boundary rather than allowed to grow the queue without bound).

## Test matrix

The IPC implementation must include:

- Handshake: `app-N ↔ daemon-N`, `app-N ↔ daemon-(N-1)`,
  `app-(N-1) ↔ daemon-N`, and `|N - M| = 2` negative case (expects
  `IncompatibleVersion`).
- Field omission: request serialized without an N-only field is handled by
  daemon-N without error and uses default values.
- Unknown field: request carrying a synthetic `future_field` is handled
  without error and logged at debug level.
- Payload bounds: a `payload_bytes` value above the cap is rejected before
  deserialization.
- Stream frame bounds: a single oversized timeline frame is rejected
  without tearing down the stream.
- Transport matrix: the same test cases run on every supported transport
  (UDS on macOS/Linux, named pipe on Windows).
