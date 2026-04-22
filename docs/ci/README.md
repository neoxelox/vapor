# CI

GitHub Actions workflows and the policy around them. Use this directory
when you want to understand which checks run, where they run, what
triggers them, and which ones gate a release.

## How to use this group

- **Adding a new workflow or modifying an existing one?** Read
  `overview.md` first for the trigger policy and toolchain defaults.
- **Configuring branch protection?** Use `required-checks.md` — it lists
  exactly which checks must be required on `main` and which are
  release-only gates.
- **Running the same validation locally?** Every CI workflow runs the
  same `./scripts/*` entry points a contributor would run locally. See
  `docs/development/runbook.md` for the local command set.

## Documents

- `overview.md` — workflow catalog (`lint.yml`, `test.yml`, `perf.yml`,
  `release.yml`), triggers (`pull_request`, `push` to `main`,
  `workflow_call`, `v*` tags), toolchain defaults (`macos-latest`,
  latest-stable Xcode/Swift, stable Rust), pinned CI action versions,
  dependency caching strategy.
- `required-checks.md` — branch-protection guidance (`lint` and `test`
  required on `main`; `perf` release-only), workflow-to-script mapping,
  local-parity command set.

## Target state (portability work)

As the `core/*` portability fixes land (see `docs/plans/core.md` §4 and
`docs/tasks/core.md` Phase C1), the Rust jobs in `lint.yml` and
`test.yml` will grow a matrix of `macos-latest`, `ubuntu-latest`,
`windows-latest`. Swift jobs stay macOS-only because Swift code is
macOS-only by policy (`AGENTS.md §8`). Per-platform release jobs
(macOS, Windows, Linux) each target their own isolated GitHub
Environment (`release-macos`, `release-windows`, `release-linux`) for
secret isolation.
