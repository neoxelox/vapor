# IPC Contracts

This document defines the transport-agnostic contract surface between every
Vapor app (`apps/macos`, future `apps/windows`, future `apps/linux`) or CLI
(`vapor`) and the Rust daemon (`vapord`).

The *transport* is platform-specific and covered by:

- `docs/architecture/macos/ipc-transport.md`: macOS (Unix domain socket;
  optional NSXPC wrapping).
- `docs/architecture/windows/ipc-transport.md`: Windows (named pipe).
- `docs/architecture/linux/ipc-transport.md`: Linux (Unix domain socket).

The contracts defined in this document are transport-agnostic: the same
schema, handshake, and skew matrix apply to every transport. The wire
format is JSON with a `u32` little-endian length prefix per frame; the
request/response envelopes are Vapor-specific tagged enums
(`{"kind": …, "payload": …}` / `{"outcome": …, "value": …}`), not
JSON-RPC.

## Contract versioning rules

The handshake (`hello`, its ack) and every data payload (the ack of
a control request, status, diagnostics, timeline) carry an explicit
`schemaVersion: u32` field at the top level. A bare method call and an
error body carry none: the handshake already agreed the protocol. Keys are camelCase and every value that names a
state or a choice is kebab-case (`run: {state: "running"}`,
`throttle: {state: "idle"}`), the same shape `vapor … --json` prints;
the values come from the vocabulary modules in
`core/shared/src/constants.rs`. The concrete version handshake and skew tolerance
rules below are pre-GA; backward-compat guarantees tighten at GA.

### Versions

Source of truth: `core/shared/src/constants.rs::ipc`
(`SCHEMA_VERSION_CURRENT`, `SCHEMA_VERSION_MIN_SUPPORTED`) and the payload types in
`core/ipc/src/protocol.rs`. The current version is 5 and the floor is 5:
no build speaks an older shape. Version 5 lets a control `Call` name
one profile. A version 4 daemon would not read the name and would act
on every profile, so the two do not talk: the handshake refuses the
pair.

A field added inside a version is optional, so a peer of the same
version that predates it reads the frame. That is right for an answer,
where a field the reader does not know is only not shown. A field
that narrows a request takes a new version, because a peer that does
not read it does more than was asked.

### Handshake

- On connection, the app sends a
  `Hello { schemaVersion, supportedMinVersion, clientId }` request.
  The daemon responds with
  `HelloAck { schemaVersion, supportedMinVersion, serverId }`.
- If `app_schema_version < daemon.supported_min_version` or
  `daemon_schema_version < app.supported_min_version`, the handshake fails
  with `IncompatibleVersion { peerVersion, requiredMin, localVersion }`. The
  daemon logs "Refused an IPC client" at INFO with the client's name and
  both versions, and the client surfaces an actionable "update Vapor"
  state. The daemon does not accept further requests from that session.
- If both sides are within their supported window, the session proceeds. No
  per-request version negotiation is required after the handshake.

### Field omission tolerance (forward compatibility)

- **Unknown fields on the receiving side are ignored, not rejected.**
  Daemon-N receiving a request serialized by app-(N-1) may see fewer fields
  than the current N schema defines; missing fields take type-specific
  defaults: numeric = `0`, boolean = `false`, string = `""`, optional =
  `None`, array = `[]`. Unknown fields added by a newer peer are
  discarded.
- **Response field omission is symmetric.** App-(N-1) receiving a response
  from daemon-N applies the same defaulting rules for fields it does not
  know about.
- **Additive-only is preferred.** Within a schema version, breaking changes
  (field removal, type change, semantic reinterpretation of an existing
  field) require a new `schema_version` bump and a new `supported_min_version`
  floor on the side that can no longer interpret the old shape.

### Removal policy

- Removing a field, or changing its type or meaning, needs a
  `schemaVersion` bump and a floor (`supportedMinVersion`) at the bump.
