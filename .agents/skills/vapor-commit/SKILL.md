---
name: vapor-commit
description: Shapes Vapor commits (Conventional Commit types mapped to labels and release categories, one cohesive change per commit, why-focused messages, the public-repository check, and the no-push rule). Use when creating a commit or splitting a change set into commits. The pull request itself, from squash to merge-ready, is the vapor-pr skill.
license: GPL-3.0-only
---

# Commit

- One commit per cohesive change while you work: small and easy to
  revert. The branch is squashed to one commit when its PR opens
  (`vapor-pr`), and the owner merges every PR as a squash, so the PR
  title is what lands on `main`.
- A PR carries one dominant type. Split a mixed change into separate
  PRs when practical, so the release notes group it correctly.
- Subject: `<type>: <why-focused summary>` in the imperative, under 72
  characters. Body: what was wrong, what changed, what a user sees.
- Types, labels, and release categories (`.github/release.yml` reads the
  label of the PR):

| Type | Label | Category |
|---|---|---|
| `feat` | `feat` / `feature` | Features |
| `fix` | `fix` / `bug` / `bugfix` | Fixes |
| `perf` | `perf` | Performance |
| `refactor` | `refactor` | Refactors |
| `docs` | `docs` | Docs |
| `test` | `test` | Testing |
| `ci` | `ci` | Tooling |
| `build` | `build` | Tooling |
| `chore` | `chore` | Tooling |
| `release` | `release` | Tooling |

## Before the commit

- Run the `vapor-validate` ladder; the optional pre-commit hook
  (`./scripts/hooks.sh`) runs lint, test, and build.
- Do the `vapor-docs` duties, the `CHANGELOG.md` `Unreleased` line
  included.
- Read `git status --porcelain --untracked-files=all`. Stage the
  intended paths by name. Never commit `.env`, build output, or editor
  state.
- The repository is public. Read the diff, the message, and the branch
  name for personal information (`AGENTS.md` §6.1).
- Never commit a secret; push protection blocks it, and
  `CONTRIBUTING.md` says what to do when it does.
- Keep any trailer the harness requires.

## Never

- Never commit on `main`.
- Never push unless the project owner asks. A request to open a PR
  covers that PR's branch (`vapor-pr`). A release request authorises
  exactly one push (`vapor-release`). Nothing else is covered.
