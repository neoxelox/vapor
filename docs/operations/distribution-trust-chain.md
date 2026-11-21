# Distribution Trust Chain

Vapor ships across multiple platforms. Each platform has its own signing,
packaging, and trust chain. This document is the cross-platform index; the
concrete policy for each platform lives in the per-platform doc below.

## Per-platform policy

- macOS: `docs/operations/macos/distribution-trust-chain.md`
- Windows: (placeholder, lands when `apps/windows` starts)
- Linux: (placeholder, lands when `apps/linux` starts)
- CLI (`vapor` binary): it has no artifact of its own. It ships inside
  each platform's install package, signed with it, and the installer
  puts it on the `PATH`.

## Shared principles (apply to every platform)

- Release artifacts are produced from a script-first pipeline, not an IDE
  archive flow.
- Each shipping platform owns an isolated GitHub Environment for its
  release secrets (`release-macos`, `release-windows`, and
  `release-linux` exist with identical protection; each platform's
  secrets go in when its surface ships). The `vapor` CLI has no
  environment of its own: it is signed and published inside each
  platform's install package, under that platform's environment.
- A platform ships install packages named `Vapor-<os>-<arch>.<ext>`.
  Each holds the app, the CLI, and the daemon, so a user installs one
  thing and updates one thing. A signature is inside the file that it
  signs: a release holds no detached signature file, and
  `Checksums.txt` is not signed.
- Each of those environments is protected before its secrets are added:
  deployments restricted to a `v*` tag rule with no branch rule, and a
  required reviewer gating the signing job. A new environment starts
  with no protection at all, so this is a per-platform setup step, not
  something inherited from the macOS one. Rule: `AGENTS.md` §7.1;
  procedure and current status: `docs/operations/release-process.md`.
- Each platform's signing secrets and notarization/signing tools never
  cross-leak into another platform's release job.
- Signing follows the tag. A stable tag ships only signed artifacts on
  every platform, and a platform's `package` job refuses to run a stable
  tag without its signing material. A prerelease tag ships unsigned on
  every platform when no material is configured, and signed when it is.
  The same check, in the same place, on every platform: the first step
  of the platform's `package` matrix entry.
- Rollback artifacts are preserved per platform for every release.
- Trust chain incidents follow the platform-specific incident playbook
  (starting point: `docs/operations/release-incident-playbook.md`, which
  links into per-platform sections as they land).
