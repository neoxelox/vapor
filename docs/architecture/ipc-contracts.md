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

Every request and response payload carries an explicit `schemaVersion: u32`
field at the top level. Keys are camelCase and every value that names a
state or a choice is kebab-case (`run: {state: "running"}`,
`throttle: {state: "idle"}`), the same shape `vapor … --json` prints;
the values come from the vocabulary modules in
`core/shared/src/constants.rs`. The concrete version handshake and skew tolerance
rules below are pre-GA; backward-compat guarantees tighten at GA.

### Schema history

Source of truth: `core/shared/src/constants.rs::ipc`
(`SCHEMA_VERSION_CURRENT`, `SCHEMA_VERSION_MIN`) and the payload types in
`core/ipc/src/protocol.rs`.

- **v1** — Wave 7 baseline: `Hello`/`HelloAck`, `Status`, `Pause`,
  `Resume`, `FlushNow`, `Reconcile`, `Timeline`, `Logs` seam.
- **v2** — Wave 8 finalisation, all additive
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
  them with the daemon stopped. `StatusResponse.conflicts_unresolved` and
  `ProfileStatus.conflicts_unresolved` count the keep-both conflict
  copies the user has not resolved (the copies in the sync index plus
  the ones queued for their first upload), so a surface can flag them
  without walking the tree; unlike the cumulative `conflicts` counter it
  returns to zero on resolution (`conflict-resolution.md` §Notify).
  `StatusResponse.reconcile_state` / `reconcile_detail` (and the same
  pair on `ProfileStatus`) say what the whole-scope scan is doing:
  `idle`, `waiting` (queued, held by the throttle; the detail names
  what it waits for, such as `waiting for an idle moment: user activity
  is active`), or `running` (the detail names the root). The daemon-wide
  pair is the most active profile's. New method `SyncNow`: the user's
  on-demand sync, a whole-scope reconcile admitted under any throttle
  state but `Suspended` plus the flush boost; answers with an `Ack`, and
  a daemon that predates it answers `unsupported` like every unknown
  control method (`data-flow.md` §Throttle states).
  `ProfileStatus.sign_in_required` is `true` while the provider refuses
  that profile's sign-in and the profile is on hold; the way out is
  `vapor auth login <provider_name> --profile <id>`
  (`operations/provider-auth-operations.md` §Failure behavior). A
  daemon that predates it sends nothing, which reads as `false`.
- **v3** — the naming pass, a breaking
  change. Every key became camelCase (`schemaVersion`, `runState`,
  `queueDepth`, `profileId`) and every state value kebab-case
  (`running`, `idle`, diagnostics stages such as
  `waiting-to-upload`, actions such as `cloud-delete`); `ProfileStatus`
  `display_name` became `name` and `provider_name` became `provider`,
  on `StatusResponse` too. `FlushNow` and `SyncNow` are gone: `Reconcile`
  now carries the on-demand semantics `SyncNow` had (a scan admitted
  under any throttle state but `suspended`, plus the boost that releases
  deferred work). `sign_in_required` is `signInRequired`, and the way
  out it names is `vapor auth login --profile <id>`, since the profile
  decides the provider. A v2 peer would read v3 frames as defaults, so
  the floor moved to 3 and the handshake refuses it.
- **v4** (current; min supported v4) — the status regrouped into one
  word per key. `run`, `throttle`, and `idleBoost` are `{state,
  reason}` objects and `reconcile` is `{state, detail}`; `queueDepth`,
  `failedIntents`, and `decisionsPending` became `queued`, `failed`, and
  `decisions` (conflicts now count as decisions, so `conflictsUnresolved`
  and the cumulative `conflicts` counter are gone); the mirror counters
  sit in `mirror {reverts, deletes}`; `resourceBudget` became `resources`
  with `cpu` / `memory` `{limitPercent, usedPercent}` and `bandwidth`
  `{limitPercent, usedKbps}`; `configRestartRequired` became
  `restartRequired` and `daemonId` became `daemon`. The daemon-wide
  `run.reason` is the daemon's own pick (a suspension first, otherwise the
  first profile in that state that gives one), so apps no longer compute
  it. A profile row's `suspendedReason` folded into `run.reason` plus
  `suspended: true`. `loopPreventionSuppressions` moved to diagnostics as
  `suppressedEchoes`, whose rows lost the word intent: `items` of `{id,
  profileId, path, action, stage, elapsedMs, attempts, error, reason}`
  plus `droppedEvents`. The throttle state `idle-drain` is `idle`. The
  `SetAutoLaunch` and `UpdateExcludes` methods are gone: nothing called
  them, and `vapor.json` has one set of writers (`vapor config set`, the
  app, `vapor service`), which the daemon reloads.

### Handshake

- On connection, the app sends a
  `Hello { schemaVersion, supportedMinVersion, clientId }` request.
  The daemon responds with
  `HelloAck { schemaVersion, supportedMinVersion, serverId }`.
