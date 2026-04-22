# macOS architecture

macOS-specific architecture documents. Common (platform-agnostic)
architecture lives one level up in `docs/architecture/`. Read the common
docs first for the portable-runtime mental model; this directory is where
the macOS-specific app lifecycle and transport choices are recorded.

## How to use this group

- **Working on the macOS app?** Start with `app-lifecycle.md` — it
  governs how the SwiftUI app, menubar, and `vapord` daemon interact.
- **Wiring app ↔ daemon communication on macOS?** `ipc-transport.md`
  covers the Unix-domain-socket default (and the optional NSXPC
  wrapping). The transport-agnostic contract lives in
  `docs/architecture/ipc-contracts.md`.
- **Changing macOS lifecycle semantics?** Also revisit
  `docs/operations/macos/launchagent-policy.md` (plist policy,
  crash-loop interaction) — the architecture and operations docs must
  stay aligned.

## Documents

- `app-lifecycle.md` — component model (main window vs menubar vs
  daemon), expected lifecycle behavior (login startup, window close,
  reopen, quit), crash-loop coordination with `core/lifecycle`, and the
  bundle layout (`Vapor.app/Contents/MacOS/{Vapor,vapord}`).
- `ipc-transport.md` — macOS-specific IPC transport: default Unix
  domain socket under `<vapor_dir>/vapord.sock`, permissions, framing,
  optional NSXPC wrapping, shutdown semantics, and test harness.
