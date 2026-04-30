# core/ipc

IPC channel between Vapor surfaces (`vapor` CLI, macOS app, future
Windows / Linux apps) and the daemon (`vapord`).

Authoritative reference: `docs/architecture/ipc-contracts.md`.
Tasks: `docs/tasks/core.md` Phase C5.

## Wire format

- Frames: `u32` little-endian length prefix + UTF-8 JSON body.
- Bound: `vapor_shared::constants::ipc::MAX_PAYLOAD_BYTES` (default
  4 MiB). Frames whose declared length exceeds the cap are rejected
  before allocation.
- Handshake: every session opens with a `Hello` request and the server
  responds with a `HelloAck`. Pre-GA the supported skew window is
  `|client - daemon| <= 1`. Outside that, the server returns
  `IncompatibleVersion` and closes the session.

## What lives here

- `protocol` — wire-format types (`Hello`, `HelloAck`, `Request`,
  `Response`, `ResponseBody`, `ErrorBody`, `StatusResponse`, `Method`).
- `framing` — length-prefixed frame codec.
- `transport` — per-OS transport: Unix-domain-socket on Unix, stub on
  Windows until Wave 12 lands the named-pipe transport.
- `server` — `Service` trait + `serve_connection` that drives one
  client session through the handshake-then-RPC loop.
- `client` — high-level `Client::connect` / `client.status()` API.
- `tests/skew_matrix.rs` — end-to-end integration tests that spin up a
  real Unix-domain-socket server and exercise every supported version
  pair plus the documented negative cases (skew of 2 rejected,
  oversized first frame closes the connection).

## Wave status

- **Done (Wave 6 phase 2):** trait + framing + UDS transport +
  Status method end-to-end. 18 unit + 5 integration tests cover the
  handshake, skew matrix, payload bounds, and forward-compat field
  tolerance.
- **Pending (Wave 7):** `pause`, `resume`, `flush_now`, `reconcile`,
  `timeline`, `auto_launch_toggle`, `config_update` methods.
- **Pending (Wave 12):** Windows named-pipe transport.

## Daemon-side wiring

`core/daemon/src/ipc_service.rs` adapts the live `DaemonApp`
into the wire-format `StatusResponse`. The daemon's `main.rs` spawns
the IPC server on startup via `core/daemon/src/ipc_server.rs`; the
returned handle drops the socket file on clean shutdown.
