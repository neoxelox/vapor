# CI required checks guidance

This project uses repository script entry points as the source of truth for local and CI validation.

See `docs/ci/overview.md` for workflow scope, triggers, and pinned action versions.

## Required GitHub status checks

`main` is protected by a repository **ruleset** named `main` (Settings →
Rules → Rulesets). It requires every matrix leg from `lint.yml`,
`test.yml`, and `build.yml`. The matrix names land as separate checks, so
the required-checks list is:

- `lint (macos-latest)`
- `lint (ubuntu-latest)`
- `lint (windows-latest)`
- `test (macos-latest)`
- `test (ubuntu-latest)`
- `test (windows-latest)`
- `build (macos-latest)`
- `build (ubuntu-latest)`
- `build (windows-latest)`

Each entry is bound to the GitHub Actions app (`integration_id: 15368`),
so a check with the same name reported by any other app does not satisfy
the rule.

Linux and Windows run only the Rust workspace (Swift wrappers skip on
non-Darwin); macOS runs both stacks. A regression on any host fails the PR
because `fail-fast` is disabled in the matrix.

When a matrix leg is renamed (for example a runner label change), update
the ruleset in the same change set. A required check that no workflow
reports any more blocks every merge until an admin bypasses it.

Release-only gates:

- `lint`, `test`, and `perf` are reusable workflows invoked by `release.yml` for versioned releases.
- `perf` is not a pull-request required status check.

### Ruleset configuration

The ruleset targets the default branch and enforces:

- **Pull request required** before merging, with one approval; stale
  approvals are dismissed when new commits are pushed, and pull requests
  authored by the Copilot coding agent ("unattributed changes") need one
  approval more than that. A solo maintainer cannot approve their own pull
  request, so today every merge goes through the admin bypass below by
  design; the approval rules start doing work the day a second maintainer
  exists.
- **Required status checks**: the nine checks above, on a branch that is
  up to date with `main` (`strict_required_status_checks_policy: true`).
  Once `main` moves, a pull request must pick up the new base and re-run
  CI before it can merge, so what lands is exactly what was tested.
- **No force pushes** (`non_fast_forward`) and **no deletion** of `main`.
- **Bypass**: the *Repository admin* role, mode `always`. It does two
  jobs. With a single maintainer it is how pull requests get merged at
  all (GitHub offers *Merge without waiting for requirements*). And the
  release flow pushes the release-prep commit and its tag straight to
  `main` (`git push origin main --follow-tags`, see
  `docs/operations/release-process.md`), which the ruleset would otherwise
  reject. GitHub records every bypass in the push output and the audit
  log. Narrow it the day a second admin exists, alongside
  `can_admins_bypass` on the release environments (`AGENTS.md` §7.1).

Inspect the live configuration:

```bash
gh api repos/neoxelox/vapor/rulesets --jq '.[] | {id,name,enforcement}'
gh api repos/neoxelox/vapor/rules/branches/main --jq '[.[] | .type]'
```

Recreate it from scratch (for example on a fresh repository):

```bash
gh api -X POST repos/neoxelox/vapor/rulesets --input - <<'EOF'
{
  "name": "main",
  "target": "branch",
  "enforcement": "active",
  "conditions": { "ref_name": { "include": ["~DEFAULT_BRANCH"], "exclude": [] } },
  "bypass_actors": [
    { "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always" }
  ],
  "rules": [
    { "type": "deletion" },
    { "type": "non_fast_forward" },
    { "type": "pull_request", "parameters": {
        "required_approving_review_count": 1,
        "dismiss_stale_reviews_on_push": true,
        "require_code_owner_review": false,
        "require_last_push_approval": false,
        "required_review_thread_resolution": false,
        "require_extra_approval_for_unattributed_changes": true } },
    { "type": "required_status_checks", "parameters": {
        "strict_required_status_checks_policy": true,
        "do_not_enforce_on_create": false,
        "required_status_checks": [
          { "context": "lint (macos-latest)",    "integration_id": 15368 },
          { "context": "lint (ubuntu-latest)",   "integration_id": 15368 },
          { "context": "lint (windows-latest)",  "integration_id": 15368 },
          { "context": "test (macos-latest)",    "integration_id": 15368 },
          { "context": "test (ubuntu-latest)",   "integration_id": 15368 },
          { "context": "test (windows-latest)",  "integration_id": 15368 },
          { "context": "build (macos-latest)",   "integration_id": 15368 },
          { "context": "build (ubuntu-latest)",  "integration_id": 15368 },
          { "context": "build (windows-latest)", "integration_id": 15368 } ] } }
  ]
}
EOF
```

`actor_id: 5` is GitHub's fixed id for the *Repository admin* role.

## Workflow to script mapping

Execution environment defaults:

- GitHub runners: `macos-latest` (full Rust + Swift), `ubuntu-latest` and
  `windows-latest` (Rust workspace only). The `-latest` aliases follow
  GitHub's newest stable image (`AGENTS.md` §8.2); `macos-latest` is the
  macOS 26 arm64 image at the time of writing.
- Xcode/Swift toolchain: `latest-stable` via `setup-xcode` (macOS leg only)
- Rust toolchain: `stable` (every leg)

Pinned CI actions (every `uses:` is pinned to a commit SHA with a trailing
`# vX.Y.Z` comment, and the repository enforces `sha_pinning_required`):

- `actions/checkout` v7.0.1
- `maxim-lobanov/setup-xcode` v1.7.0
- `actions-rust-lang/setup-rust-toolchain` v1.17.0
- `actions/cache` v5.1.0

Dependency caches used in CI:

- Rust: `~/.cargo/bin`, `~/.cargo/registry/index`, `~/.cargo/registry/cache`, `~/.cargo/git/db`, `target`
- SwiftPM: `.build`

Dependency source defaults:

- Rust crates: `crates.io` via Cargo
- Swift packages: SwiftPM

- Lint workflow (`.github/workflows/lint.yml`)
  - triggers: `pull_request`, `push` to `main`, `workflow_call`
  - `./scripts/lint.sh`
- Test workflow (`.github/workflows/test.yml`)
  - triggers: `pull_request`, `push` to `main`, `workflow_call`
  - `./scripts/test.sh`
- Build workflow (`.github/workflows/build.yml`)
  - triggers: `pull_request`, `push` to `main`, `workflow_call`
  - `./scripts/build.sh`
- Perf workflow (`.github/workflows/perf.yml`)
  - trigger: `workflow_call` from `.github/workflows/release.yml`
  - thresholds via env: `VAPOR_PERF_SMOKE_RUST_MAX_SECONDS` (default `600`), `VAPOR_PERF_SMOKE_SWIFT_MAX_SECONDS` (default `900`)
  - `./scripts/perf.sh`
- Release workflow (`.github/workflows/release.yml`)
  - trigger: pushed `v*` tags
  - validates tag format, `VERSION` match, and tag ancestry on `main`
  - calls `lint`, `test`, and `perf` in parallel
  - `release` job declares `needs: [preflight, lint, test, perf]`
  - `release` job targets GitHub Environment `release-macos`
  - `./scripts/build.sh package`
  - `gh release create/edit/upload`

## Local parity command set

Run the same validations locally before opening a PR:

- `./scripts/lint.sh`
- `./scripts/test.sh`
- `./scripts/build.sh`

For performance-sensitive changes, run:

- `./scripts/perf.sh`

`./scripts/lint.sh` includes format checks (`./scripts/format.sh check`).

Release workflow is tag-driven and should not be configured as a required status check for pull requests on `main`.
