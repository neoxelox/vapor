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
| `enumerate()` / `stat()` / `content_hash()` | Read-side used by reconcile walks; `enumerate` is non-recursive so walks stay slice-interruptible. A walk asks for the listings of several folders at one time, each on a thread of its own, so a listing must be safe next to other listings and next to transfers. An entry carries the id that the backend has for the object (`RemoteEntry::id`), when it has ids: see "Object ids" below. |
| `enumerate_with_unsyncable()` | The listing the walk asks for: the entries of `enumerate`, and the names in the directory that cannot be a sync path (a `/` in a Drive name, a backslash). Such a name is no entry and never syncs; the engine asks the user about it for as long as a listing reports it. A backend that leaves a name out of `enumerate` reports it here at every listing. The default reports none. |
| `begin_upload()` / `begin_download()` | Return a `TransferSession` that moves a bounded byte budget per `step()`. The session names its own step size (`step_bytes`, the chunk the backend wants) and, when it knows, what is left to move (`remaining_bytes`); the engine grants each step from the bandwidth bucket and never asks for more than is left. Uploads take a `RemotePrecondition` guard (keep-both safety), and every backend must honour it: `HashEquals` and `Absent` are part of the contract, not a capability. A backend that cannot make the check of `HashEquals` and the write one step must find a version that the write replaced without having compared it, and keep it next to the file as a copy named with `vapor_shared::conflict::copy_name` and `constants::conflict::REPLACED_VERSION` (`data-flow.md` §Conflict handling): the filesystem provider exchanges the two files, Google Drive reads the stored versions after the upload. A check that runs after the upload is a follow-up (the row below), so a stop of the daemon does not lose it. The completed `TransferOutcome` carries the content hash and the remote object's mtime as the backend reports it after the transfer (`remote_modified_at`); the sync index records that mtime so the reconcile walk can tell a same-size remote edit from an untouched object, so report it whenever the backend has one. |
| `delete()` | Prefer recoverable semantics (Drive moves to trash; filesystem removes). |
| `move_object()` | One-call move of a file to another path, keeping its content and tagging it with the op-id, when `supports_server_side_move`. A missing source is `NotFound`, an occupied destination `PreconditionFailed`; the engine falls back to a plain upload on either. The filesystem provider renames; Drive patches name and parents. The engine also calls it for a folder, to carry a name of this device to the cloud as one move (`data-flow.md` §A name or a place that the cloud changed), so a backend that can move a folder moves it with everything in it. |
| `take_follow_up_changes()` / `resume_follow_ups()` | For a backend that owes work after a write, and must not lose it when the daemon stops (`data-flow.md` §Conflict handling). The provider holds the work in memory and gives the engine one record of text for each piece: `Started` when the work begins, `Finished` when it is done or can never be done. The engine never reads a record. It stores each one in the state database of the profile, hands the stored ones back when the profile starts, and removes one when the provider reports its end. Both calls run on the tick thread, so they read memory only: no request, no file, no lock that a request is made under. The record of a transfer is in the list before the step that ends the transfer returns, on success and on failure; the engine then saves it in the write that saves the outcome. `resume_follow_ups` answers with the records the provider does not take (text it cannot read, work that can no longer be done), and the engine forgets those. Work that is handed back can have run in part, so it must be safe to do twice. A record holds no token, no auth header, and no address of an upload session. It is at most 4 096 bytes, and a profile stores at most 10 000; a record over either limit is not stored, and its work still runs in that daemon. The defaults report nothing and take nothing, which is right for a backend whose guarded write is one step. |
| `note_unrecorded_write()` | The engine found a cloud object that carries an op-id of this device and this profile, while the index row of the path records another op-id and another hash: an upload of this device landed in a daemon that stopped before it stored the outcome, and the follow-up of that write was never stored. The engine tells the provider the path, the hash that the index held (the base), the hash that the object holds, and the op-id. A backend that owes work after a write takes the call as that write: an overwrite of the base with the content, with an outcome that nobody recorded. The call comes whatever the local file holds, and it can come again until the index row records the object, so a second call for one write starts no second piece of work. The engine makes no call for content that another device or another program put there, and none for a path whose index row is missing or holds no hash: with no base, nothing can have been replaced. The call runs on the tick thread, so it reads and writes memory only; what needs a request waits for a worker. A follow-up that the call starts is in the answer of the next `take_follow_up_changes`, so the engine stores it in that tick. The default does nothing, which is right for a backend whose guarded write is one step. |
| `poll_changes()` | Incremental changes feed with an opaque cursor. Return `CursorExpired` when the cursor lapses: the engine reconciles and re-baselines. A change is `CreatedOrModified` or `Removed` for a file. A backend that knows where a changed object was sends `Moved { from, entry_kind }` for a file or a folder that got another name or another place: the path of the change is where the object is now, `from` is where it was, and for a file the change carries what a `CreatedOrModified` carries. The engine then renames the same entry on the device. Send it only when both places are known and each path names that object; for anything less, send `Removed` and `CreatedOrModified` for a file. A feed that has one entry for a folder which got another name or another place, and none for the files in it, sends `FolderMoved` with a path of the folder (the one it had, the one it has, or one change for each) when it cannot send `Moved`; the engine walks the folder that holds that path. With the root as its path it says that the place of a change could not be read, and the engine walks everything. |
| `restore_remote_ids()` | The engine hands back the ids that it stored for the synced files and folders, one time, before the first poll and the first transfer, on the tick thread: the call reads memory only. The ids are those of one cloud root: the engine hands them over when the root is ready and is the folder it adopted, and forgets them when another folder becomes the root. A backend that finds its root to be another folder drops what it knows of the folder before. A backend whose feed names a changed object by its id needs it to say which path an entry is about after a stop of the daemon. A path that comes back is what the engine believed at its last write, so the backend confirms it with a request before it writes through it. The default keeps nothing, which is right for a backend without ids. |

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

