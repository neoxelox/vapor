#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

started_at="$(date +%s)"

"$ROOT_DIR/scripts/rust/test.sh"

if [[ "$(uname -s 2>/dev/null || echo unknown)" == "Darwin" ]]; then
  "$ROOT_DIR/scripts/swift/test.sh"
else
  echo "[test] Host is not macOS. Skipping Swift tests (apps/macos is macOS-only)."
fi

bash "$ROOT_DIR/scripts/tests/version.sh"

# Tier 1 budget (AGENTS.md §9.1): CI sets VAPOR_TEST_MAX_SECONDS so a suite
# that outgrows its budget fails loudly instead of eroding the feedback
# loop; locally the variable stays unset so a slow machine never fails.
elapsed=$(( $(date +%s) - started_at ))
echo "[test] Tier 1 finished in ${elapsed}s"
if [[ -n "${VAPOR_TEST_MAX_SECONDS:-}" && "$elapsed" -gt "$VAPOR_TEST_MAX_SECONDS" ]]; then
  echo "[test] Tier 1 took ${elapsed}s, over the ${VAPOR_TEST_MAX_SECONDS}s budget." >&2
  echo "[test] Split slow tests into Tier 2 (scripts/perf.sh); see docs/architecture/testing-strategy.md §Discipline rules." >&2
  exit 1
fi
