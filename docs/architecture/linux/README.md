# Linux architecture

Linux-specific architecture documents. Common (platform-agnostic)
architecture lives one level up in `docs/architecture/`. Read the
common docs first; this directory captures Linux-specific transport
and lifecycle choices.

The `core/platform/*::linux` implementations exist: the daemon and
the `vapor` CLI run on Linux with inotify, a systemd user unit, the
freedesktop trash, `/proc` and `/sys` throttle inputs, and a secret
store backed by `VAPOR_SECRETS_COMMAND` or the desktop Secret Service
(`docs/architecture/platform-abstractions.md` has every row). The
Linux app (`apps/linux`), the trust chain, and the release lane are
still to come; until they land, Linux is not a shipping surface.

## How to use this group

- **Wiring app ↔ daemon communication on Linux?** `ipc-transport.md`
  covers the Unix-domain-socket choice (shared with macOS).
  The transport-agnostic contract lives in
  `docs/architecture/ipc-contracts.md`.

## Documents

- `ipc-transport.md` — Linux-specific IPC transport: Unix domain
  socket under `<vapor_dir>/vapord.sock`, owner-only permissions,
  framing, shutdown semantics, test harness.
