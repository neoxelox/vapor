---
name: vapor-docs
description: Keeps Vapor's documentation in step with a change (CHANGELOG Unreleased line, root README Features and Configuration, the owning docs/ file, group README entrypoints, plans, tasks, and specs, AGENTS.md when a rule changes), and applies the writing rules for each document class. Use for any non-trivial change and whenever a file is added, removed, or renamed under docs/.
license: GPL-3.0-only
---

# Keep the docs true

A change is not done until the documents that describe the behaviour
describe the new behaviour. Do this before the commit, not after.

## Every non-trivial change

1. `CHANGELOG.md` `Unreleased`: one line for each pull request, under
   `Added`, `Changed`, `Fixed`, `Docs`, or `Tooling / CI` (one group of
   each in a section, no other group names). One or two sentences, 300
   characters at most, in words a user can notice: what is different
   for them, not how it was built. The section becomes the release
   body, and GitHub refuses a body over 125 000 characters. The detail
   belongs in the pull request and in the owning document.
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
   or changes dependencies. Every bug, broken thing, or possible
   improvement that the work found and did not fix gets a task here,
   so the owner can decide on it later (`AGENTS.md` §8). `docs/plans/<surface>.md` when the scope, a
   principle, or a non-goal of a surface changes.
5. `AGENTS.md` when a durable rule, invariant, or contributor policy
   changes. Edit `AGENTS.md` and `.agents/` only; `CLAUDE.md` and
   `.claude/skills/*` are symlinks.
6. `docs/specs/` when the change builds a spec (`vapor-design`): set
   the spec's status and add what was verified, in the same change
   set. The spec records why; the owning document under `docs/` still
   describes the new behaviour on its own.
7. macOS UI changes: state the intended experience and its HIG
   alignment in the change, and give the owner the screenshots of the
   Tier VM run with a list of what to judge (`vapor-vm`).

## The sweep for stale mentions

Before the last commit of a change that introduces, renames, or
removes a command, flag, path, config key, environment variable,
scenario id, decision kind, or tier name, search the whole knowledge
base for every old and new spelling and fix each stale mention:

```sh
git grep -n -e "<old spelling>" -e "<new spelling>" -- '*.md' .env.example
```

That covers every tracked Markdown file at every level (`AGENTS.md`,
the skills, each README, `CONTRIBUTING.md`, `SECURITY.md`, `docs/**`,
the CHANGELOG) and `.env.example`.
A default that changed shows up as a number, so search for the old
value too. A mention in a document that describes history (an older
CHANGELOG release, a dated task entry) stays; a mention in a document
that describes the system now is a bug.

## Group README rule

Every `docs/<group>/` and `docs/<group>/<platform>/` has a `README.md`
that says what the group covers, describes each file (what it is, what
to expect, how to use it), links related groups, and changes in the same
change set as any file added, removed, or renamed there.

## Writing rules

- Which style applies to which reader: `AGENTS.md` §8.11. Specs and PR
  descriptions use Simplified Technical English; the root README keeps
  its voice.
- Root `README.md` is product-facing: concise, scannable, one emoji per
  Features bullet, user outcomes over internals, provider-agnostic
  wording, "device" not "laptop", no duplicated section content.
- Everything else may be technical, but no task ids, wave numbers, or
  spec decision numbers in architecture, operations, development, or
  CI prose; those live in `docs/tasks/` and `docs/specs/`. Pointers to
  `docs/tasks/` files are fine. A system doc may link a spec file; a
  code comment may not (`AGENTS.md` §8.9).
- Present tense for shipped behaviour; "planned" or "open work" with a
  task pointer for anything else. A document that says "will" about
  something that exists is a bug.
- Apply the `unslop` skill to every sentence you write.
- Code comments follow the same rule: state the constraint or the
  non-obvious why, never the history or the schedule.

## If docs are deliberately untouched

Say so in the PR description and why.
