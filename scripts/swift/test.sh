#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/apps/macos"

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

if [[ ! -d "$SWIFT_DIR" ]]; then
  echo "[swift-test] apps/macos not found. Skipping Swift tests."
  exit 0
fi

"$ROOT_DIR/scripts/swift/resources.sh"

if [[ ! -f "$SWIFT_DIR/Package.swift" ]]; then
  echo "[swift-test] Package.swift not found in apps/macos. Skipping Swift tests."
  exit 0
fi

if [[ "$MODE" == "build" ]]; then
  echo "[swift-test] swift build --build-tests --package-path $SWIFT_DIR"
  swift build --build-tests --package-path "$SWIFT_DIR"
  exit 0
fi

echo "[swift-test] swift test --package-path $SWIFT_DIR"
swift test --package-path "$SWIFT_DIR"
