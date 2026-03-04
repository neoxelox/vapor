# CI required checks guidance

This project uses repository script entry points as the source of truth for local and CI validation.

## Required GitHub status checks

Configure branch protection for `main` to require the following workflow checks:

- `lint`
- `test`

## Workflow to script mapping

- Lint workflow (`.github/workflows/lint.yml`)
  - `./scripts/lint.sh`
  - `./scripts/format.sh check`
- Test workflow (`.github/workflows/test.yml`)
  - `./scripts/test.sh`

## Local parity command set

Run the same validations locally before opening a PR:

- `./scripts/lint.sh`
- `./scripts/format.sh check`
- `./scripts/test.sh`
