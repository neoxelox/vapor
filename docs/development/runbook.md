# Development Runbook

## Repository bootstrap

- Rust workspace: root `Cargo.toml` with crates in `core/daemon`, `core/providers`, `core/shared`, `core/ipc`, `core/platform`, `core/lifecycle`, and `core/cli`, plus the development tools under `tools/` (`tools/e2e`, never shipped; see `tools/README.md`).
- Daemon binary: `vapord`.
- CLI binary: `vapor` (`core/cli`).
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
- Release gate (format, lint, Tier 1, and the e2e suite once per provider; the Google Drive leg needs the test account signed in): `./scripts/release-gate.sh`
- Performance gate (one release-profile soak cell with SLO assertions): `./scripts/perf.sh` (`VAPOR_PERF_SOAK_DURATION`, `VAPOR_PERF_SOAK_SEED`)
- Soak verification (Tier S): `./scripts/soak.sh` (`--duration`, `--seed`, `--mode`, `--load`, `--faults`, `--throttle`, `--release`, `--status`, `--verify`; see `docs/development/soak-testing.md`)
- End-to-end verification of the real binaries in disposable sandboxes: `./scripts/e2e.sh` (`--only Sxx`, `--keep`, `--skip-build`, `--json`, `--list`, `--sandbox`, `--sandbox-stop`; see `docs/development/e2e-verification.md`)

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
clone (worktrees included — the hooks directory is resolved through
git, so `.git`-as-a-file layouts work). The hook runs the full local
validation pipeline before any commit is allowed to land:

1. `./scripts/lint.sh` — Rust + Swift lint plus `format.sh check`.
2. `./scripts/test.sh` — Tier 1 test suite for both stacks.
3. `./scripts/build.sh` — build of every shipping binary.

Any failure aborts the commit. This is the same gate the agent's
feedback loop uses, which is what makes it suitable for autonomous
contributors: when a commit lands, `lint → test → build` is
known-green on the working tree.

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
- The hook builds **incrementally** (it does not run `clean.sh`), so a
  commit is fast and — importantly — it never wipes `.vapor`/`dist`,
  which would destroy a preserved or running e2e sandbox
  (`e2e.sh --keep`/`--sandbox`). The from-scratch, empty-cache guarantee
  lives in CI, which checks out fresh and builds cold on every PR; that
  is the authoritative gate against stale artifacts masking a
  regression. To force a clean local build, run `./scripts/clean.sh`
  yourself before committing. Skip the hook with `--no-verify` only with
  the project owner's explicit approval (see the commit policy in `AGENTS.md` §10).

## Notes

- Scripts default `VAPOR_DIR` to repo-local `./.vapor` for local dev and test ergonomics.
- Scripts default `VAPOR_ENV` to `dev` (and `prod` for `./scripts/build.sh package`).
- `VERSION` is the release version source-of-truth; wrapper scripts fail fast when `Cargo.toml` is out of sync with it.
- `./scripts/version.sh` is the release-prep entrypoint: it requires a clean `main` branch except for `CHANGELOG.md`, then writes `VERSION`, syncs Cargo, creates `release: v...` commit, and creates the matching tag.
- Override runtime root with `VAPOR_DIR=/path/to/vapor ./scripts/test.sh` (same for build, lint, and format).
- `Vapor.app` is a single package that ships three executables: `Contents/MacOS/Vapor` (app), `Contents/MacOS/vapord` (daemon), and `Contents/Helpers/vapor` (CLI — it cannot live in `Contents/MacOS/` because the default macOS filesystem is case-insensitive and `vapor` would collide with `Vapor`).
- Runtime daemon launch path is always the bundled `vapord`: a sibling of the launching binary, or `../MacOS/vapord` when resolved from the bundled CLI in `Contents/Helpers/`.
- What the daemon parks for a person to answer lives in the profile state DB and is read and answered with `vapor decisions list|show|resolve <id> --choose <key>`, with or without a daemon; what the daemon removed on this device is listed and restored with `vapor trash list|restore|empty`. Both are the CLI surfaces the app shells drive with `--json`.
- Daemon lifecycle (install/start/stop/supervision, crash-loop state) is driven through `vapor service` on every surface; `vapor service check` is one supervision tick (`--loop` keeps ticking, which is what `vapor service install --supervise` registers as the kept-alive `sh.arn.vapor.supervisor` job for installs without the app), `vapor service acknowledge` clears a crash-loop pause, and durable crash-loop state lives at `<vapor_dir>/state/lifecycle.json`.
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
- **`--json` shape locks:** every `--json` command has an explicit
  assertion test on its serialized shape (`insta` snapshots are still
  an open task). An intentional schema change updates that test in the
  same change set; CI fails the PR otherwise.
- **Flaky test?** Fix it or remove it in the same PR. We do not carry
  flaky tests forward — the agent's feedback loop depends on
  determinism.
- **What not to test:** trivial getters, `Default` impls mirroring
  constants, UI rendering (SwiftUI, menubar, Dock, future GUI
  surfaces), interactive TTY behavior on the `vapor` CLI. See
  `AGENTS.md §9.3` and `docs/architecture/testing-strategy.md`.
- **Performance SLO checks** run via `./scripts/perf.sh` (Tier 2;
  release gate only, not a PR gate): one soak cell against the release
  profile, its report asserted against the budgets.
- **Soak verification** runs via `./scripts/soak.sh` (Tier S): hours
  of seeded churn with faults and a model-checked oracle; nightly in
  `soak.yml` and on demand; see `docs/development/soak-testing.md`.
- **End-to-end verification** runs via `./scripts/e2e.sh` (Tier E2E,
  the `tools/e2e` harness) after Tier 1 passes, whenever a change
  alters runtime behavior a user would observe through the daemon or
  CLI. One sandbox per scenario under `.vapor/e2e/`; `--only Sxx`
  runs one scenario; see `docs/development/e2e-verification.md`.
- **Linux from a macOS checkout.** The `core/platform` Linux
  implementations and the daemon on inotify are verified in a
  container over the same working tree. The target directory is a
  named volume mounted outside the checkout, so the two toolchains
  never overwrite each other's binaries and no mount point appears in
  the repo, and a tmpfs covers `.vapor/`, because a Docker Desktop
  bind mount refuses to bind the daemon's Unix socket. The image's
  `rustup` follows `rust-toolchain.toml`, so the container lints with
  the latest stable clippy, which is what CI runs too:

  ```sh
  docker run --rm -v "$PWD":/work -w /work \
    -v vapor-linux-target:/target \
    -v vapor-linux-cargo:/usr/local/cargo/registry \
    --tmpfs /work/.vapor:rw,size=2g \
    -e CARGO_TARGET_DIR=/target -e VAPOR_ENV=dev \
    rust:slim sh -c 'apt-get update -qq && apt-get install -y -qq \
      pkg-config libsqlite3-dev build-essential attr procps >/dev/null && \
      cargo clippy --workspace --all-targets -- -D warnings && \
      cargo test --workspace && ./scripts/e2e.sh'
  ```

## Release build policy

- A single release mode is used and tuned for performance with safe optimizations.
- The GitHub release pipeline validates the tag ref first, then runs `lint.yml`, `test.yml`, and `perf.yml` in parallel; the `release` job proceeds only with `needs: [preflight, lint, test, perf]`.
- Stable tag releases provision signing and notarization material on the runner before packaging.
- Rust release profile uses `opt-level=3`, `lto=fat`, `codegen-units=1`, `panic=abort`, and `strip=symbols`.
- Swift release build uses whole-module and cross-module optimization flags.
