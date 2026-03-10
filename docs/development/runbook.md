# Development Runbook

## Repository bootstrap

- Rust workspace: root `Cargo.toml` with crates in `core/daemon`, `core/providers`, `core/shared`.
- Daemon binary: `vapord`.
- Swift package: `apps/macos/Package.swift` (`Vapor`, `VaporCore`).
- Implementation sequencing reference: `docs/plans/vapor-macos-task-list.md`.

## Local script entry points

- Build both stacks (release): `./scripts/build.sh`
- Build + package macOS app bundle: `./scripts/build.sh package`
- Clean build and dist artifacts: `./scripts/clean.sh`
- Lint both stacks: `./scripts/lint.sh`
- Format both stacks: `./scripts/format.sh`
- Format check both stacks: `./scripts/format.sh check`
- Test both stacks: `./scripts/test.sh`
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
- macOS app packaging: `apps/macos/scripts/package.sh`

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

## Release build policy

- A single release mode is used and tuned for performance with safe optimizations.
- The GitHub release pipeline validates the tag ref first, then runs `lint.yml`, `test.yml`, and `perf.yml` in parallel; the `release` job proceeds only with `needs: [preflight, lint, test, perf]`.
- Stable tag releases provision signing and notarization material on the runner before packaging.
- Rust release profile uses `opt-level=3`, `lto=fat`, `codegen-units=1`, `panic=abort`, and `strip=symbols`.
- Swift release build uses whole-module and cross-module optimization flags.
