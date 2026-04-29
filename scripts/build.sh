#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-build}"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
if [[ "$MODE" == "package" ]]; then
  export VAPOR_ENV="${VAPOR_ENV:-prod}"
else
  export VAPOR_ENV="${VAPOR_ENV:-dev}"
fi
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

if [[ "$MODE" != "build" && "$MODE" != "package" ]]; then
  echo "Usage: scripts/build.sh [build|package]"
  exit 1
fi

"$ROOT_DIR/scripts/rust/build.sh" "$MODE"

if [[ "$(uname -s 2>/dev/null || echo unknown)" == "Darwin" ]]; then
  "$ROOT_DIR/scripts/swift/build.sh" "$MODE"
else
  echo "[build] Host is not macOS. Skipping Swift build (apps/macos is macOS-only)."
fi
