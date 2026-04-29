#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

"$ROOT_DIR/scripts/rust/test.sh"

if [[ "$(uname -s 2>/dev/null || echo unknown)" == "Darwin" ]]; then
  "$ROOT_DIR/scripts/swift/test.sh"
else
  echo "[test] Host is not macOS. Skipping Swift tests (apps/macos is macOS-only)."
fi

bash "$ROOT_DIR/scripts/tests/version.sh"
