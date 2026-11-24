# Specs

One file per designed feature. A spec records what the project owner
asked for, the facts the design started from, and every decision made
on the way, numbered D1, D2, and so on. It is how a later session
learns why Vapor works the way it does.

## How to use this group

- **Changing something that is already built?** Read the specs that
  cover it first. A change that contradicts a recorded decision is a
  question for the owner, not an edit (`AGENTS.md` §1.2).
- **Designing a feature?** Follow the `vapor-design` skill. It holds
  the question format, the spec template, and the rules for recording
  a decision.
- **Looking for how the system works now?** Go to
  `docs/architecture/README.md`. A spec records decisions and their
  reasons. It is not rewritten to follow the code.

## How specs relate to the other groups

- `docs/plans/` holds the scope and the non-goals of each surface.
- `docs/tasks/` holds what is next and in which order.
- `docs/specs/` holds the decisions of one feature and the reasons
  behind them.
- `docs/architecture/` describes the system as built. It must make
  sense to a reader who has never seen a spec, so decision numbers
  stay out of it and out of code comments (`AGENTS.md` §8.9).

## File convention

- Name: `YYYY-MM-DD-<slug>.md`, dated the day the design starts, with a
  short kebab-case slug.
- Status line: `designing`, `agreed`, or `implemented YYYY-MM-DD`. The
  design sets `agreed` when the owner says to start, and the change
  that builds the feature sets `implemented`.
- Decisions are never rewritten. A later decision replaces an earlier
  one by number ("D9 replaces D4"), and the earlier one is marked. A
  change to a shipped feature is a new round in the same spec.
- Specs are public, like the rest of the repository
  (`AGENTS.md` §6.1).

## Documents

Each spec gets a row here in the change that adds it. The status lives
in the spec file only. A decision made before 2026-11-14 gets a spec
when its area is next changed; there is no backfill.

| Spec | What it decides |
|---|---|
| `2026-11-21-isolated-macos-e2e.md` | Tier VM, the tier in which an agent tests the Mac app and the install as a user does: in a macOS virtual machine made with Tart, on the newest macOS that Tart has an image for, so that the contributor's Mac holds nothing from a test. It tests the unchanged installer, an agent acts on the app through accessibility, a scenario gets a fresh guest or a shared one by what it tests, and the owner judges the look from screenshots. Holds the measured results of the spike. |
| `2026-11-21-vapor-init.md` | `vapor init`, the one command for the first run from a terminal: its steps and their order, that it is the same onboarding as the apps', start at login and lingering asked with the answer yes by default, a later run that does only what is left, and flags for a run with no terminal. |
| `2026-11-21-platform-release-assets.md` | What a release holds on each platform: one install package that holds the app, the CLI, and the daemon, no standalone CLI, asset names that say the platform and the architecture, `vapor` on the `PATH` at install time, one update for all three, a `.pkg` on macOS, a `.deb` and an `.rpm` on Linux that install on a server with no desktop, and no update check for now. |
| `2026-11-24-core-cli-strengthening.md` | What the second round on the core and the CLI built with no design round, the choices it made that wait for the owner (which changes of `vapor.json` need no restart, the lane of small transfers, one transaction for each tick, how long a sign-in waits, what a locked keychain means, the proxy), and the first design round on the mass-delete guard, whose questions have no answer yet. |
| `2026-11-23-core-runtime-completion.md` | What the completion of the core runtime built with no design round, and the choices it made that wait for the owner: the name of a version that a second upload replaced, the durability of a transferred file, when an intent counts as blocked, the times of a burst and of the debounce, how often the cloud is asked, a file and a folder under one name, the sign-in on a build with no Developer ID, and when the supervisor ends a daemon. |
| `2026-11-14-full-review.md` | What the full review of the repository changed: when a deletion propagates, how a change of spelling syncs, that a throttle state slows sync and does not stop it, measured CPU and bandwidth ceilings, no folder before the user chose, the one-way warning as a decision, no state migrations before GA, ignore-rule semantics, the redacted report, and the release rules. Also the choices that still wait for the owner. |
