# Product

Product direction: what Vapor is, what it is *not*, and where it is on
the roadmap. Use this directory when you need shared vocabulary with the
project owner about scope, non-goals, guarantees, and current shipping
status.

## How to use this group

- **Aligning on scope?** `status-and-goals.md` is the single living
  answer to "what is Vapor committing to and what is explicitly
  deferred". If a PR or plan contradicts it, update this doc in the
  same change.
- **Deciding whether a feature belongs in MVP?** Check the non-goals and
  deferred items here before writing a plan.
- **Communicating with users about status?** The root `README.md`
  **Features** section surfaces user-facing language; this doc is the
  source of truth the README draws from.

## Documents

- `status-and-goals.md`: current shipping stage (pre-GA), active work
  phase, provider roadmap (filesystem reference → Google Drive →
  additional adapters), product goals, runtime invariants (throttle
  states, user resource ceilings, idle boost, graceful shutdown,
  crash-loop pause), pre-GA compatibility policy.
- `configuration.md`: every `vapor.json` key in detail: when a change
  applies, the edge cases and floors, and how profiles merge. The root
  `README.md` keeps the one-table summary.
- `ignore-rules.md`: which files stay out of sync: the four sources of
  rules and their precedence, how a rule is read, the built-in list,
  what no rule can change, and `vapor ignored`. The root `README.md`
  links here from its Configuration table.
- `install.md`: the install file of each device, the install, update,
  and removal steps, the first run with `vapor init` on a terminal and
  with no terminal, and what to know before it (an unsigned
  prerelease, the macOS folder prompts, the Google Drive limits). The
  root `README.md` keeps the table and the short steps.

## Related references

- `docs/plans/README.md`: how product direction turns into per-surface
  plans (`core`, `macos`, `cli`, …).
- `docs/tasks/README.md`: what is actively being executed next and
  across which surfaces.
