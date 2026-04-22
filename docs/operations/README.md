# Operations

Runbooks and operational policy for running, releasing, and supporting
Vapor. Use this directory when you need to configure secrets, cut a
release, respond to an incident, or understand the runtime's logging and
localization behavior.

Common (cross-platform) operations live at the top of this directory;
per-platform specifics (signing, service install mechanics, per-OS
incident scenarios) live in per-platform subdirectories.

## How to use this group

- **Cutting a release?** Start at `release-process.md`. It points at the
  per-platform distribution trust chain and at `release-incident-playbook.md`
  if something goes wrong mid-flight.
- **Configuring provider auth / Keychain / secret store?** Read
  `provider-auth-operations.md`; secret-store mechanics per OS live in
  `docs/architecture/platform-abstractions.md` under `SecretStore`.
- **Diagnosing a production issue?** `runtime-logging-and-localization.md`
  documents log paths, redaction rules, and language fallback behavior.
- **Shipping on a new platform?** The `distribution-trust-chain.md`
  index shows what each platform needs.

## Common documents

- `distribution-trust-chain.md` — cross-platform index: signing,
  packaging, and trust-chain principles shared across macOS / Windows /
  Linux / CLI, plus pointers into each platform's concrete policy.
- `release-process.md` — the release runbook: versioning governance,
  `VERSION` source-of-truth, tag discipline, script-first packaging,
  environment gating, publication flow.
- `release-incident-playbook.md` — detection / mitigation / verification
  steps for common release-time failures (signing, notarization,
  publishing).
- `provider-auth-operations.md` — OAuth (PKCE) flow, token lifecycle,
  refresh policy, and degraded-auth behavior. Transport-neutral; secret
  storage delegates to `core/platform::SecretStore`.
- `runtime-logging-and-localization.md` — `VAPOR_DIR` runtime layout,
  log level override via `VAPOR_LOG_LEVEL`, structured log line format,
  redaction markers, locale catalog resolution (`assets/locales/*.json`),
  `languageCode` with English fallback.

## Per-platform subdirectories

- `macos/` — macOS LaunchAgent plist policy and crash-loop interaction,
  macOS-specific distribution trust chain (Developer ID, hardened runtime,
  notarization, entitlement review).
- (`windows/` and `linux/` subdirectories will be added when those app
  surfaces start — signed installer policy, service install mechanics,
  per-OS incident playbooks.)

Runbooks expand here as implementation matures. Platform-specific
runbooks belong under the matching per-platform subdirectory.
