#!/usr/bin/env bash
# Tier 2 performance gate (release pipeline only): one bounded soak cell
# against the release profile, with the SLO checks from
# docs/performance/acceptance-budgets-and-benchmark-harness.md asserted
# on its report. Long property, fuzz, and loom runs join here as they
# land.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

BUDGET_DOC="$ROOT_DIR/docs/performance/acceptance-budgets-and-benchmark-harness.md"
DURATION="${VAPOR_PERF_SOAK_DURATION:-12m}"
SEED="${VAPOR_PERF_SOAK_SEED:-11}"

for marker in SLO-1 SLO-2 SLO-3 SLO-4 SLO-5; do
  if ! grep -Fq "$marker" "$BUDGET_DOC"; then
    echo "[perf] missing required budget marker: $marker"
    echo "[perf] update $BUDGET_DOC before running the performance gate"
    exit 1
  fi
done

echo "[perf] soak cell: two-way, mixed load, crash faults, release profile, $DURATION, seed $SEED"
# The wrapper unsets VAPOR_DIR for the driver: the daemon under test
# lives in its own sandbox.
"$ROOT_DIR/scripts/soak.sh" --duration "$DURATION" --seed "$SEED" --release \
  --mode two-way --load mixed --faults crash --throttle walk

report="$ROOT_DIR/.vapor/e2e/soak-last-report.json"
if [[ ! -f "$report" ]]; then
  echo "[perf] soak report missing at $report"
  exit 1
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
