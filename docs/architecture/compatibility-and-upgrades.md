# Compatibility and Upgrade Policy

## Scope

Compatibility rules for app, daemon, shared contracts, and persisted schema across releases.

## Version surfaces

- App version (`apps/macos`)
- Daemon version (`core/daemon`)
- Shared contract version (`core/shared`)
- State schema version (durable DB)

## Baseline policy

- Prefer backward-compatible additive contract evolution.
- Reject unsupported version pairs with explicit diagnostics.
- Avoid silent behavior divergence between app and daemon.

## Compatibility matrix (initial)

| App | Daemon | Shared contracts | Schema |
| --- | --- | --- | --- |
| N | N | N | N |
| N | N-1 | N-1 compatible | migrated or compatible |
| N-1 | N | N-1 compatible | compatible |

`N` denotes latest stable release line.

## Upgrade flow expectations

1. Install/update app and daemon artifacts.
2. Validate launch configuration and compatibility handshake.
3. Perform schema migration if required.
4. Resume queues with recovery checks.

## Rollback expectations

- Keep rollback-safe artifacts for app and daemon.
- Define whether schema rollback is supported per migration.
- If rollback cannot be automatic, document operator mitigation.
