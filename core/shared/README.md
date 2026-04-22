# shared

Shared contracts and reusable Rust utilities consumed across the workspace
(`core/daemon`, `core/providers`, future `core/platform`, future
`core/lifecycle`, future `core/cli`).

Scope:

- IPC schemas and versioning types (transport-agnostic; see
  `docs/architecture/ipc-contracts.md`).
- Shared error taxonomy.
- Settings and policy models.
- Compatibility metadata.
- Shared Rust logging primitives used by the daemon, providers, and the
  CLI — with sensitive-value redaction.
- Runtime path resolution (`vapor_dir` + subdirectories for logs/state).
- Source-of-truth constants module (`core/shared/src/constants.rs`).
  Swift mirrors this in `apps/macos/Sources/VaporCore/VaporConstants.swift`
  and must stay in sync.

Contract changes must preserve backward compatibility or ship with a
migration plan (pre-GA, the project owner may fast-track breaking changes
per `AGENTS.md §1.1`).

Testing: logging redaction, runtime-path resolution, and log-level
parsing are unit-tested. Constants are not tested individually — they
are data. Policy: `docs/architecture/testing-strategy.md`.
