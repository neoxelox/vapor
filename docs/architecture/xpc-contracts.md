# XPC Contracts

This document defines the contract surface between `apps/macos` and `core/daemon`.

## Contract versioning rules

Every request and response payload carries an explicit `schema_version: u32` field at the top level. The concrete version handshake and skew tolerance rules below are pre-GA; backward-compat guarantees tighten at GA.

### Handshake

- On connection, the app sends a `Hello { app_schema_version }` request. The daemon responds with `HelloAck { daemon_schema_version, supported_min_version }`.
- If `app_schema_version < daemon.supported_min_version` or `daemon_schema_version < app.supported_min_version`, the handshake fails with `IncompatibleVersion { peer_version, required_min }`. Both sides log a structured diagnostic (`xpc.handshake.incompatible` with both versions) and the app surfaces an actionable "Please update Vapor" state. The daemon does not accept further requests from that session.
- If both sides are within their supported window, the session proceeds. No per-request version negotiation is required after the handshake.

### Field omission tolerance (forward compatibility)

- **Unknown fields on the receiving side are ignored, not rejected.** Daemon-N receiving a request serialized by app-(N-1) may see fewer fields than the current N schema defines; missing fields take type-specific defaults: numeric = `0`, boolean = `false`, string = `""`, optional = `None`, array = `[]`. Unknown fields added by a newer peer are logged at debug level (`xpc.unknown_field` with the field name) and discarded.
- **Response field omission is symmetric.** App-(N-1) receiving a response from daemon-N applies the same defaulting rules for fields it does not know about.
- **Additive-only is preferred.** Within a schema version, breaking changes (field removal, type change, semantic reinterpretation of an existing field) require a new `schema_version` bump and a new `supported_min_version` floor on the side that can no longer interpret the old shape.

### Removal policy

- Removing a field requires a two-step migration: first mark the field `deprecated = true` in the schema and ignore it on the receiving side while still accepting it on the sending side; second, at the next `schema_version` bump, remove it and raise `supported_min_version` to the bump point.
- Pre-GA exception: the project owner may fast-track field removals without the two-step migration when both app and daemon ship in the same release artifact. The release notes must call out the breaking shape change.

### App/daemon skew matrix

Pre-GA supported skew is `|app_schema_version - daemon_schema_version| <= 1`. Concretely:

- `app-N` ↔ `daemon-N`: fully supported.
- `app-N` ↔ `daemon-(N-1)`: supported if `N-1 >= app.supported_min_version`. App omits N-only fields on send; daemon returns N-1 responses that the app defaults missing fields on.
- `app-(N-1)` ↔ `daemon-N`: supported if `N-1 >= daemon.supported_min_version`. Daemon defaults missing fields on receive; app ignores N-only fields in response.
- `|N - M| >= 2`: not supported; handshake returns `IncompatibleVersion`.

### Payload size bounds

- Every XPC payload carries a declared `payload_bytes` hint and is bounded at the transport layer at `XPC_MAX_PAYLOAD_BYTES` (defined in `core/shared/src/constants.rs`, default `4 * 1024 * 1024`). Oversized payloads fail with `PayloadTooLarge` before any deserialization attempt and are logged with the declared size.
- Streamed endpoints (diagnostics timeline, activity events) use chunked frames; each frame is bounded independently.

## Contract groups

### Status

- Running state, throttle state, throttle reason, queue depth (total + per-profile), last sync markers, effective resource ceilings (cpu/memory/bandwidth), current utilization, idle-boost state and reason.

### Provider/auth

- Auth state, token-refresh health, provider capability flags, last auth error classification (per profile).

### Lifecycle

- Auto-launch enabled state, last launch result, crash-loop pause state, daemon uptime, startup-reconstruction-barrier state.

### Controls

- Pause/resume, flush-now, excludes update, auto-launch toggle, config reload trigger.

### Diagnostics (per-intent)

- For each pending durable intent the daemon exposes: `intent_id`, `profile_id`, `path`, `action`, current `stage` (e.g., `Queued`, `WaitingForHash`, `Hashing`, `WaitingForUpload`, `Uploading`, `Retrying`, `DeferredReconcile`), elapsed-in-stage, attempt count, last-error classification, and a human-readable `blocker_reason` (e.g., "Throttle Suspended: no uploads allowed", "Hash worker cap 2/2 in use", "Rate-limit slowdown until {iso8601}"). This is the data source for the diagnostics "why is this intent stuck" surface.

## Test matrix

The Phase 6 XPC implementation (P6-1 onward) must include:

- Handshake: `app-N ↔ daemon-N`, `app-N ↔ daemon-(N-1)`, `app-(N-1) ↔ daemon-N`, and `|N - M| = 2` negative case (expects `IncompatibleVersion`).
- Field omission: request serialized without an N-only field is handled by daemon-N without error and uses default values.
- Unknown field: request carrying a synthetic `future_field` is handled without error and logged at debug level.
- Payload bounds: a `payload_bytes` value above the cap is rejected before deserialization.
- Stream frame bounds: a single oversized timeline frame is rejected without tearing down the stream.
