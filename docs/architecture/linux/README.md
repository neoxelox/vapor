# Linux architecture

Linux-specific architecture documents. Common (platform-agnostic)
architecture lives one level up in `docs/architecture/`. Read the
common docs first; this directory captures Linux-specific transport
and lifecycle choices.

The native Linux surfaces (`apps/linux`, the `core/platform/*::linux`
impls) are part of the optional Wave 13 work and are not committed
deliverables. Until that wave lands, this directory is
documentation-only — the runtime trait stubs return `Unsupported` on
Linux.

## How to use this group

- **Wiring app ↔ daemon communication on Linux?** `ipc-transport.md`
  covers the Unix-domain-socket choice (shared with macOS).
  The transport-agnostic contract lives in
  `docs/architecture/ipc-contracts.md`.

## Documents

- `ipc-transport.md` — Linux-specific IPC transport: Unix domain
  socket under `<vapor_dir>/vapord.sock`, owner-only permissions,
  framing, shutdown semantics, test harness.
