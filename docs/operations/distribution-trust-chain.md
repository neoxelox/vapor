# Distribution Trust Chain

Vapor ships across multiple platforms. Each platform has its own signing,
packaging, and trust chain. This document is the cross-platform index; the
concrete policy for each platform lives in the per-platform doc below.

## Per-platform policy

- macOS: `docs/operations/macos/distribution-trust-chain.md`
- Windows: (placeholder, lands when `apps/windows` starts)
- Linux: (placeholder, lands when `apps/linux` starts)
- CLI (`vapor` binary): signing follows the host OS policy (Developer ID
  on macOS, EV cert on Windows, GPG signature on Linux). The CLI ships
  alongside the platform installers under the same GitHub Release tag.

## Shared principles (apply to every platform)

- Release artifacts are produced from a script-first pipeline, not an IDE
  archive flow.
- Release secrets live in an isolated GitHub Environment, never as
  repository-wide secrets. Today that is the single `release`
  environment (macOS signing/notarization); each new shipping platform
  gets its own environment (`release-windows`, `release-linux`).
- Each platform's signing secrets and notarization/signing tools never
  cross-leak into another platform's release job.
- Rollback artifacts are preserved per platform for every release.
- Trust chain incidents follow the platform-specific incident playbook
  (starting point: `docs/operations/release-incident-playbook.md`, which
  links into per-platform sections as they land).
