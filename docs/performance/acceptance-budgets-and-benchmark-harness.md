# Acceptance Budgets and Benchmark Harness

## Scope

Define measurable performance and reliability targets with explicit pass/fail thresholds.
All thresholds below are initial SLOs and are intended to tighten as benchmark coverage matures.

Each threshold carries its state, so nobody reads a target as a
measured fact:

- **gated**: `scripts/perf.sh` asserts it on the report of the Tier 2
  soak cell, and a release does not ship without it.
- **reported**: the soak report holds the number; nothing fails on it.
- **tested**: a Tier 1 test asserts the rule in a constructed case; no
  run measures it on a live daemon.
- **not measured**: a target with no tool behind it yet
  (`docs/tasks/core.md`).

## Scenario SLO thresholds

### SLO-1 Idle impact budget (10 minute idle window)

| Threshold | State |
|---|---|
| Daemon CPU average <= 1.0% | reported (the `quiet` phases of the soak cell) |
| Daemon CPU p95 <= 3.0% | not measured |
| Disk read+write throughput p95 <= 1.5 MB/s | not measured |
| Throttle state stays out of `Suspended` for >= 99% of sampled intervals | not measured |

### SLO-2 Load protection

| Threshold | State |
|---|---|
| Daemon CPU average <= 5.0% across the cell, throttle walk included | gated |
| Daemon CPU p95 <= 12.0% | not measured |
| Work outside `Idle` stays inside the caps of the state: one planner, one hash worker, one upload, and one download in `Throttled` | tested |
| The daemon uses well under half a core while a transfer waits for bandwidth | tested (e2e S59) |
| p95 intent-to-queued latency <= 2.0 seconds | not measured |

### SLO-3 Storm backpressure and bounded memory

| Threshold | State |
|---|---|
| Event/intent bookkeeping stays bounded (global pending path entries <= 20_000, per-subtree pending entries <= 5_000 before coalesce/reconcile deferral) | tested |
| Daemon RSS p95 <= 350 MB | gated |
| No DB, hash, or network work in the callback path | tested (the `timing_guardrail_*` tests) |
| After pressure clears, pending intents converge to < 500 within 15 minutes | not measured |

### SLO-4 Network degradation and rate-limit resilience

| Threshold | State |
|---|---|
| Retry backoff is exponential with jitter, at most 15 minutes, and a provider `Retry-After` is honoured even when longer | tested |
| On a 429/5xx burst, uploads, downloads, and planner probes narrow to one each within 10 seconds | tested |
| After network recovery, the failed-attempt rate falls below 5% within 10 minutes | not measured |

No run measures SLO-4 on a live provider until the Google Drive soak
mode exists.

### SLO-5 Crash/restart durability and replay

| Threshold | State |
|---|---|
| A crash loses zero persisted intents, and every injected crash is followed by a phase that converges with no oracle violation | gated |
| Every phase of the cell converges inside its deadline | gated |
| Replay begins within 30 seconds after daemon restart | not measured |
| On an idle device, replay of 10_000 queued intents reaches >= 95% completion within 5 minutes | not measured |

## SLO applicability under user resource ceilings

SLO-1 through SLO-5 above are measured against default `resourceLimits` (`cpuPercent: 15`, `memoryPercent: 10`, `bandwidthPercent: 25`) with `idleBoost` enabled at defaults. User-configurable ceilings do not relax these SLOs; they tighten the throttle controller's admission envelope so Vapor stays inside its budget at all times.

- **Lowered ceilings.** When a user lowers `resourceLimits.*Percent` below defaults, the SLO thresholds still apply as stated. The throttle controller is expected to engage `Light`/`Throttled`/`Suspended` earlier (at lower Vapor-attributable CPU / measured bandwidth), which keeps the daemon inside the SLO envelope by construction. A test case at `resourceLimits.cpuPercent = 5` must still pass SLO-1 idle CPU p95 `<= 3.0%` and SLO-2 active CPU p95 `<= 12.0%`.
- **Raised ceilings.** User values above defaults are allowed but never relax the throttle controller's decision points; SLO thresholds still apply because the throttle controller remains the binding constraint at the SLO-level workload mix.
- **Idle-boost engaged.** Under an active idle boost, CPU/memory/bandwidth may transiently approach `boost*Percent` while the machine is genuinely idle. SLOs are evaluated over representative workload windows that explicitly include idle-boost-eligible periods; boost-driven transient headroom consumption does not count as an SLO violation as long as the throttle state is `Idle` and all idle-boost gating conditions hold.
- **Profile overrides.** Effective daemon ceilings resolve by MIN-lowering across global and enabled-profile values (see `docs/architecture/data-flow.md`). SLO runs must cover representative profile configurations: global-only, single profile with override, and two profiles with divergent overrides.

The resource-ceiling integration tests (open work, `docs/tasks/core.md`) must cover the following cross-product: `{default, cpuPercent=5, memoryPercent=5}` x `{idle, active, storm}` x `{boost-enabled, boost-disabled}` x `{global-only, profile-override-lowered}`. Each cell asserts the relevant SLOs above.

## Benchmark scenarios

- Idle machine with low event throughput.
- Active coding workload with frequent small file writes.
- Event storm burst with thousands of path updates.
- Network degradation and API rate-limit responses.
- Restart during pending queue replay.

## Harness expectations

- Deterministic fixture generation for each scenario.
- Scripted execution for local and CI use.
- Metrics artifact output per run with scenario-level summaries.
- Threshold-based pass/fail evaluation against this document.
- Current Rust micro-regression coverage includes callback burst, debounce tick, and scheduler superseding hot-path guards inside the daemon test suite.
- Current Rust stress coverage also exercises large per-subtree caps, large global caps, and multi-subtree deferred-storm markers so bounded in-memory behavior stays regression-tested.

## Release gate: one soak cell (Tier 2)

`scripts/perf.sh`, invoked by `release.yml` through `perf.yml`, runs
one soak cell against the release profile (`./scripts/soak.sh
--release --mode two-way --load mixed --faults crash --throttle walk`,
twelve minutes by default, `VAPOR_PERF_SOAK_DURATION` overrides) and
asserts on its report:

- zero oracle violations (nothing lost, nothing invented, both trees
  converged after every phase);
- every phase converged inside its deadline and every injected crash
  was followed by a clean phase (SLO-5);
- daemon RSS p95 under 350 MB (SLO-3);
- daemon CPU average under 5% across the cell, throttle walk included
  (SLO-2).

Those five checks are every "gated" row above. The soak driver, its
model, and how to read its report are in
`docs/development/soak-testing.md`.

## Reporting policy

- Record baseline results for each release line.
- Track regressions by scenario and metric.
- Include a soak report reference (`soak-last-report.json` or the CI artifact) in PRs that change daemon hot paths, throttling, scheduler behavior, deletion, or provider execution flow.
