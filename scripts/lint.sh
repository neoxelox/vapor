#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

"$ROOT_DIR/scripts/rust/lint.sh"

if [[ "$(uname -s 2>/dev/null || echo unknown)" == "Darwin" ]]; then
  "$ROOT_DIR/scripts/swift/lint.sh"
else
  echo "[lint] Host is not macOS. Skipping Swift lint (apps/macos is macOS-only)."
fi

# Only the Rust format check here: the Swift lint above already ran the
# same `swift format lint`, and calling the aggregate `format.sh check`
# would run it a second time for no added coverage.
"$ROOT_DIR/scripts/rust/format.sh" check
