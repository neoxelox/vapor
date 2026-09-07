# Provider Onboarding

How to add a new cloud backend to Vapor. The provider system is
a trait boundary in `core/providers`; the engine never sees a concrete
provider type, so onboarding is additive — no engine changes, no
per-provider `#[cfg]`s, no schema changes.

Reference implementations:

- `core/providers/src/filesystem/` — the canonical local reference
  provider (also the E2E harness backend).
- `core/providers/src/gdrive/` — the first real cloud provider
  (HTTP-based, OAuth, resumable uploads, changes feed).

## The contract you are implementing

`core/providers/src/lib.rs::Provider`:

| Method | Contract |
| ------ | -------- |
| `name()` | Stable identifier; also the `provider` config value. |
| `capabilities()` | Honest `ProviderCapabilities` flags (see below). |
| `content_hash_algorithm()` | The hash the backend can serve cheaply (`Sha256` default, `Md5` for Drive-style metadata hashes). The engine hashes local files with the same algorithm so comparisons are meaningful. |
| `ensure_cloud_sync_directory()` | Resolve-or-create the configured cloud root. Required, no silent-Ok default: a failure blocks sync with an actionable error. |
| `root_identity()` / `adopt_root()` | The cloud root's stable identity, so a re-created or swapped folder at the same path is told apart from the adopted one (`data-flow.md` §Root identity). `root_identity` never creates the root: `NotFound` when missing, `Ok(None)` for a backend without an identity. `adopt_root` writes whatever marker the backend needs (the filesystem provider's `.vapor-root`) and returns the identity; Drive-style backends answer with the folder id for both. |
| `enumerate()` / `stat()` / `content_hash()` | Read-side used by reconcile walks; `enumerate` is non-recursive so walks stay slice-interruptible. |
| `begin_upload()` / `begin_download()` | Return a `TransferSession` that moves a bounded byte budget per `step()`; the engine grants budgets from the bandwidth shaper + auto-tuned step size. Uploads accept `RemotePrecondition` guards (keep-both safety). The completed `TransferOutcome` carries the content hash and the remote object's mtime as the backend reports it after the transfer (`remote_modified_at`); the sync index records that mtime so the reconcile walk can tell a same-size remote edit from an untouched object, so report it whenever the backend has one. |
| `delete()` | Prefer recoverable semantics (Drive moves to trash; filesystem removes). |
| `move_object()` | One-call move of a file to another path, keeping its content and tagging it with the op-id, when `supports_server_side_move`. A missing source is `NotFound`, an occupied destination `PreconditionFailed`; the engine falls back to a plain upload on either. The filesystem provider renames; Drive patches name and parents. |
| `poll_changes()` | Incremental changes feed with an opaque cursor. Return `CursorExpired` when the cursor lapses — the engine reconciles and re-baselines. |

Error taxonomy: every failure maps into `vapor_shared::ProviderErrorKind`
(`Transient`, `RateLimited`, `Authentication`, `PreconditionFailed`,
`NotFound`, `Permanent`). Classification drives retry/backoff, so err on
`Transient` only for genuinely retryable failures and use
`Authentication` for anything a user must fix (include the fixing
command in the message).

## Capability flags

Declare only what the backend truly does — the engine plans around
these flags (`ProviderCapabilities`):

- `supports_remote_changes_feed` — incremental polling; without it the
  engine falls back to periodic reconcile enumeration.
- `supports_server_side_rename` — in-place rename vs delete+reupload.
- `supports_write_preconditions` — upload guards (`HashEquals`,
  `Absent`); without them the engine verifies before upload instead.
- `supports_op_id_tags` — the backend persists the engine's op-id tag
  (xattr / side-file / `appProperties`) and echoes it back through
  entries and changes. Loop prevention's primary correlator; without it
  the content-hash fallback carries loop prevention alone.
- `supports_content_hash_in_listings` — entries carry a hash without an
  extra round trip.

## Onboarding checklist

1. **Module.** `core/providers/src/<name>/mod.rs`; constructor takes
   its dependencies injectable (HTTP transport, secret store, clock)
   so every flow tests offline.
2. **Networking.** All HTTP goes through the `HttpTransport` seam
   (`core/providers/src/http.rs`). Production uses
   `NativeHttpTransport`; tests script `ScriptedHttpTransport`. Never
   contact the network in Tier 1 tests (AGENTS.md §9.1).
3. **Auth.** Tokens live only in `core/platform/secrets::SecretStore`
   under `auth.{profile_id}.{provider}.token`. Document the operational
   flow in `docs/operations/provider-auth-operations.md`.
4. **Constants.** Add the provider name to
   `core/shared/src/constants.rs::provider` (and `ALL`), mirror in the
   Swift `VaporConstants.Providers`, and document the config value in
   the root `README.md` **Configuration** table.
5. **Selection.** Wire the name into `select_provider_for_profile` in
   `core/providers/src/lib.rs`.
6. **Contract suite.** Run the parameterized provider contract tests
   (`core/providers/tests/provider_contract.rs`) against the new
   provider (with a scripted/in-memory backend). This is the gate for
   making the provider selectable — the Google Drive provider was kept
   inert until it passed.
7. **Offline behavior tests.** Scripted-transport tests for: root
   ensure/create, upload (small + chunked/resumable if applicable),
   download, delete, changes mapping, cursor expiry, rate-limit
   classification, auth-failure classification.
8. **Docs + tasks.** Update `docs/architecture/README.md` if you add
   docs, the per-surface task file, and `CHANGELOG.md` in the same
   change set (AGENTS.md §10).

## Engine invariants the provider must respect

- **Never lose intent state**: fail with a classified error; do not
  swallow failures or partially apply without reporting.
- **Chunked interruptibility**: `TransferSession::step` must do a
  bounded amount of work per call and tolerate arbitrarily long gaps
  between calls (throttle `Suspended` holds sessions at checkpoints).
- **Scope discipline**: all remote operations stay under the configured
  cloud sync root (`RemotePath` is validated-relative by construction;
  do not bypass it).
- **Redaction**: never log tokens, auth headers, or raw error bodies
  that may embed them.
