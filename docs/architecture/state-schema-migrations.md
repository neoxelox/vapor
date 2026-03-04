# State Schema and Migration Policy

## Scope

Durable queue/state schema rules for daemon persistence.

## Invariants

- At-least-once intent durability is non-negotiable.
- Schema version is explicit and persisted.
- Migrations are tested before release.

## Migration strategy

- Use monotonic integer schema versions.
- Support forward migration on startup.
- Define rollback behavior for each schema transition.

## Compatibility and safety

- New code must handle prior supported schema versions or block with clear error.
- Partial migration failures must not corrupt existing persisted data.
- Backup or snapshot strategy required for high-risk migrations.

## Corruption recovery

- Detect corruption early with integrity checks.
- Enter recoverable degraded mode with user-visible diagnostics.
- Provide documented operator recovery steps and verification.

## Validation requirements

- Unit tests for migration transforms.
- Integration tests for restart with mixed-version states.
- Negative tests for partial/corrupt migration inputs.
