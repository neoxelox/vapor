# Security Policy

Vapor keeps a local folder and a cloud folder in bidirectional sync. It
holds provider OAuth tokens, runs a background daemon, and has write
access to whatever you point it at. Security reports are welcome and
taken seriously.

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

Report privately through either channel:

1. **GitHub Private Vulnerability Reporting** (preferred) — open the
   [Security tab](https://github.com/neoxelox/vapor/security/advisories)
   and choose *Report a vulnerability*. This keeps the report, the fix,
   and the disclosure in one place.
2. **Email** — `me@arn.sh`.

A useful report includes the affected version (`vapor --version` or the
`VERSION` file), the platform, what an attacker gains, and the smallest
reproduction you can manage. A sandboxed repro under `.vapor/` is ideal;
please never attach real cloud credentials or personal file contents.

### What to expect

Vapor is pre-GA and maintained by one person, so treat these as
good-faith intentions rather than guarantees: an acknowledgement within
about a week, an assessment of severity and scope after that, and credit
in the release notes when a report leads to a fix — tell us how you want
to be named, or if you would rather stay anonymous.

Please give us a reasonable window to ship a fix before disclosing
publicly. We will tell you when the fix lands and coordinate timing with
you.

## Supported versions

| Version                | Supported |
| ---------------------- | --------- |
| Latest release         | ✅        |
| Any earlier release    | ❌        |

Vapor is pre-GA and under heavy active development. There are no
security backports: fixes land on `main` and ship in the next release.
Config, state, and schema formats may change without migration
(`AGENTS.md` §1.1), so always report against the latest release or
`main`.

## Scope

Most valuable to look at:

- **Secret handling** — provider tokens are held through
  `core/platform/src/secrets.rs` (Keychain on macOS). Anything that
  writes a token to disk, a log, or an argv is a real finding.
- **Log redaction** — `core/shared/src/logging.rs` and
  `core/providers/src/logging.rs` must keep tokens, auth headers, and
  sensitive identifiers out of logs and support bundles
  (`vapor support bundle`).
- **OAuth flow** — `core/providers/src/gdrive/oauth.rs`: PKCE handling,
  the loopback redirect, state validation, refresh handling.
- **IPC** — `core/ipc/*`. The daemon listens on a Unix domain socket.
  Anything that lets another local user or process drive the daemon,
  read its state, or bypass the version handshake.
- **Sync-root escape** — `core/daemon/src/sync_directories.rs` and
  `core/providers/src/paths.rs`. Vapor must never read or write outside
  the configured local and cloud roots. Path traversal, symlink
  escapes, and `..` handling all count.
- **Data-destroying behaviour** — the mass-deletion guard
  (`core/daemon/src/safeguards.rs`), conflict resolution
  (`core/daemon/src/conflict.rs`), and loop prevention
  (`core/daemon/src/self_write_cache.rs`). Anything that silently
  overwrites or destroys user data in `two-way` mode is a security-class
  bug to us, not just a sync bug.
- **Lifecycle** — `core/lifecycle/*` and the macOS LaunchAgent
  (`docs/operations/macos/launchagent-policy.md`): privilege escalation,
  hijackable launch paths, or persistence abuse.

Explicitly out of scope:

- **Unsigned pre-GA binaries.** Release artifacts are not yet code-signed
  or notarized, so Gatekeeper will refuse them and their authenticity
  cannot be verified. This is known and tracked, not a finding. Build
  from source if that matters to you.
- **One-way sync modes destroying data.** `pull-only` and `push-only`
  are documented strict mirrors that intentionally overwrite the
  non-authoritative side (`AGENTS.md` §4,
  `docs/architecture/sync-modes.md`). Data loss there is the specified
  behaviour. A way to *enter* those modes without the up-front warning
  is a finding.
- Attacks that require an already-compromised device or root.
- UI rendering issues with no security consequence.
- Reports from automated scanners with no demonstrated impact.

## Handling your report

Reports are read only by the maintainer. We will not share your report
or your identity with third parties without asking you first.
