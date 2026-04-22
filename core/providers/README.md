# providers

Rust provider modules implementing cloud integrations. Providers are
consumed by the daemon via the `Provider` trait in `core/providers/src/lib.rs`
and are deliberately provider-neutral from the engine's perspective.

## Current state

- `FilesystemStubProvider` — pre-GA default, inert. Reports no remote
  changes feed and no server-side rename, so the daemon stays in a known
  quiet state until a real provider is selected.
- `GoogleDriveProvider` — compiled-in but inert until the bidirectional
  runtime shell (Phase C8) and provider contract tests (Phase C8-43..47)
  stabilize. Lands in Phase C8-48 onward.

## Planned providers

- `provider_filesystem` (loopback local) — the Phase C8 reference provider
  used to validate every provider-neutral bidirectional mechanic
  (self-write cache, op-id correlation, remote-to-local apply, provider
  cursor) before any external provider ships. Reused as the contract-test
  harness for future providers.
- `provider_gdrive` — first external cloud target, OAuth (PKCE) with
  tokens stored via `core/platform/secrets::SecretStore` (Keychain on
  macOS, Credential Manager on Windows, Secret Service on Linux, age-file
  fallback for headless Linux).
- Additional adapters (for example iCloud, S3, R2, Proton Drive) come
  through the provider-system extensibility hardening pass and are not
  part of the first release.

All providers implement the shared trait and map errors into the
provider-neutral error taxonomy (`Transient`, `RateLimited`,
`Authentication`, `PreconditionFailed`, `NotFound`, `Permanent`) defined in
`core/shared`.

Logging:

- Provider modules use shared structured logging from `vapor-shared`.
- Default provider logs target `<vapor_dir>/logs/vapord.logs` where
  `vapor_dir` comes from `VAPOR_DIR`.
