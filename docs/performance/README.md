# Performance

Measurable performance and reliability targets, plus the harness that
validates them. Use this directory when you want to understand Vapor's
explicit SLOs, how they are enforced, or how to reproduce a performance
scenario locally.

## How to use this group

- **Shipping a feature?** Check the relevant SLO in
  `acceptance-budgets-and-benchmark-harness.md` and run
  `./scripts/perf.sh` before merging if the change is performance-
  sensitive.
- **Investigating a regression?** The harness scenarios (idle, active
  coding, storm, network degradation, crash durability) are the
  reproducible starting points. Thresholds are explicit; pass/fail is
  mechanical.
- **Tuning the throttle or budget system?** SLOs constrain what you can
  change: user resource ceilings must hold under every scenario, and
  idle-boost must never preempt `Suspended`.
- **Enforcing in CI?** `docs/ci/README.md` covers how `perf.yml` is
  invoked and which thresholds gate a release.

## Documents

- `acceptance-budgets-and-benchmark-harness.md` — five-scenario SLO
  matrix (idle impact, active load, storm backpressure, network
  degradation, crash durability) with explicit CPU / memory / disk /
  latency thresholds; benchmark scenarios; cross-product coverage
  requirements when layered with user resource ceilings and idle boost.

## Related references

- `docs/ci/overview.md` — where `perf.yml` is invoked and how it gates
  releases.
- `docs/ci/required-checks.md` — required status checks; `perf` is a
  release-only gate, not a PR-required check.
- `docs/tasks/core.md` — the runtime tasks that land (and tighten) these
  SLOs.

This area tracks measurable targets and repeatable validation methods;
SLO thresholds are expected to tighten as benchmark coverage matures.
Per-OS absolute numbers may differ on Windows/Linux once those platforms
ship; *ratios* (idle vs active, storm cap) must stay consistent.
