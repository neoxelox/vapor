# Windows IPC transport

The contract surface is defined in `docs/architecture/ipc-contracts.md`
(transport-agnostic). This document covers the Windows-specific
transport choice. The native implementation lands with Wave 12
(`docs/tasks/core.md` C6 named-pipe work); until then
`vapor_ipc::transport::bind_listener` returns
`TransportError::Unsupported` on Windows.

## Transport

- **Default:** Win32 named pipe at
  `\\.\pipe\vapord-<user-sid>`. Per-user scoping via the SID prevents
  cross-user reuse on multi-user hosts (matches the macOS / Linux UDS
  per-user-directory model).
- **Listener creation:** the daemon calls `CreateNamedPipeW` with
  `PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED`,
  `PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT`, and a security
  descriptor that grants Read/Write only to the current user.
- **Stale-instance recovery:** opening a new pipe instance with the
  same name is benign; the daemon does not need a stale-socket cleanup
  step the way the UDS path does, because Windows scopes pipe
  instances per-process.

## Permissions and discovery

- The pipe security descriptor restricts access to the running user
  (DACL with one ACE granting `GENERIC_READ | GENERIC_WRITE` to
  the process owner; no inherited ACEs).
- Clients (`vapor` CLI, future `apps/windows`) discover the pipe by
  composing the same `<user-sid>` they get from
  `OpenProcessToken` / `GetTokenInformation(TokenUser)`. They do not
  need access to `<vapor_dir>` to find the pipe.

## Framing

Length-prefixed frames, 32-bit little-endian length header followed by
a UTF-8 JSON envelope (`{kind, payload}` requests, `{outcome, value}`
responses; not JSON-RPC). Frame length bound by
`vapor_shared::constants::ipc::MAX_PAYLOAD_BYTES` (default
`4 * 1024 * 1024`). The exact same framing applies on UDS (macOS /
Linux) so cross-platform clients reuse one codec.

## Shutdown

When the daemon receives a service-stop request (Wave 12 wires
`SetConsoleCtrlHandler` + `SERVICE_STOP` + `WM_ENDSESSION` through
`core/platform::ProcessSupervisor`), it:

1. Calls `DisconnectNamedPipe` on the listening instance to refuse
   further connections.
2. Drains in-flight requests to a clean completion or returns a
   `Shutting Down` error frame.
3. Exits at the next tick boundary.

## Testing

- The `windows-latest` job already runs the Rust test suite on every
  pull request; the UDS-bound skew-matrix tests are `cfg(unix)`. Wave 12
  runs them against the named-pipe transport. The skew matrix,
  payload-bounds rejection, and field-omission tolerance are
  transport-agnostic.
- Until that wave lands, the in-process IPC unit tests in
  `core/ipc/src/{framing,protocol,server}.rs` continue to give
  cross-OS coverage.
