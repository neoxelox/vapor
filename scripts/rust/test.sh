#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

# usage: test.sh [build|run]
#   build  compile the tests and run none
#   run    run the tests (compiles what is missing)
#   (none) both, in one step
MODE="${1:-all}"
case "$MODE" in
  build | run | all) ;;
  *)
    echo "usage: $0 [build|run]" >&2
    exit 2
    ;;
esac

"$ROOT_DIR/scripts/version.sh" check-sync >/dev/null

MANIFEST="$ROOT_DIR/Cargo.toml"

if [[ "$MODE" == "build" ]]; then
  echo "[rust-test] cargo test --no-run for ${MANIFEST}"
  cargo test --manifest-path "$MANIFEST" --all-targets --all-features --no-run
  exit 0
fi

echo "[rust-test] cargo test for ${MANIFEST}"
# --no-fail-fast: keep running the remaining crates after one fails, so a
# single red crate cannot hide failures in the crates tested after it.
cargo test --manifest-path "$MANIFEST" --all-targets --all-features --no-fail-fast
