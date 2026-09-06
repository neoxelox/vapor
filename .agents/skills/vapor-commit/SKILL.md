---
name: vapor-commit
description: Shapes Vapor commits and pull requests (Conventional Commit types mapped to labels and release categories, one cohesive change per commit, why-focused messages, the no-push rule, and the PR description template). Use when creating a commit, splitting a change set into commits, or writing a PR description.
license: GPL-3.0-only
---

# Commit and PR

## Commits

- One commit per feature or tightly related change group; small,
  cohesive, rollback-friendly. Split mixed changes when practical so
  changelog grouping stays accurate.
- Subject: `<type>: <why-focused summary>` in the imperative, under 72
  characters. Body: what was wrong, what changed, what a user sees.
- Types, labels, and release categories (`.github/release.yml` reads the
  label):

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

- Run the `vapor-validate` ladder before committing; the optional
  pre-commit hook (`./scripts/hooks.sh`) runs lint, test, and build.
- Never push unless the project owner asks. A release request
  authorises exactly one push (see `vapor-release`); nothing else.
- Never commit a secret; push protection blocks it, and
  `CONTRIBUTING.md` says what to do when it does.

## Pull request description

Answer, in this order:

1. What user or reliability problem does this solve?
2. How does it preserve low-impact behaviour?
3. What durability or failure paths were validated?
4. What tests were added or updated?
5. What docs and contracts were updated (or why none were needed)?

Then:

- Scope and non-goals.
- Risk assessment and rollback plan.
- Migration or compatibility implications (pre-GA: say "none required"
  when a format changed without migration, and where it is documented).
- For UI changes, a manual verification checklist for the owner.
- End the body with the generated-with line the harness requires.

Label the PR with the dominant commit type.
