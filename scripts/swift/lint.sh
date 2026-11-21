#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/apps/macos"
# The Tier VM driver is the other Swift code of the repository.
DRIVER_DIR="$ROOT_DIR/tools/vm/driver"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

if [[ ! -d "$SWIFT_DIR" ]]; then
  echo "[swift-lint] apps/macos not found. Skipping Swift lint."
  exit 0
fi

if ! find "$SWIFT_DIR" -type f -name "*.swift" -print -quit | grep -q .; then
  echo "[swift-lint] No Swift files found in apps/macos. Skipping Swift lint."
  exit 0
fi

if ! swift format --help >/dev/null 2>&1; then
  echo "[swift-lint] 'swift format' is required but not available."
  exit 1
fi

# --strict: without it the command prints each finding as a warning and
# exits 0, so the step could not fail.
for dir in "$SWIFT_DIR" "$DRIVER_DIR"; do
  [[ -d "$dir" ]] || continue
  echo "[swift-lint] swift format lint --strict --recursive $dir"
  swift format lint --strict --recursive "$dir"
done
