# Compatibility and Upgrade Policy

## Scope

Compatibility rules for app, daemon, shared contracts, and persisted schema across releases.

## Version surfaces

- App version (`apps/macos`)
- Daemon version (`core/daemon`)
- IPC schema version (`core/shared` constants, `core/ipc` types)
- State schema version (durable DB)

## Baseline policy

- Prefer backward-compatible additive contract evolution.
- Reject unsupported version pairs with explicit diagnostics.
- Avoid silent behavior divergence between app and daemon.

## Compatibility matrix (initial)

| App | Daemon | IPC schema | State schema |
| --- | --- | --- | --- |
| N | N | N | N |
| N | N-1 | N-1 compatible | set aside and rebuilt when it differs |
| N-1 | N | N-1 compatible | set aside and rebuilt when it differs |

`N` denotes latest stable release line. The `|N - M| <= 1` rule for the app-vs-daemon skew is enforced at the IPC handshake layer; concrete handshake mechanics (fail-closed `IncompatibleVersion`, default-on-unknown-field, payload bounds) live in `docs/architecture/ipc-contracts.md`. Before GA the state database has no migrations: a database of another schema version is set aside and a whole-scope reconcile rebuilds it (`docs/architecture/state-schema-migrations.md`).

## Upgrade flow expectations

1. Install/update app and daemon artifacts.
2. Validate launch configuration and compatibility handshake.
3. Open the state database. Before GA, set a database of another
   schema version aside and rebuild it with a whole-scope reconcile;
   from GA on, migrate it.
4. Resume queues with recovery checks.

## Rollback expectations

- Keep rollback-safe artifacts for app and daemon.
- From GA on, define whether schema rollback is supported per migration.
  Before GA a rollback sets the newer database aside like any other
  version.
- If rollback cannot be automatic, document operator mitigation.