- Before GA the app, the CLI, and the daemon ship in one artifact, so a
  removal ships in one step. The release notes call out the breaking
  shape change.

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
- Every answer is one frame. The timeline answer keeps the newest
  entries that fit the frame and sets `truncated: true` when it left
  older ones out; `timelineLimit` (at most 10 000) bounds the buffer
  itself. The diagnostics answer is bounded the same way and has its
  own `truncated`.
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
  "schemaVersion": 5,
  "daemon": "vapord/0.2.0-alpha.3",
  "provider": "gdrive",
  "run": { "state": "running", "reason": "watching ~/Vapor" },
  "throttle": { "state": "idle", "reason": "idle, plugged in, and cool" },
  "reconcile": { "state": "idle", "detail": "" },
  "queued": 0,
  "failed": 0,
  "blocked": 1,
  "blockedFiles": [
    { "profileId": "default", "path": "/Users/sam/Vapor/locked.txt", "action": "upload",
      "reason": "cannot stat local file: Permission denied", "attempts": 7, "nextAttemptMs": 1795364849500 }
  ],
  "decisions": 1,
  "mirror": { "reverts": 0, "deletes": 0 },
  "restartRequired": null,
  "resources": {
    "cpu": { "limitPercent": 15, "usedPercent": 2 },
    "memory": { "limitPercent": 10, "usedPercent": 1 },
    "bandwidth": { "limitPercent": 25, "usedKbps": 0, "uploadCapacityKbps": 40000, "downloadCapacityKbps": 300000 },
    "idleBoost": { "state": "off", "reason": "user idle for 12s of the required 300s" }
  },
  "profiles": [
    {
      "id": "default", "name": "default", "provider": "gdrive", "syncMode": "two-way",
      "run": { "state": "running", "reason": "watching ~/Vapor" },
      "reconcile": { "state": "idle", "detail": "" },
      "queued": 0, "failed": 0, "blocked": 1, "decisions": 1,
      "mirror": { "reverts": 0, "deletes": 0 },
      "suspended": false, "signInRequired": false, "ignoredPaths": 214
    }
  ]
}
```

- `run.state` is one of `starting`, `running`, `paused`, `error`, and
  `unconfigured`. `unconfigured` means the daemon is up and has no
  usable profile: it syncs nothing, creates no folder, and `run.reason`
  says what to fix. `starting` covers a profile's first check of its
  cloud folder, which runs on a worker so the socket answers at once.
- The daemon-wide `run.state` is the most severe profile's; `run.reason`
  is the daemon's pick (a suspended profile names itself first,
  otherwise the first profile in that state that gives a reason). A
  profile's `run.reason` says what is watched, what blocks sync, or which
  decision it waits on; the throttle reason is separate and never
  overwrites it.
- `decisions` counts open decisions, conflicts included
  (`conflict-resolution.md`).
- `blocked` counts the files that cannot be synced for now: a queued
  item that failed `BLOCKED_AFTER_ATTEMPTS` (5) times in a row, with the
  tries minutes apart (a file that cannot be read, a target that is
  locked, a cloud that does not answer). They are not part of `queued`,
  so a surface does not show "syncing" for them. `blockedFiles` names
  the first ten with the reason of the last try and the time of the
  next one; `vapor diagnostics` lists all. A new event for the path, a
  `vapor reconcile`, and an answer about the path make the next try due
  at once.
- `suspended: true` means the profile is not syncing: it was stopped by
  its configuration or by repeated failures, or its local root is
  missing and Vapor waits for it (`run.reason` says which). A profile
  that could not be composed at all is listed the same way.
  `signInRequired` means the provider refused its sign-in.
- `reconcile.state` is `idle`, `waiting` (a scan is queued and `detail`
  says what it waits for, or a folder that took a burst waits for its
  scan and `detail` says how many changes it holds and when the scan
  starts), or `running` (`detail` names the root).
- `resources.bandwidth.usedKbps` is the measured rate of the last
  seconds. The two capacities are what the daemon measured for each
  direction of the link, and are absent before the first measure.
- `ignoredPaths` is the number of local paths the ignore rules kept out
  of the last whole-scope scan, absent before the first scan.
  `vapor ignored` lists them.
- The daemon-wide `provider` is the configured provider of the profile
  whose state is reported, and `throttle` comes from a profile that is
  running, never from a suspended one's stand-in.

### Provider / auth

- `signInRequired` on a profile row is the auth state the daemon
  serves. `vapor auth status` reads the secret store itself, so it
  answers with the daemon stopped.

### Lifecycle

- Not served over IPC. `vapor service status --json` reads the
  auto-launch setting, the service state, and the crash-loop state
  from `core/lifecycle`, with the daemon running or not. Its `answers`
  key is the one thing it asks the socket: whether a daemon whose
  process runs answers a handshake.

### Controls

- `Pause` / `Resume`, `Reconcile` (the user's on-demand scan). Each
  answers with an `Ack`. Settings are not an IPC control: `vapor config
  set`, the app, and `vapor service` write `vapor.json`, and the daemon
  reloads it.
- A control is for every profile, or for one: the `Call` payload takes
  an optional `profile` (`{"method": "Pause", "profile": "work"}`).
  A name the daemon has no profile for changes nothing: the `Ack` has
  `accepted: false`, and its `note` lists the profiles there are. A
  `Resume` with no profile ends every pause. A `Resume` for one
  profile while the whole daemon is paused resumes that profile and
  leaves the others paused. A profile that was paused by name and is
  composed again (a retry, a configuration reload) starts paused. A
  request for every profile replaces the requests for one profile
  that came before it and were not applied yet. A `Pause` or a
  `Resume` for a profile that does not run now (it is suspended) is
  accepted and holds when the profile runs again; a `Reconcile` for it
  is not accepted, and the `note` gives the reason the profile does
  not run. A profile with `enabled: false` is not a profile the daemon
  has.
- A `Call` that names a method the daemon does not know is answered
  with the `MethodNotFound` error and the name.

### Timeline

- `Timeline` returns the bounded activity buffer: `entries` of
  `{timestampMs, kind, message, profileId}`, newest last, and
  `truncated`.
- `kind` is one of `run-state`, `throttle`, `profile`, `config`,
  `decision`, `conflict` (a file Vapor kept both versions of; the
  message names it, and says how many more when a tick made several),
  `collision`, `mirror`, `cloud-delete` (a file
  Vapor removed in the cloud, with the cause), `failure`, `reconcile`,
  `feed`, `cloud-root`, `sign-in`, and `cloud-full`
  (`constants::timeline`).

### Diagnostics (per item of work)

- For each queued or in-flight item the daemon exposes `id`,
  `profileId`, `path`, `action`, `stage`, `elapsedMs` (time in the
  stage), `attempts`, `error` (the last one), and a human-readable
  `reason` it is not moving (for example "hash workers 2/2 and read
  tokens 1/1 in use", "retry backoff active (attempt 3); next try in
  4 min", "waiting for decision #4"). This is the data source for the "why is this stuck"
  surface.
- `action` says what the item does (`constants::intent::ACTION_*`):
  - `upload` / `download`: content moving to or from the cloud.
  - `rename`: a local rename carried to the cloud.
  - `local-delete`: a delete made on this device, carried to the cloud.
  - `cloud-delete`: a delete made in the cloud, applied on this device.
  - `reconcile`: a subtree the reconcile walk still has to compare.
- `stage` says where it is (`constants::intent::STAGE_*`). Three values
  describe the durable queue, the rest mirror the staged executor:
  - `queued`, in the durable queue, waiting to be leased.
  - `retrying`: hit a transient or rate-limited failure and waits out
    its backoff; it reads `queued` again once the backoff elapses.
  - `held`: parked behind an open decision; the blocker names the
    decision number. Not leased until the decision is answered.
  - `planning`: leased and in the planner.
  - `waiting-to-hash`: planned, waiting for a hash permit (workgate cap
    exhausted, or the throttle disallows hashing).
  - `hashing`: being hashed.
  - `waiting-to-upload`: hashed (or needing no hash), waiting for an
    upload permit.
  - `uploading`: being uploaded.
  - `waiting-to-download`: a cloud change waiting for a download permit.
  - `downloading`: being downloaded.
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
  without error.
- Payload bounds: a `payload_bytes` value above the cap is rejected before
  deserialization.
- Timeline bound: an answer that would pass the frame cap keeps its
  newest entries and sets `truncated`.
- Transport matrix: the same test cases run on every supported transport
  (UDS on macOS/Linux, named pipe on Windows).
