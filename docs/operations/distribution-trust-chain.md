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
- Each shipping platform owns an isolated GitHub Environment for its
  release secrets (`release-macos` today; `release-windows` /
  `release-linux` when those platforms ship). The `vapor` CLI has no
  environment of its own: its artifacts are signed and published by
  each platform's release job under that platform's environment.
- Each platform's signing secrets and notarization/signing tools never
  cross-leak into another platform's release job.
- Rollback artifacts are preserved per platform for every release.
- Trust chain incidents follow the platform-specific incident playbook
  (starting point: `docs/operations/release-incident-playbook.md`, which
  links into per-platform sections as they land).
