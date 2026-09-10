# tools

Development tools that live in the Cargo workspace so `format`, `lint`,
and `test` cover them, but never ship in a release artifact.

- `e2e/` — `vapor-e2e`, the Tier E2E harness: drives the real `vapor`
  and `vapord` binaries black-box inside disposable sandboxes under
  `.vapor/e2e/`. Entry point `./scripts/e2e.sh`; process doc
  `docs/development/e2e-verification.md`; procedure the `vapor-e2e`
  skill.

- `soak/` — `vapor-soak`, the Tier S driver: hours of seeded file
  churn on both sides of a real daemon, fault injection, and a
  model-checked no-loss oracle after every phase. Reuses the harness
  library. Entry point `./scripts/soak.sh`; process doc
  `docs/development/soak-testing.md`; procedure the `vapor-soak` skill.
