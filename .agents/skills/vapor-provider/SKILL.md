---
name: vapor-provider
description: Guides adding or changing a cloud provider behind Vapor's Provider trait, covering capability honesty, RemotePath scope safety, op-id tags, the changes feed and cursor contract, retry classification, the contract-suite fixtures, offline HTTP tests, and the docs and README rows a provider must update. Use when touching core/providers or adding a provider kind.
license: GPL-3.0-only
---

# Add or change a provider

Full checklist with acceptance criteria:
`docs/architecture/provider-onboarding.md`. Auth and token policy:
`docs/operations/provider-auth-operations.md`. The engine only ever sees
`Provider` and `ProviderCapabilities` (`core/providers/src/lib.rs`).

## Contract

- **Capability honesty.** Advertise only what the backend implements;
  an unadvertised operation errors loudly, never no-ops. The contract
  suite checks this against the filesystem provider, the same provider
  without xattr support, and an object-store mock.
- **Scope safety.** Every remote path is a `RemotePath` relative to the
  cloud root; resolve it inside the root and refuse anything that
  escapes. Names that cannot be represented (a `/` inside a Drive
  name) are no entries: the listing reports them
  (`enumerate_with_unsyncable`), and the engine asks the user about
  each one.
- **Op-id tags.** Writes carry the engine's op id; enumeration and
  changes echo it back so loop prevention can drop the daemon's own
  writes. Without native metadata, use the side-file fallback.
- **Changes feed.** `poll_changes(cursor)` returns a page and the next
  cursor, or `CursorExpired`; the engine persists the cursor only after
  the page's intents are durable and re-baselines on expiry. Never
  return an empty cursor as "no change". A backend that knows where a
  changed file or folder was sends `Moved { from, entry_kind }`: the
  engine renames the same entry on the device, with everything in a
  folder. Send it only when both places are known and each path names
  that object. A feed that reports a folder which got another name or
  place with one entry, and nothing for the files in it, and cannot
  send `Moved`, sends `FolderMoved` for the folder's paths; the engine
  walks the folder that holds them. `FolderMoved` with the root as its
  path means "a change whose place could not be read": the engine walks
  everything. Never drop a change because a lookup failed.
- **Object ids.** A backend with ids reports the id of an object on a
  listed entry, on a change, and on the outcome of a transfer: an
  opaque text that stays with the object under a new name. An id for
  every object or for none; a backend without ids reports none. The
  engine stores the ids and hands them back through
  `restore_remote_ids` at a start (tick thread, memory only), so the
  feed can name the old path of an object after a stop of the daemon.
  A path that comes back is a hint: confirm it with a request before a
  write goes through it.
- **Capabilities.** Four flags, all honest: `supports_remote_changes_feed`,
  `supports_server_side_move`, `changes_feed_is_watched` (a poll costs
  no request), and `small_transfers_share_a_commit` (one flush of a
  drive covers the transfers of a moment, so the engine runs small
  transfers in a wider lane). Write preconditions are not a flag:
  every backend honours them.
- **Transfers.** `begin_upload` / `begin_download` return a
  `TransferSession`; each `step(max_bytes)` moves a bounded budget and
  reports `Progressed` or `Completed`. The session names its own step
  size (`step_bytes`) and what is left when it knows (`remaining_bytes`).
  A step that moves zero bytes on a non-empty range must fail as
  transient, not report progress. Uploads honour `RemotePrecondition`
  (keep-both safety). A backend whose `HashEquals` is a check followed
  by a write keeps any version the write replaced unseen as a
  `~conflict-replaced-<ms>` copy (`provider-onboarding.md`). A completed
  upload reports the hash of the bytes the backend stored, checked
  against the hash of what was sent.