## Object ids

A backend that has an id for each object reports it: on a listed entry
(`RemoteEntry::id`), on a change (`RemoteChange::id`), and on the
outcome of a transfer (`TransferOutcome::remote_id`). The id is an
opaque text. It stays with the object when the object gets another
name or another place, and no other object under the root has it. The
engine stores it in the sync index and compares two ids for equality;
it never reads one.

- An id for every object, or for none. The engine pairs a renamed
  entry by its id, so ids on some entries only would pair some renames
  and not others. The contract suite checks it.
- There is no capability flag: an entry with an id is the signal.
- A backend without ids (`filesystem`) reports none. A folder that is
  renamed in such a cloud is one folder that is gone and one that is
  new, and the device works file by file.

## Capability flags

Declare only what the backend truly does: the engine plans around
these flags (`ProviderCapabilities`):

- `supports_remote_changes_feed`: incremental polling; without it the
  engine falls back to periodic reconcile enumeration.
- `supports_server_side_move`: `move_object` moves a file with no
  re-upload; without it a rename is a delete and an upload.
- `changes_feed_is_watched`: the feed is fed by a watcher on this
  device and a poll costs no request, so the engine reads it each
  second.
- `small_transfers_share_a_commit`: the commit of a transfer is one
  flush of a drive that the transfers of the same moment share. The
  engine then runs small transfers (256 KiB at most, and deletions) in
  a lane of their own: five for each transfer slot of the throttle
  state, in each direction. A backend that pays for each transfer on
  its own, with a request that counts against a rate limit, leaves it
  off; its small transfers take the slots like any other.

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
2. **Networking.** The production transport trusts the certificate
   authorities that ship with Vapor and the ones the system trusts, so
   a network that inspects TLS with an authority of its own works. It
   sends a request through the proxy that `HTTPS_PROXY` or `ALL_PROXY`
   names (with `NO_PROXY` for the hosts that go around it), and with
   neither set through the proxy of the system's network settings
   (`ProxySettings` in `platform-abstractions.md`). A provider gets
   both with no code of its own.
   All HTTP goes through the `HttpTransport` seam
   (`core/providers/src/http.rs`). Production uses
   `NativeHttpTransport`; tests script `ScriptedHttpTransport`. Never
   contact the network in Tier 1 tests (AGENTS.md §9.1).
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
   classification, auth-failure classification. A backend with
   follow-ups also tests a stop of the daemon: a new provider that
   takes the records back ends the work, with the result it has with
   no stop and no step done twice. It tests a stop that left no
   record too: a new provider that is told of a write with no
   recorded outcome does the work of that write one time. A backend
   with ids tests a start:
   a new provider that got its ids back reports a renamed folder and a
   renamed file with both paths, and a removal by its path.
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
  that may embed them. The same holds for a follow-up record, which
  the engine writes to its state database.

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
