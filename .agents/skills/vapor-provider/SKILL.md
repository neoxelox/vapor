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
  name) are skipped with a WARNING that names the entry.
- **Op-id tags.** Writes carry the engine's op id; enumeration and
  changes echo it back so loop prevention can drop the daemon's own
  writes. Without native metadata, use the side-file fallback.
- **Changes feed.** `poll_changes(cursor)` returns a page and the next
  cursor, or `CursorExpired`; the engine persists the cursor only after
  the page's intents are durable and re-baselines on expiry. Never
  return an empty cursor as "no change". A feed that reports a folder
  which got another name or place with one entry, and nothing for the
  files in it, sends `FolderMoved` for the folder's paths; the engine
  walks the folder that holds them.
- **Capabilities.** Two flags, both honest: `supports_remote_changes_feed`
  and `supports_server_side_move`. Write preconditions are not a flag:
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
  network.
- Add the provider to `core/providers/tests/provider_contract.rs` when
  it can run against an in-memory backend.
- e2e stays on the filesystem provider by default. `./scripts/e2e.sh
  --provider gdrive` is a leg of the release gate, run by hand with a
  dedicated test account, never by an agent on its own
  (`AGENTS.md` §9.8).

## Docs and surfaces

- `provider::ALL` in `core/shared/src/constants.rs` and the Swift
  `Provider` mirror, so `provider = "<kind>"` validates and displays.
- `select_provider_for_profile` in `core/providers/src/lib.rs`.
- Root `README.md` **Providers** row; `core/providers/README.md`;
  the onboarding and auth-operations docs; `CHANGELOG.md`.
