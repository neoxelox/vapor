# tools

Development tools that live in the Cargo workspace so `format`, `lint`,
and `test` cover them, but never ship in a release artifact.

- `e2e/` — `vapor-e2e`, the Tier E2E harness: drives the real `vapor`
  and `vapord` binaries black-box inside disposable sandboxes under
  `.vapor/e2e/`. Entry point `./scripts/e2e.sh`; process doc
  `docs/development/e2e-verification.md`; procedure the `vapor-e2e`
  skill.

Planned: `soak/` (`vapor-soak`), the long-run workload and model-checked
oracle that reuses the harness library (`docs/tasks/core.md` TR-9).
