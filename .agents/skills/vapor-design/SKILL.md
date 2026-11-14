---
name: vapor-design
description: Designs a Vapor feature together with the project owner before any code exists. Gathers the facts first, then asks short rounds of numbered questions, each with its options and a bracketed recommendation, and records every answer as a numbered decision in a dated spec under docs/specs/, so later sessions know why Vapor works the way it does. Use when the owner asks for a new feature, a change to sync semantics, a redesign, or any change with more than one reasonable answer; and when a change would contradict a decision already recorded in docs/specs/.
license: GPL-3.0-only
---

# Design a feature with the owner

No code until the owner and the agent agree on what to build. The
design happens in rounds of questions, and every answer becomes a
numbered decision in a spec. Later sessions read the spec to learn why
Vapor works the way it does.

The format exists because it works: the owner can accept a whole round
with one word or correct a single line, and asking about requirements
before mechanisms keeps the design from jumping to a solution nobody
agreed to.

A fix with one obvious answer needs no spec. A runtime failure goes
through `vapor-debug`, which ends in its own fix plan.

## 1. Facts first

Before the first question, find out what is true.

- Read the code, the specs in `docs/specs/`, the architecture docs for
  the area, and the plan and tasks of the surface
  (`docs/plans/`, `docs/tasks/`).
- Name the invariants in `AGENTS.md` §3 to §6 that the feature touches.
  They are constraints, not questions.
- Check outside sources when a question depends on them: a provider's
  API limits, an OS API's behaviour, what another sync product does.
- Measure when the answer is a number. The e2e manual sandbox
  (`vapor-e2e`) runs the real daemon without touching `~/.vapor`.
- Name the source of each fact: a file and line, a URL, a measurement.
  Never claim a fact you did not check.

Open the reply with the findings: the current state, the constraints,
and anything that rules an idea out. If something the owner asked for
cannot work, say so plainly, show the evidence, and offer what can.

When a question has real alternatives, show them as a short table
before the round asks the owner to choose:

```md
| # | Option | What it gives | Cost or risk |
|---|---|---|---|
| 1 | ... | ... | ... |
```

## 2. Ask in rounds

Write the round in the reply as text, under a `## Round N` heading. A
multiple-choice question tool cannot carry a recommendation per
question or let the owner accept everything at once. Write findings,
questions, and decisions in Simplified Technical English
(`AGENTS.md` §8.11). Each question:

- states one decision, concretely;
- lists the options when there is more than one;
- ends with the agent's recommendation in brackets, plus the reason
  when it is not obvious.

An example round, for a quiet-hours feature that does not exist:

```md
## Round 1

**Q1.** During quiet hours, does Vapor stop transfers only, or the
reconcile scan too? [Both. The feature is "do not use my device now",
and a scan uses CPU and disk.]

**Q2.** Is the window one per day, or one per weekday? [One per day.
Nobody asked for a weekday grid yet.]

**Q3.** Does `vapor reconcile` run inside the window? [Yes. It is the
user's own request, the same exception the throttle states make.]
```

The owner answers "ok" to take every recommendation, or corrects
single lines ("Q2: one per weekday"). A question the owner skips stays
open; it is not accepted.

- What the user gets and what must never happen come before
  mechanisms. Settle the behaviour, the failure cases, and the device
  impact before the crates, the tables, and the wire shapes.
- Keep rounds small: roughly three to eight questions. Later rounds
  build on earlier decisions and cite them by number ("D3 says ...").
- Be critical. Push back with evidence, on the owner's ideas too.
- A question about the Mac app's look needs something the owner can
  judge: a precise description, the Apple HIG pattern it follows, or a
  reference app. Agents never verify UI, so the owner makes that call.
- Stop when the remaining questions would not change what gets built.
  Say the design is complete and ask to start.

## 3. Record every answer

After each round, write the answers into the spec before asking the
next round. The spec is `docs/specs/YYYY-MM-DD-<slug>.md`, dated the
day the design starts, with a short kebab-case slug. Add its row to
`docs/specs/README.md` in the same change.

```md
# <Feature name>

Status: designing | agreed | implemented YYYY-MM-DD

## Goal

What the owner asked for, in the owner's words where possible, and
what a user gets from it.

## Facts

What was true when the design started: the current behaviour, the
constraints, the invariants touched, research. Each fact with its
source.

## Decisions

### Round 1, YYYY-MM-DD

| # | Decision |
|---|---|
| D1 | ... |
| D2 | ... |

### Round 2, YYYY-MM-DD

| # | Decision |
|---|---|
| D3 | ... |

## Design

How it gets built once the decisions settle it: crates, contracts,
schema, config keys, and how each touched invariant still holds.

## Verification

What proves it works: the tests, the e2e scenarios, and the tiers
(`vapor-validate`), plus anything specific to this feature.

## Open points

Questions still unanswered, numbered.
```

- One decision per number. Numbers run per spec from D1 and are never
  reused or renumbered.
- Write a decision as the fact that now holds, with the reason when it
  is not obvious. Note who chose it when that matters:
  "(owner, 2026-11-14)".
- Never edit a decision to mean something else. A later round that
  changes it adds a new one ("D9 replaces D4: ..."), and D4 gets
  "Replaced by D9." at its end.
- An unanswered question stays under Open points. It never becomes a
  decision by default.
- The spec is public (`AGENTS.md` §6.1). No personal information, no
  raw account output.

## 4. Build

Start only when the owner says to. Build what the decisions say. If
the code cannot honour a decision, take it back to the owner as a
question; do not work around it. When the build raises a new choice,
ask it as a short round and record the answer like any other.

When the feature is done:

- set the status to `implemented YYYY-MM-DD` and add what was verified
  to the spec, in the same change set as the code;
- bring the system docs in step (`vapor-docs`). The spec records why;
  `docs/architecture/` describes what the system does now, and must
  stand on its own;
- keep decision numbers out of code comments and system docs
  (`AGENTS.md` §8.9). The PR description is where they are cited.

The spec stays in `docs/specs/` as the record.

## Before changing something already built

Read the specs that cover it. A change that contradicts a recorded
decision is a design question for the owner, not an edit.