- **Follow-ups.** Work that a backend still owes after a write (the
  check for a replaced version on Google Drive) must outlive the
  daemon. `take_follow_up_changes` gives the engine one record of text
  for each piece (`Started`, then `Finished`); the engine stores it
  with the outcome of the transfer and hands it back through
  `resume_follow_ups` at the next start. Both calls run on the tick
  thread and read memory only. The record of a transfer is in the list
  before the step that ends it returns. Work that comes back must be
  safe to do twice, and must end one day: it reports `Finished` when
  it is done, and also when it can never be done. A record holds no
  token. The defaults owe nothing and take no record.
- **Writes with no recorded outcome.** A daemon can stop after a write
  landed and before it stored the outcome, so no follow-up record is
  left. At the next start the engine finds a cloud object with an
  op-id of this device and this profile that its index row does not
  hold, and calls
  `note_unrecorded_write(path, base_hash, content_hash, op_id)`: the
  base is what the index row held, the content is what the object
  holds. A backend with follow-ups starts the work of that write
  there, and reports it as `Started`. The call runs on the tick
  thread and uses memory only; an id that the backend does not know
  is read later, on a worker. The call can come more than once for
  one write, so one write must never get its work twice. No call
  comes for bytes that another device or another program put there.
  The default does nothing.
- **Root identity and moves.** `root_identity` reports a stable
  identity for the cloud root (a marker file the provider writes at
  `adopt_root`, or the backend's folder id) and never creates the root;
  `move_object` moves a file in one call when
  `supports_server_side_move`, refusing an occupied destination with
  `PreconditionFailed`. Both feed safety paths (`data-flow.md` §Root
  identity, item 14 of §Local to remote), so their answers must be
  exact, never a best guess. Every transfer outcome carries the remote
  mtime when the backend has one.
- **Errors.** Map every failure to `ProviderErrorKind` (`Transient`,
  `RateLimited { retry_after }`, `Authentication`, `PreconditionFailed`,
  `NotFound`, `CloudRootUnavailable`, `StorageFull`, `Permanent`). The
  retry policy never parses messages. Authentication errors name the
  command the user must run. `StorageFull` is "the cloud has no room":
  uploads wait and downloads go on.
- **Secrets.** Tokens only through `vapor_platform::SecretStore`, opened
  with `open_store` so the runtime directory's scope applies, keyed
  `auth.<profile>.<provider>.token`; never in `vapor.json`, the state DB,
  logs, support reports, or a command line.
- **Threads.** Provider calls run on worker threads; a provider must be
  `Send + Sync` and must not hold locks across network calls.

## Tests

- Unit tests for error mapping and path resolution.
- Offline HTTP tests through the injectable `HttpTransport` with
  scripted responses (rate limits, 401 and refresh, resumable upload
  resumption, ranged downloads, cursor expiry). No test touches the
  network. A backend with follow-ups tests a stop of the daemon: a new
  provider that takes the records back ends the work with the result
  it has with no stop, and a new provider that is told of a write
  with no recorded outcome does the work of that write one time. A
  backend with ids tests a start: a new provider
  that got its ids back reports a renamed folder and a renamed file
  with both paths, and a removal by its path.
- Add the provider to `core/providers/tests/provider_contract.rs` when
  it can run against an in-memory backend.
- e2e stays on the filesystem provider by default. `./scripts/e2e.sh
  --provider gdrive` is a leg of the release gate, with a dedicated
  test account.
- A change to the Google Drive provider is checked with that account
  before it is reported as done, in a Tier VM guest with the Internet
  (`vapor-vm`). Run the check without asking the owner: the account
  is there for it (`AGENTS.md` §9.8, §10.1). Never use the owner's own
  account.

## Docs and surfaces

- `provider::ALL` in `core/shared/src/constants.rs` and the Swift
  `Provider` mirror, so `provider = "<kind>"` validates and displays.
- `select_provider_for_profile` in `core/providers/src/lib.rs`.
- Root `README.md` **Providers** row; `core/providers/README.md`;
  the onboarding and auth-operations docs; `CHANGELOG.md`.
