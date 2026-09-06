# shared

Shared contracts and reusable Rust utilities consumed across the workspace
(`core/daemon`, `core/providers`, `core/platform`, `core/lifecycle`,
`core/ipc`, `core/cli`).

Scope:

- Throttle inputs, sync modes, and the error taxonomy the engine and
  providers share (the IPC wire types live in `core/ipc`).
- Shared error taxonomy.
- Settings and policy models.
- Compatibility metadata.
- Shared Rust logging primitives used by the daemon, providers, and the
  CLI — with sensitive-value redaction.
- Runtime path resolution (`vapor_dir` + subdirectories for logs/state),
  the cross-process config lock, and the one atomic private writer every
  `vapor.json` writer uses.
- The `vapor.json` model with lenient per-key loading, and the device
  id persisted on first run.
- Source-of-truth constants module (`core/shared/src/constants.rs`).
  Swift mirrors this in `apps/macos/Sources/VaporCore/VaporConstants.swift`
  and must stay in sync.

Pre-GA, contracts change freely when the change is documented in the
same change set (`AGENTS.md §1.1`).

Testing: logging redaction, runtime-path resolution, and log-level
parsing are unit-tested. Constants are not tested individually — they
are data. Policy: `docs/architecture/testing-strategy.md`.
