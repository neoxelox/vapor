# macOS IPC transport

The contract surface is defined in `docs/architecture/ipc-contracts.md`
(transport-agnostic). This document covers only the macOS-specific
transport choice.

## Transport

- **Default:** Unix domain socket at `<vapor_dir>/vapord.sock` with
  restrictive owner-only permissions (`0o600`).
- **Optional wrapping:** If sandboxing or cross-process capability delegation
  becomes a requirement, an NSXPC wrapper can be layered on top of the same
  JSON-RPC contract. NSXPC is not required for MVP; the Unix domain socket
  is sufficient.

## Permissions and discovery

- Socket is created by the daemon on startup at a deterministic path under
  `<vapor_dir>`.
- Clients (Swift app, `vapor` CLI, future tools) discover the socket using
  the same `VAPOR_DIR` resolution as every other runtime artifact.
- On install/update the daemon must remove any stale socket file at startup
  before `bind()` (after verifying no live process is attached).

## Framing

Length-prefixed frames, 32-bit big-endian length header followed by a
UTF-8 JSON-RPC 2.0 body. Frame length bound by `IPC_MAX_PAYLOAD_BYTES`
(default `4 * 1024 * 1024`). Oversized frames are rejected before
deserialization.

## Shutdown

When the daemon receives `SIGTERM` (from `launchctl kill TERM` or the
Swift app's `stopDaemon`), it:

1. Closes the listening socket.
2. Drains in-flight requests to a clean completion or returns a `Shutting
   Down` error frame.
3. Exits at the next tick boundary.

## Testing

- Local integration tests spawn a daemon in a temp `VAPOR_DIR`, bind a
  UDS, run the full handshake + control matrix, and tear down.
- The same test harness runs on Linux unchanged; the Windows transport
  test (named pipe) uses an equivalent harness.
