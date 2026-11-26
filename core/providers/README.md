# providers

Cloud integrations behind the `Provider` trait in `src/lib.rs`. The
engine talks to the trait and to `ProviderCapabilities`; nothing in
`core/daemon` knows which backend it is driving. Onboarding checklist:
`docs/architecture/provider-onboarding.md`.

## Providers

- `FilesystemProvider` (`src/filesystem/`) is the default. It treats a
  local directory as the cloud side, with a real changes feed, write
  preconditions, op-id tags (xattr, side-file on filesystems without
  them), and atomic downloads. It validated every provider-neutral
  mechanic before an external provider shipped and is the reference
  implementation for the contract suite.
- `GoogleDriveProvider` (`src/gdrive/`) is selected by
  `provider = "gdrive"`. OAuth 2.0 with PKCE (`src/gdrive/oauth.rs`),
  tokens in the platform `SecretStore`, resumable uploads, ranged
  downloads verified by MD5, and the Drive changes feed with cursor
  re-baselining. After a guarded overwrite it reads the versions of
  the file and keeps one that the overwrite replaced unseen; the
  engine stores a record of that check, so a stop of the daemon does
  not lose it (`take_follow_up_changes`, `resume_follow_ups`). A
  daemon that stopped before it stored the outcome of the overwrite
  left no record: at the next start the engine knows that write by the
  op-id on the file and says so (`note_unrecorded_write`), and the
  provider owes the check from there. It
  reports the Drive id of each object, and a file or a folder that got
  another name or place as one move with both paths; the engine stores
  the ids and hands them back at a start (`restore_remote_ids`), so
  the feed names the old path of an object after a restart too.
  Operations: `docs/operations/provider-auth-operations.md`.
- `SuspendedProvider` refuses every call. It is not selectable from
  configuration; the daemon gives it to a profile that cannot run, so
  nothing that profile queued can read as done.
- `FilesystemStubProvider` is inert: every write succeeds as a no-op and
  every read is empty. Tests use it to compose a pipeline without a
  backend (`inert_stub_provider()`); it is never the provider of a
  profile of the daemon.
- `select_provider_for_profile` maps the configured kind to a provider
  and returns an error for unknown kinds, which suspends that profile.

Every provider maps its failures onto the shared taxonomy in
`core/shared` (`Transient`, `RateLimited`, `Authentication`,
`PreconditionFailed`, `NotFound`, `CloudRootUnavailable`, `StorageFull`,
`Permanent`), so retry policy never parses error strings.

## Testing

- Unit tests per module, including error mapping and the OAuth flow.
- `tests/provider_contract.rs` runs one contract suite against three
  fixtures: the filesystem provider, the same provider on a filesystem
  without xattr support (the side-file mode of FAT and network mounts),
  and an in-memory object-store mock (no op-id tags, no changes feed).
  A provider must implement what it advertises and error loudly on what
  it does not.
- Google Drive is tested offline through the injectable `HttpTransport`
  seam with scripted HTTP responses; no test touches the network.

Policy: `docs/architecture/testing-strategy.md`.

## Logging

Provider modules use the structured logger from `vapor-shared`; lines
land in `<vapor_dir>/logs/vapord.log` and never contain tokens or auth
headers.
