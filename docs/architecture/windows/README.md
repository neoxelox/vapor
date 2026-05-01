# Windows architecture

Windows-specific architecture documents. Common (platform-agnostic)
architecture lives one level up in `docs/architecture/`. Read the
common docs first; this directory is where Windows-specific transport
choices and lifecycle decisions are recorded.

The native Windows surfaces (`apps/windows`, the
`core/platform/*::windows` impls, the named-pipe transport) are part
of the optional Wave 12 work and are not committed deliverables. Until
that wave lands, this directory is documentation-only — the runtime
trait stubs return `Unsupported` on Windows.

## How to use this group

- **Wiring app ↔ daemon communication on Windows?** `ipc-transport.md`
  covers the named-pipe transport choice. The transport-agnostic
  contract lives in `docs/architecture/ipc-contracts.md`.

## Documents

- `ipc-transport.md` — Windows-specific IPC transport: named pipe at
  `\\.\pipe\vapord-<user-sid>`, security descriptor, framing,
  shutdown semantics, test harness.