- If `app_schema_version < daemon.supported_min_version` or
  `daemon_schema_version < app.supported_min_version`, the handshake fails
  with `IncompatibleVersion { peerVersion, requiredMin, localVersion }`. Both sides log
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

The daemon as a whole, then one row per profile (`StatusResponse` and
`ProfileStatus` in `core/ipc/src/protocol.rs`):

```json
{
  "schemaVersion": 4,
  "daemon": "vapord/0.2.0-alpha.3",
  "provider": "gdrive",
  "run": { "state": "running", "reason": "watching ~/Vapor" },
  "throttle": { "state": "idle", "reason": "idle, plugged in, and cool" },
  "reconcile": { "state": "idle", "detail": "" },
  "queued": 0,
  "failed": 0,
  "decisions": 1,
  "mirror": { "reverts": 0, "deletes": 0 },
  "restartRequired": null,
  "resources": {
    "cpu": { "limitPercent": 15, "usedPercent": 2 },
    "memory": { "limitPercent": 10, "usedPercent": 1 },
    "bandwidth": { "limitPercent": 25, "usedKbps": 0 },
    "idleBoost": { "state": "off", "reason": "user idle for 12s of the required 300s" }
  },
  "profiles": [
    {
      "id": "default", "name": "default", "provider": "gdrive", "syncMode": "two-way",
      "run": { "state": "running", "reason": "watching ~/Vapor" },
      "reconcile": { "state": "idle", "detail": "" },
      "queued": 0, "failed": 0, "decisions": 1,
      "mirror": { "reverts": 0, "deletes": 0 },
      "suspended": false, "signInRequired": false
    }
  ]
}
```

- The daemon-wide `run.state` is the most severe profile's; `run.reason`
  is the daemon's pick (a suspended profile names itself first,
  otherwise the first profile in that state that gives a reason). A
  profile's `run.reason` says what is watched, what blocks sync, or which
  decision it waits on; the throttle reason is separate and never
  overwrites it.
- `decisions` counts open decisions, conflicts included
  (`conflict-resolution.md`).
- `suspended: true` means the profile was stopped by its configuration
  or by repeated failures (`run.reason` says which); `signInRequired`
  means the provider refused its sign-in.

### Provider / auth

- Auth state, token-refresh health, provider capability flags, last auth
  error classification (per profile).

### Lifecycle

- Auto-launch enabled state, last launch result, crash-loop pause state,
  daemon uptime, startup-reconstruction-barrier state.

### Controls

- `Pause` / `Resume`, `Reconcile` (the user's on-demand scan), excludes
  update, auto-launch toggle, config reload trigger.

### Diagnostics (per item of work)

- For each queued or in-flight item the daemon exposes `id`,
  `profileId`, `path`, `action`, `stage`, `elapsedMs` (time in the
  stage), `attempts`, `error` (the last one), and a human-readable
  `reason` it is not moving (for example "hash workers 2/2 and read
  tokens 1/1 in use", "retry backoff active (attempt 3)", "waiting for
  decision #4"). This is the data source for the "why is this stuck"
  surface.
- `action` says what the item does (`constants::intent::ACTION_*`):
  - `upload` / `download` — content moving to or from the cloud.
  - `rename` — a local rename carried to the cloud.
  - `local-delete` — a delete made on this device, carried to the cloud.
  - `cloud-delete` — a delete made in the cloud, applied on this device.
  - `reconcile` — a subtree the reconcile walk still has to compare.
- `stage` says where it is (`constants::intent::STAGE_*`). Three values
  describe the durable queue, the rest mirror the staged executor:
  - `queued` — in the durable queue, waiting to be leased.
  - `retrying` — hit a transient or rate-limited failure and waits out
    its backoff; it reads `queued` again once the backoff elapses.
  - `held` — parked behind an open decision; the blocker names the
    decision number. Not leased until the decision is answered.
  - `planning` — leased and in the planner.
  - `waiting-to-hash` — planned, waiting for a hash permit (workgate cap
    exhausted, or the throttle disallows hashing).
  - `hashing` — being hashed.
  - `waiting-to-upload` — hashed (or needing no hash), waiting for an
    upload permit.
  - `uploading` — being uploaded.
  - `waiting-to-download` — a cloud change waiting for a download permit.
  - `downloading` — being downloaded.
- Diagnostics also expose `suppressedEchoes` (the daemon's own writes
  seen again and skipped, loop prevention at work) and the
  `BoundedFsEventRecorder` drop count as `droppedEvents`, so users can
  detect prolonged callback-vs-
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
- Unknown field: request carrying a synthetic `futureField` is handled
  without error and logged at debug level.
- Payload bounds: a `payload_bytes` value above the cap is rejected before
  deserialization.
- Stream frame bounds: a single oversized timeline frame is rejected
  without tearing down the stream.
- Transport matrix: the same test cases run on every supported transport
  (UDS on macOS/Linux, named pipe on Windows).
