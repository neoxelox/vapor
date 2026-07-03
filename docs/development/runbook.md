# Development Runbook

## Repository bootstrap

- Rust workspace: root `Cargo.toml` with crates in `core/daemon`, `core/providers`, `core/shared` (with `core/platform`, `core/lifecycle`, and `core/cli` arriving per `docs/plans/core.md`).
- Daemon binary: `vapord`.
- CLI binary (arriving soon): `vapor` (`core/cli`).
- Swift package: `apps/macos/Package.swift` (`Vapor`, `VaporCore`) — macOS-only.
- Implementation sequencing references:
  - Runtime + platform + CLI: `docs/tasks/core.md`.
  - macOS app: `docs/tasks/macos.md`.
  - `vapor` CLI: `docs/tasks/cli.md`.

## Local script entry points

- Build both stacks (release): `./scripts/build.sh`
- Build + package macOS app bundle: `./scripts/build.sh package`
- Clean build and dist artifacts: `./scripts/clean.sh`
- Lint both stacks: `./scripts/lint.sh`
- Format both stacks: `./scripts/format.sh`
- Format check both stacks: `./scripts/format.sh check`
- Test both stacks: `./scripts/test.sh`
- Sync locale catalogs into every app surface: `./scripts/locales.sh`
- Install git pre-commit hook: `./scripts/hooks.sh` (uninstall: `./scripts/hooks.sh uninstall`)
- Version helper: `./scripts/version.sh`
- Performance smoke thresholds: `./scripts/perf.sh` (`VAPOR_PERF_SMOKE_RUST_MAX_SECONDS`, `VAPOR_PERF_SMOKE_SWIFT_MAX_SECONDS`)

## Stack helpers

- Rust lint: `./scripts/rust/lint.sh`
- Rust format: `./scripts/rust/format.sh check`
- Rust tests: `./scripts/rust/test.sh`
- Rust build: `./scripts/rust/build.sh`
- Swift lint: `./scripts/swift/lint.sh`
- Swift format: `./scripts/swift/format.sh check`
- Swift tests: `./scripts/swift/test.sh`
- Swift build: `./scripts/swift/build.sh`
- Swift locale sync: `./scripts/swift/locales.sh` (mirrors `assets/locales/*.json` into the macOS app's `VaporCore/Resources/locales/`)
- macOS app packaging: `apps/macos/scripts/package.sh`

## Pre-commit hook (optional but recommended for agentic workflows)

`./scripts/hooks.sh` installs a git `pre-commit` hook into the local
clone. The hook runs the incremental validation pipeline before any
commit is allowed to land:

1. `./scripts/lint.sh` — Rust + Swift lint plus `format.sh check`.
2. `./scripts/test.sh` — Tier 1 test suite for both stacks.

Any failure aborts the commit. This is the same gate the agent's
feedback loop uses, which is what makes it suitable for autonomous
contributors: when a commit lands, `lint → test` is known-green on the
working tree. The hook is deliberately incremental — a cold
`clean → build` per commit costs minutes without adding signal (CI runs
the full matrix, and `./scripts/build.sh` remains available for manual
release-mode verification).

Operational notes:

- The hook lives at `.git/hooks/pre-commit` and is per-clone (git does
  not track hooks). Re-run `./scripts/hooks.sh` after a fresh clone or
  after switching machines.
- If a non-vapor `pre-commit` hook already exists, the installer
  preserves it as `.git/hooks/pre-commit.bak` and refuses to overwrite
  if a backup is already there — resolve the conflict manually.
- Remove the hook with `./scripts/hooks.sh uninstall`; it only deletes
  hooks that carry the `vapor-managed-hook` marker, so unrelated hooks
  are left alone.
- Because the hook runs `clean.sh`, every commit re-builds the
  workspace from scratch. That trade-off is intentional: it guarantees
  no stale artifacts mask a regression. If you need a faster local
  iteration loop, run `./scripts/lint.sh` and `./scripts/test.sh`
  manually and skip the hook by passing `--no-verify` only with the
  project owner's explicit approval (see `AGENTS.md` Bash safety).

## Notes

- Scripts intentionally skip missing stack artifacts during early bootstrap (for example, no `Cargo.toml` yet or no `apps/macos` project yet).
- Scripts default `VAPOR_DIR` to repo-local `./.vapor` for local dev and test ergonomics.
- Scripts default `VAPOR_ENV` to `dev` (and `prod` for `./scripts/build.sh package`).
- `VERSION` is the release version source-of-truth; wrapper scripts fail fast when `Cargo.toml` is out of sync with it.
- `./scripts/version.sh` is the release-prep entrypoint: it requires a clean `main` branch except for `CHANGELOG.md`, then writes `VERSION`, syncs Cargo, creates `release: v...` commit, and creates the matching tag.
- Override runtime root with `VAPOR_DIR=/path/to/vapor ./scripts/test.sh` (same for build, lint, and format).
- `Vapor.app` is a single package that ships both binaries: `Contents/MacOS/Vapor` and `Contents/MacOS/vapord`.
- Runtime daemon launch path is always the bundled sibling binary (`vapord`) next to the app executable.
- Swift lint and format scripts intentionally use `swift format` only.
- If an Xcode project exists, set `VAPOR_XCODE_SCHEME` to enable `xcodebuild build` in `./scripts/swift/build.sh`.

## Testing discipline

Vapor is coded autonomously. The suite is the feedback loop — it has to
be fast, deterministic, and honest. Full policy in `AGENTS.md §9`; full
taxonomy in `docs/architecture/testing-strategy.md`.

Fast facts for local dev:

- **One command:** `./scripts/test.sh` runs the full Tier 1 suite
  (Rust + Swift unit/integration/property/snapshot tests plus version
  consistency checks). Target wall time: under 2 minutes on a
  contemporary M-series dev machine; under 5 minutes on CI.
- **Stack-specific:** `./scripts/rust/test.sh` or
  `./scripts/swift/test.sh` if you want to run just one side.
- **No network, no `~/.vapor`.** Tests use `tempfile::TempDir` for
  filesystem work and never contact the real Internet.
- **No sleeps for timing.** Use test-injectable clock abstractions;
  retry decorators are banned.
- **Snapshot updates:** when an intentional `--json` schema change
  lands on the CLI (once it ships), run `cargo insta review` to
  approve the new snapshot. CI fails the PR if there is an
  unapproved drift.
- **Flaky test?** Fix it or remove it in the same PR. We do not carry
  flaky tests forward — the agent's feedback loop depends on
  determinism.
- **What not to test:** trivial getters, `Default` impls mirroring
  constants, UI rendering (SwiftUI, menubar, Dock, future GUI
  surfaces), interactive TTY behavior on the `vapor` CLI. See
  `AGENTS.md §9.3` and `docs/architecture/testing-strategy.md`.
- **Performance SLO tests** run via `./scripts/perf.sh` (Tier 2;
  release gate only, not a PR gate).

## Release build policy

- A single release mode is used and tuned for performance with safe optimizations.
- The GitHub release pipeline validates the tag ref first, then runs `lint.yml`, `test.yml`, and `perf.yml` in parallel; the `release` job proceeds only with `needs: [preflight, lint, test, perf]`.
- Stable tag releases provision signing and notarization material on the runner before packaging.
- Rust release profile uses `opt-level=3`, `lto=fat`, `codegen-units=1`, `panic=abort`, and `strip=symbols`.
- Swift release build uses whole-module and cross-module optimization flags.
