# Acceptance Budgets and Benchmark Harness

## Scope

Define measurable performance and reliability targets with explicit pass/fail thresholds.
All thresholds below are initial SLOs and are intended to tighten as benchmark coverage matures.

## Scenario SLO thresholds

### SLO-1 Idle impact budget (10 minute idle window)

- Daemon CPU average <= 1.0%.
- Daemon CPU p95 <= 3.0%.
- Disk read+write throughput p95 <= 1.5 MB/s.
- Throttle state stays out of `Suspended` for >= 99% of sampled intervals.

### SLO-2 Active coding/load protection

- Daemon CPU average <= 5.0%.
- Daemon CPU p95 <= 12.0%.
- No sustained reconcile work outside `IdleDrain` (no contiguous reconcile run > 10 seconds in `Light`/`Throttled`/`Suspended`).
- p95 intent-to-queued latency <= 2.0 seconds.

### SLO-3 Storm backpressure and bounded memory

- Event/intent bookkeeping remains bounded (global pending path entries <= 20_000, per-subtree pending entries <= 5_000 before coalesce/reconcile deferral).
- Daemon RSS p95 <= 350 MB during storm scenario.
- Under storm mode, heavy work remains throttle-gated and callback invariants hold (no DB/hash/network in callback path).
- After pressure clears, pending intents converge to < 500 within 15 minutes.

### SLO-4 Network degradation and rate-limit resilience

- Locally computed retry policy applies exponential backoff + jitter with max delay <= 15 minutes, while explicit provider `Retry-After` floors are honored even when longer.
- On 429/5xx burst, request aggressiveness downshifts within 10 seconds.
- After network recovery, failed-attempt rate falls below 5% within 10 minutes.

### SLO-5 Crash/restart durability and replay

- Crash/restart loses zero persisted intents (pre-crash vs post-restart durable intent count delta = 0, excluding successfully completed intents).
- Replay begins within 30 seconds after daemon restart.
- On idle device, replay of 10_000 queued intents reaches >= 95% completion within 5 minutes.

## SLO applicability under user resource ceilings

SLO-1 through SLO-5 above are measured against default `resourceLimits` (`cpuPercent: 15`, `memoryPercent: 10`, `bandwidthPercent: 25`) with `idleBoost` enabled at defaults. User-configurable ceilings do not relax these SLOs; they tighten the throttle controller's admission envelope so Vapor stays inside its budget at all times.

- **Lowered ceilings.** When a user lowers `resourceLimits.*Percent` below defaults, the SLO thresholds still apply as stated. The throttle controller is expected to engage `Light`/`Throttled`/`Suspended` earlier (at lower Vapor-attributable CPU / measured bandwidth), which keeps the daemon inside the SLO envelope by construction. A test case at `resourceLimits.cpuPercent = 5` must still pass SLO-1 idle CPU p95 `<= 3.0%` and SLO-2 active CPU p95 `<= 12.0%`.
- **Raised ceilings.** User values above defaults are allowed but never relax the throttle controller's decision points; SLO thresholds still apply because the throttle controller remains the binding constraint at the SLO-level workload mix.
- **Idle-boost engaged.** Under an active idle boost, CPU/memory/bandwidth may transiently approach `boost*Percent` while the machine is genuinely idle. SLOs are evaluated over representative workload windows that explicitly include idle-boost-eligible periods; boost-driven transient headroom consumption does not count as an SLO violation as long as the throttle state is `IdleDrain` and all idle-boost gating conditions hold.
- **Profile overrides.** Effective daemon ceilings resolve by MIN-lowering across global and enabled-profile values (see `docs/architecture/data-flow.md`). SLO runs must cover representative profile configurations: global-only, single profile with override, and two profiles with divergent overrides.

The resource-ceiling integration tests (open work, `docs/tasks/core.md` T-16) must cover the following cross-product: `{default, cpuPercent=5, memoryPercent=5}` x `{idle, active, storm}` x `{boost-enabled, boost-disabled}` x `{global-only, profile-override-lowered}`. Each cell asserts the relevant SLOs above.

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

## CI smoke gate thresholds (initial proxy gate)

The `perf` workflow currently enforces a coarse proxy gate while the full benchmark harness is being expanded. It is invoked by `release.yml` for versioned release runs only:

- CI-SMOKE-1: `./scripts/rust/test.sh` elapsed time <= 600 seconds.
- CI-SMOKE-2: `./scripts/swift/test.sh` elapsed time <= 900 seconds.

Override knobs (for controlled CI tuning):

- `VAPOR_PERF_SMOKE_RUST_MAX_SECONDS` (default `600`)
- `VAPOR_PERF_SMOKE_SWIFT_MAX_SECONDS` (default `900`)

These smoke thresholds are intentionally conservative and serve as early regression tripwires, not full performance certification.

## Reporting policy

- Record baseline results for each release line.
- Track regressions by scenario and metric.
- Include benchmark or smoke-run references in PRs that change daemon hot paths, throttling, scheduler behavior, or provider execution flow.
