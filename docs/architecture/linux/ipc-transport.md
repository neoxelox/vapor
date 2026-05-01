# Linux IPC transport

The contract surface is defined in `docs/architecture/ipc-contracts.md`
(transport-agnostic). This document covers only the Linux-specific
transport choice. The Linux native implementation reuses the Unix
domain socket code from `core/ipc/src/transport.rs::unix_impl`, so
this document is a short delta on top of `macos/ipc-transport.md`.

## Transport

- **Default:** Unix domain socket at `<vapor_dir>/vapord.sock` with
  restrictive owner-only permissions (`0o600`).
- **No NSXPC equivalent:** unlike macOS there is no platform XPC
  wrapping. If a sandboxing layer becomes needed (e.g. running the
  daemon under a `systemd-run --user --scope` unit with a private
  namespace), it goes underneath the same UDS contract.

## Permissions and discovery

- Socket is created by the daemon on startup at a deterministic path
  under `<vapor_dir>`.
- Clients (`vapor` CLI, future `apps/linux`) discover the socket
  using the same `VAPOR_DIR` resolution as every other runtime
  artifact.
- On install / update / unclean exit the daemon removes any stale
  socket file before `bind()`. Linux deals with stale UDS sockets the
  same way macOS does (the `bind` call would otherwise fail with
  `EADDRINUSE`).

## Framing

Length-prefixed frames, 32-bit little-endian length header followed by
a UTF-8 JSON-RPC 2.0 body. Frame length bound by
`vapor_shared::constants::ipc::MAX_PAYLOAD_BYTES` (default
`4 * 1024 * 1024`). Identical to the macOS framing — the codec under
`core/ipc/src/framing.rs` is shared across every Unix transport.

## Shutdown

When the daemon receives a stop request (`SIGTERM` from
`systemctl --user stop vapord` or any `kill TERM`), it:

1. Closes the listening socket and lets the `Drop` on the listener
   handle clean up the socket file.
2. Drains in-flight requests to a clean completion or returns a
   `Shutting Down` error frame.
3. Exits at the next tick boundary.

## Testing

- The `core/ipc/tests/skew_matrix.rs` integration tests run unchanged
  on Linux because the UDS path is shared with macOS. Wave 13 adds a
  full `ubuntu-latest` test job that exercises the suite against the
  packaged daemon binary; until then the lint-only Linux CI leg keeps
  cross-OS compilation gated.
