# Provider Onboarding

How to add a new cloud backend to Vapor. The provider system is
a trait boundary in `core/providers`; the engine never sees a concrete
provider type, so onboarding is additive, no engine changes, no
per-provider `#[cfg]`s, no schema changes.

Reference implementations:

- `core/providers/src/filesystem/`: the canonical local reference
  provider (also the E2E harness backend).
- `core/providers/src/gdrive/`: the first real cloud provider
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
| `begin_upload()` / `begin_download()` | Return a `TransferSession` that moves a bounded byte budget per `step()`. The session names its own step size (`step_bytes`, the chunk the backend wants) and, when it knows, what is left to move (`remaining_bytes`); the engine grants each step from the bandwidth bucket and never asks for more than is left. Uploads take a `RemotePrecondition` guard (keep-both safety), and every backend must honour it: `HashEquals` and `Absent` are part of the contract, not a capability. A backend that cannot make the check of `HashEquals` and the write one step must find a version that the write replaced without having compared it, and keep it next to the file as a copy named with `vapor_shared::conflict::copy_name` and `constants::conflict::REPLACED_VERSION` (`data-flow.md` §Conflict handling): the filesystem provider exchanges the two files, Google Drive reads the stored versions after the upload. The completed `TransferOutcome` carries the content hash and the remote object's mtime as the backend reports it after the transfer (`remote_modified_at`); the sync index records that mtime so the reconcile walk can tell a same-size remote edit from an untouched object, so report it whenever the backend has one. |
| `delete()` | Prefer recoverable semantics (Drive moves to trash; filesystem removes). |
| `move_object()` | One-call move of a file to another path, keeping its content and tagging it with the op-id, when `supports_server_side_move`. A missing source is `NotFound`, an occupied destination `PreconditionFailed`; the engine falls back to a plain upload on either. The filesystem provider renames; Drive patches name and parents. |
| `poll_changes()` | Incremental changes feed with an opaque cursor. Return `CursorExpired` when the cursor lapses: the engine reconciles and re-baselines. |

Error taxonomy: every failure maps into `vapor_shared::ProviderErrorKind`
(`Transient`, `RateLimited`, `Authentication`, `PreconditionFailed`,
`NotFound`, `CloudRootUnavailable`, `StorageFull`, `Permanent`).
Classification drives retry/backoff, so err on `Transient` only for
genuinely retryable failures and use `Authentication` for anything a
user must fix (include the fixing command in the message).
`CloudRootUnavailable` means the cloud root itself is gone: the engine
holds the profile and checks the root again, and no intent fails for
it. `StorageFull` means the cloud has no room for an upload: the upload
waits and is tried again each 15 minutes without spending an attempt
(at once when the user asks for a sync), and downloads and deletions
continue.

## Capability flags

Declare only what the backend truly does: the engine plans around
these flags (`ProviderCapabilities`):

- `supports_remote_changes_feed`: incremental polling; without it the
  engine falls back to periodic reconcile enumeration.
- `supports_server_side_move`: `move_object` moves a file with no
  re-upload; without it a rename is a delete and an upload.

Nothing else is a flag, because the engine does not plan around
anything else. An entry or a change carries the engine's op-id tag when
the backend has one for that object (xattr, side-file,
`appProperties`), and none when it has not: loop prevention uses the
tag when it is there and the content hash when it is not. A change
also carries the object's size and mtime when the backend reports them
(`RemoteChange::size_bytes`, `modified_at`); the engine needs both to
tell an in-place edit of a tagged object from an echo. An entry carries
a content hash when the listing has one.

## Onboarding checklist

1. **Module.** `core/providers/src/<name>/mod.rs`; constructor takes
   its dependencies injectable (HTTP transport, secret store, clock)
   so every flow tests offline.
2. **Networking.** All HTTP goes through the `HttpTransport` seam
   (`core/providers/src/http.rs`). Production uses
   `NativeHttpTransport`; tests script `ScriptedHttpTransport`. Never
   contact the network in Tier 1 tests (AGENTS.md §9.1). The native
   transport reads no proxy setting and uses its bundled root
   certificates, so a network that inspects TLS cannot sync yet
   (`docs/tasks/core.md`).
3. **Auth.** Tokens live only in `core/platform/secrets::SecretStore`
   under `auth.{profile_id}.{provider}.token`, opened through
   `open_store` so the runtime directory's scope applies
   (`platform-abstractions.md` §`SecretStore`). Document the operational
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
   making the provider selectable: the Google Drive provider was kept
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
- **A completed upload is a verified upload**: the outcome carries the
  hash of the bytes the backend stored. A backend that reports its own
  checksum compares it with the hash of what was sent and fails the
  transfer on a mismatch.
- **Fail closed**: a provider name the build does not know, or a
  profile with no provider, gets `SuspendedProvider`, which refuses
  every call. `FilesystemStubProvider` is for tests only.
- **Scope discipline**: all remote operations stay under the configured
  cloud sync root (`RemotePath` is validated-relative by construction;
  do not bypass it).
- **Redaction**: never log tokens, auth headers, or raw error bodies
  that may embed them.

## What Google Drive cannot carry

The Drive provider maps a Drive folder onto a file tree, and four
things do not map. Each one is skipped and reported, never forced:

- Google Docs, Sheets, Slides, and other files with no bytes of their
  own, and shortcuts: skipped.
- Two files with one name in one folder: one of them syncs (the one
  with the lowest id, so the choice is stable), and a warning names the
  other. A change of the other file is not a change of the path, and
  its removal or its rename is not the removal of the file that syncs:
  the changes feed asks Drive one time which file a path names before
  it reports the first change of that path. Giving the second file a
  local name of its own is open work (`docs/tasks/core.md`).
- A name in another spelling (`README.md` for `readme.md`), which a
  Drive query can answer for: a lookup binds a path only to a file
  whose name is exactly the name asked for.
- Two uploads into one new folder at the same time: the lookup and the
  creation of a folder run one at a time, so the folder is made one
  time. A folder id found in the cache is confirmed with Drive before
  an upload goes into it when the last confirmation is older than 5
  seconds, so a folder that was trashed or moved out does not take the
  file.
- A name with `/`: the file stays in the cloud only.
- A deletion made in the cloud while the daemon was not running is
  found by the startup reconcile, not by the feed. Keeping the Drive
  file id in the sync index, so the feed alone is enough, is open work
  (`docs/tasks/core.md`).
