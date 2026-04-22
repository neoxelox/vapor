# Vapor Documentation Index

Use this page as the default entrypoint for product, engineering, and
operations context.

## How the docs are organised

- Every group under `docs/` has a `README.md` that is the entrypoint for
  that group: it explains what the group covers, what each file is, and
  how to use them. **Always start at the group README**, not at
  individual files.
- Common (cross-platform) content lives at the top of each group
  (`docs/architecture/`, `docs/operations/`, …).
- Platform-specific content lives in per-platform subdirectories
  (`docs/architecture/macos/`, `docs/operations/macos/`, later
  `windows/`, `linux/`). Each platform subdirectory also has its own
  `README.md`.
- Plans and tasks are flat and platform-named: one file per deliverable
  surface (`docs/plans/{core,macos,cli}.md`,
  `docs/tasks/{core,macos,cli}.md`).
- `docs/tasks/README.md` is the **cross-surface roadmap orchestrator** —
  the "what should be done next" guide when multiple task lists have
  pending items.

## Groups

Jump to a group's `README.md` for an explanation of its contents and how
to use them.

- **Product** — direction, scope, status, non-goals:
  `docs/product/README.md`.
- **Architecture** — system design, contracts, platform abstractions:
  `docs/architecture/README.md`.
- **Operations** — release, signing, provider auth, logging, incident
  playbooks: `docs/operations/README.md`.
- **Development** — local runbook, scripts, toolchain baseline:
  `docs/development/README.md`.
- **CI** — GitHub Actions workflows and required-check policy:
  `docs/ci/README.md`.
- **Performance** — SLOs and benchmark harness:
  `docs/performance/README.md`.
- **Plans** — per-surface implementation plans (intent):
  `docs/plans/README.md`.
- **Tasks** — per-surface task lists and the cross-surface roadmap:
  `docs/tasks/README.md`.

## Quick intent mapping

- "What is Vapor and where is it going?" → `docs/product/README.md`
- "What should I work on next?" → `docs/tasks/README.md`
- "How does the runtime fit together?" → `docs/architecture/README.md`
- "How do I build / test / lint locally?" → `docs/development/README.md`
- "How is the release cut?" → `docs/operations/README.md`
- "Which CI checks must pass?" → `docs/ci/README.md`
- "Where are the performance SLOs?" → `docs/performance/README.md`
- "Should I write this test?" → `docs/architecture/testing-strategy.md` + `AGENTS.md §9`
- "What is the plan for surface X?" → `docs/plans/README.md`

The contributor operating rules live at the root: `AGENTS.md`.
