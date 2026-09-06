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
- `transport` — per-OS transport: Unix domain socket on Unix; the
  Windows named pipe is a stub until Wave 12.
- `server` — `Service` trait + `serve_connection` that drives one
  client session through the handshake-then-RPC loop.
- `client` — high-level `Client::connect` / `client.status()` API.
- `tests/skew_matrix.rs` — end-to-end integration tests that spin up a
  real Unix-domain-socket server and exercise every supported version
  pair plus the documented negative cases (skew of 2 rejected, an
  oversized first frame answered with `PayloadTooLarge` and then EOF).

## Status

Every method the surfaces use is implemented end to end: `status`,
`pause`, `resume`, `flush_now`, `reconcile`, `timeline`, `diagnostics`,
`set_auto_launch`, and `update_excludes`. The Windows named-pipe
transport is the one open item (Wave 12 in `docs/tasks/README.md`).

## Daemon-side wiring

`core/daemon/src/ipc_service.rs` adapts the live `DaemonApp`
into the wire-format `StatusResponse`. The daemon's `main.rs` spawns
the IPC server on startup via `core/daemon/src/ipc_server.rs`; the
returned handle drops the socket file on clean shutdown.
