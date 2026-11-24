#!/usr/bin/env bash
# Tier 2 performance gate (a release gate, and on demand): the property
# tests at a case count that Tier 1 has no time for, then one bounded
# soak cell against the release profile, with the SLO checks from
# docs/performance/acceptance-budgets-and-benchmark-harness.md asserted
# on its report. Fuzz and loom runs join here as they land.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export VAPOR_ENV="${VAPOR_ENV:-dev}"

DURATION="${VAPOR_PERF_SOAK_DURATION:-12m}"
SEED="${VAPOR_PERF_SOAK_SEED:-11}"

# Tier 1 runs each property 256 times. A counterexample that shows one
# time in thousands needs thousands of cases.
PROPERTY_CASES="${VAPOR_PERF_PROPERTY_CASES:-4000}"
echo "[perf] property tests: $PROPERTY_CASES cases for each property"
# The tests get the runtime directory of every repository script.
VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}" PROPTEST_CASES="$PROPERTY_CASES" \
  cargo test --manifest-path "$ROOT_DIR/Cargo.toml" \
  -p vapor-daemon --test properties --release

echo "[perf] soak cell: two-way, mixed load, crash faults, release profile, $DURATION, seed $SEED"
report="$ROOT_DIR/.vapor/e2e/soak-last-report.json"
# A stale report from an earlier run must never stand in for this one.
rm -f "$report"
# The daemon under test lives in the driver's own sandbox, so this
# script sets no runtime directory. A host that cannot run the cell (the
# driver's exit 3, turned into a clean exit by the wrapper) leaves no
# report behind; the gate then reports the skip and exits clean, so a
# platform whose daemon has not shipped yet does not block a release.
"$ROOT_DIR/scripts/soak.sh" --duration "$DURATION" --seed "$SEED" --release \
  --mode two-way --load mixed --faults crash --throttle walk

if [[ ! -f "$report" ]]; then
  echo "[perf] skipped: this host cannot run the soak cell, so there is no report to check"
  exit 0
fi

python3 - "$report" <<'PY'
import json
import sys

report = json.load(open(sys.argv[1], encoding="utf-8"))
slo = report["slo"]
health = report["status"]["health"]
failures = []
if report["violations"]:
    failures.append(f"{len(report['violations'])} oracle violation(s)")
if not slo["every_phase_converged"]:
    failures.append("a phase did not converge (SLO-5 replay)")
if not slo["crashes_recovered_without_loss"]:
    failures.append("a crash was followed by loss (SLO-5)")
if not slo["rss_p95_within_budget"]:
    failures.append(
        f"RSS p95 {health['rss_p95_bytes'] // (1024 * 1024)} MiB over the {slo['rss_budget_bytes'] // (1024 * 1024)} MiB budget (SLO-3)"
    )
if not slo["cpu_avg_within_budget"]:
    failures.append(
        f"CPU average {health['cpu_avg_percent']:.1f}% over the {slo['cpu_budget_percent']:.0f}% budget (SLO-2)"
    )
print(
    f"[perf] phases={len(report['phases'])} ops={report['status']['ops_done']} "
    f"faults={len(report['faults'])} rss_p95={health['rss_p95_bytes'] // (1024 * 1024)}MiB "
    f"cpu_avg={health['cpu_avg_percent']:.1f}% threads_max={health['threads_max']}"
)
if failures:
    for failure in failures:
        print(f"[perf] FAIL {failure}")
    sys.exit(1)
print("[perf] all SLO checks passed")
PY
