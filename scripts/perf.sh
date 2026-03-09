#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

BUDGET_DOC="$ROOT_DIR/docs/performance/acceptance-budgets-and-benchmark-harness.md"
RUST_MAX_SECONDS="${VAPOR_PERF_SMOKE_RUST_MAX_SECONDS:-600}"
SWIFT_MAX_SECONDS="${VAPOR_PERF_SMOKE_SWIFT_MAX_SECONDS:-900}"

require_doc_marker() {
  local marker="$1"
  if ! grep -Fq "$marker" "$BUDGET_DOC"; then
    echo "[perf] missing required budget marker: $marker"
    echo "[perf] update $BUDGET_DOC before running the performance gate"
    exit 1
  fi
}

run_with_budget() {
  local label="$1"
  local budget_seconds="$2"
  shift 2

  if [[ ! "$budget_seconds" =~ ^[0-9]+$ ]]; then
    echo "[perf] invalid numeric budget for $label: $budget_seconds"
    exit 1
  fi

  local start_seconds
  start_seconds="$(date +%s)"
  "$@"
  local end_seconds
  end_seconds="$(date +%s)"
  local elapsed_seconds
  elapsed_seconds="$((end_seconds - start_seconds))"

  echo "[perf] ${label}: elapsed=${elapsed_seconds}s budget=${budget_seconds}s"

  if (( elapsed_seconds > budget_seconds )); then
    echo "[perf] ${label} exceeded budget by $((elapsed_seconds - budget_seconds))s"
    exit 1
  fi
}

echo "[perf] validating performance SLO markers in $BUDGET_DOC"
for marker in SLO-1 SLO-2 SLO-3 SLO-4 SLO-5 CI-SMOKE-1 CI-SMOKE-2; do
  require_doc_marker "$marker"
done

echo "[perf] running Rust smoke test budget"
run_with_budget "rust-tests" "$RUST_MAX_SECONDS" "$ROOT_DIR/scripts/rust/test.sh"

echo "[perf] running Swift smoke test budget"
run_with_budget "swift-tests" "$SWIFT_MAX_SECONDS" "$ROOT_DIR/scripts/swift/test.sh"

echo "[perf] all smoke thresholds passed"
