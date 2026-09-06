---
name: vapor-docs
description: Keeps Vapor's documentation in step with a change (CHANGELOG Unreleased line, root README Features and Configuration, the owning docs/ file, group README entrypoints, plans and tasks, AGENTS.md when a rule changes), and applies the writing rules for each document class. Use for any non-trivial change and whenever a file is added, removed, or renamed under docs/.
license: GPL-3.0-only
---

# Keep the docs true

A change is not done until the documents that describe the behaviour
describe the new behaviour. Do this before the commit, not after.

## Every non-trivial change

1. `CHANGELOG.md` `Unreleased`: one entry that says what changed, why,
   and what a user or contributor sees differently. Group under
   `Added`, `Changed`, `Fixed`, `Docs`, or `Tooling / CI`.
2. The owning document under `docs/` (architecture for design,
   operations for runbooks, development for workflow, ci for pipeline,
   performance for budgets, product for status). If none owns it, add a
   file and register it in that group's `README.md`.
3. Root `README.md`: **Features** whenever a capability, guarantee, or
   supported behaviour changes; **Configuration** whenever a key,
   default, or `VAPOR_*` variable changes. Move a claim between
   "Available now" and "In flight and coming next" rather than delete
   it. Shortened README content moves into `docs/`, never disappears.
4. `docs/tasks/<surface>.md` and `docs/tasks/README.md`: close or add
   the task, and update the wave orchestrator when a wave opens, closes,
   or changes dependencies.
5. `AGENTS.md` when a durable rule, invariant, or contributor policy
   changes. Edit `AGENTS.md` and `.agents/` only; `CLAUDE.md` and
   `.claude/skills/*` are symlinks.
6. macOS UI changes: state the intended experience and its HIG
   alignment in the change, and hand the owner a manual checklist.

## Group README rule

Every `docs/<group>/` and `docs/<group>/<platform>/` has a `README.md`
that says what the group covers, describes each file (what it is, what
to expect, how to use it), links related groups, and changes in the same
change set as any file added, removed, or renamed there.

## Writing rules

- Root `README.md` is product-facing: concise, scannable, one emoji per
  Features bullet, user outcomes over internals, provider-agnostic
  wording, "device" not "laptop", no duplicated section content.
- Everything else may be technical, but no task ids or wave numbers in
  architecture, operations, development, or CI prose; those live in
  `docs/tasks/`. Pointers to `docs/tasks/` files are fine.
- Present tense for shipped behaviour; "planned" or "open work" with a
  task pointer for anything else. A document that says "will" about
  something that exists is a bug.
- Apply the `unslop` skill to every sentence you write.
- Code comments follow the same rule: state the constraint or the
  non-obvious why, never the history or the schedule.

## If docs are deliberately untouched

Say so in the PR description and why.
