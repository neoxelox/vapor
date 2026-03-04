# Acceptance Budgets and Benchmark Harness

## Scope

Define measurable performance/reliability targets and repeatable validation for milestone gates.

## Target budget categories

- CPU impact budget (idle and active-workload scenarios)
- Disk I/O pressure budget
- Sync latency budget (p50/p95 eventual completion)
- Retry/rate-limit error budget
- Time in throttle states budget

## Initial benchmark scenarios

- Idle machine with low event throughput.
- Active coding workload with frequent small file writes.
- Event storm burst with thousands of path updates.
- Network degradation and API rate-limit responses.
- Restart during pending queue replay.

## Harness expectations

- Repeatable fixture generation (deterministic event sets).
- Scripted run mode for local and CI execution.
- Metrics collection window and report artifact output.
- Threshold-based pass/fail gates with regression detection.

## Reporting

- Record baseline for each major release line.
- Highlight regressions by scenario and metric.
- Link benchmark results in PRs for non-trivial engine changes.
