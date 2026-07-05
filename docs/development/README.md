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
- **Shipping a feature or fix that changes runtime behavior?** After
  Tier 1 passes, run `./scripts/e2e.sh` and read
  `e2e-verification.md` for when the tier is required and how to
  extend it.

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
- `e2e-verification.md` — the Tier E2E process: black-box verification
  of the real `vapor`/`vapord` binaries inside a disposable
  `.vapor/e2e/` sandbox (`./scripts/e2e.sh`). Covers the safety
  contract, when a change requires an E2E run, the scenario catalog,
  and the discipline rules for adding scenarios.

## Related references

- `docs/ci/README.md` — how those same scripts run in CI.
- `docs/architecture/testing-strategy.md` — the authoritative reference
  for the test taxonomy, discipline rules, per-surface scope, and the
  explicit "do not test" list. Read before writing or reviewing a test.
- `docs/plans/core.md` — the runtime plan behind the workspace structure
  the runbook covers (`core/platform`, `core/lifecycle`, `core/cli`).
