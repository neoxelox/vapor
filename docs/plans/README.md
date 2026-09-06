# Plans

High-level planning artifacts. Use this directory when you want to
understand *what* Vapor is building and *why* — scope, principles, hard
constraints, milestone ordering, and non-goals for each deliverable
surface. Concrete step-by-step execution lives next door in
`docs/tasks/`.

## How to use this group

- **Deciding where to contribute?** Start with `docs/product/status-and-goals.md` for
  product intent. Then read `core.md` to understand the portable runtime strategy.
  Then pick a surface plan (`macos.md`, `cli.md`, future `windows.md` /
  `linux.md`) matching what you want to work on.
- **Writing code?** Plans give you the *why*; jump to
  `docs/tasks/README.md` for the prioritized *what*.
- **Writing or reviewing a doc/spec?** Plans are where scope/non-goals
  are decided. When a plan and a code change disagree, the plan is the
  authoritative intent — update it explicitly if the scope is changing.

## File convention

Plans are flat and platform-named: one file per deliverable surface.

- `core.md` — portable Rust runtime plan. Covers `core/daemon`,
  `core/providers`, `core/shared`, the planned `core/platform`
  abstraction layer, `core/lifecycle`, and the `vapor` CLI. This is the
  plan that powers every app surface.
- `macos.md` — macOS app surface plan (`apps/macos`): SwiftUI shell,
  menubar UX, macOS distribution trust chain.
- `cli.md` — `vapor` CLI plan: headless-first control plane shared across
  every OS.
- `windows.md` — placeholder. Added when `apps/windows` starts.
- `linux.md` — placeholder. Added when `apps/linux` starts.

## How plans relate to tasks

Every plan in this directory has a matching task list in `docs/tasks/`
with the same filename. The plan captures *intent*; the task list tracks
*execution*.

| Plan | Task list |
|---|---|
| `core.md` | `docs/tasks/core.md` |
| `macos.md` | `docs/tasks/macos.md` |
| `cli.md` | `docs/tasks/cli.md` |

`docs/tasks/README.md` is the cross-surface roadmap — the "what should be
done next" guide when multiple task lists contain pending items.

## Usage flow

1. Read `core.md` for product intent, non-negotiables, the portable
   runtime framing, platform traits, and execution sequence.
2. Read the platform-specific plan matching your surface (`macos.md`,
   `cli.md`, …).
3. Execute against the matching `docs/tasks/<surface>.md` and check the
   cross-surface roadmap in `docs/tasks/README.md` before picking up a
   new task.
