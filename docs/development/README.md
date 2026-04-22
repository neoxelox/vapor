# Development

Everything a contributor (human or AI) needs to bootstrap, build, test,
and validate Vapor locally. Use this directory for local script entry
points, toolchain expectations, and conventions that apply across the
workspace.

## How to use this group

- **New contributor?** Start with `runbook.md`; it lists every
  `./scripts/*` entry point and when to use it. Cross-reference
  `toolchain-baseline.md` to confirm your local toolchain matches the
  known-good set.
- **Upgrading a toolchain?** Policy is "latest stable by default" per
  `AGENTS.md §8.2`; `toolchain-baseline.md` is informational and should
  be updated when the contributor baseline shifts materially.
- **Running the same checks as CI?** `docs/ci/README.md` maps every
  workflow to the same `./scripts/*` entry points listed in the
  runbook, so local and CI validation stay identical.

## Documents

- `runbook.md` — repository bootstrap (workspace layout, binary names,
  planned crates), local script entry points for build / test / lint /
  format / perf, stack-specific helpers (Rust and Swift), runtime-path
  defaults (`VAPOR_DIR=./.vapor` for dev/test), and release-build
  policy knobs (Rust release profile, Swift optimization flags).
- `toolchain-baseline.md` — known-good local reference toolchain (macOS
  version, Rust/Cargo, Swift driver, Swift compiler). Pre-GA this is a
  reference, not a hard pin; the project targets latest stable by
  default.

## Related references

- `docs/ci/README.md` — how those same scripts run in CI.
- `docs/plans/core.md` — upcoming crates and structure changes that will
  extend what the runbook covers (`core/platform`, `core/lifecycle`,
  `core/cli`).
