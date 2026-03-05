#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/apps/macos"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_LOCAL_DEV="${VAPOR_LOCAL_DEV:-1}"
export VAPOR_LOG_DIR="${VAPOR_LOG_DIR:-$VAPOR_DIR/logs}"
export VAPOR_APP_LOG_FILE="${VAPOR_APP_LOG_FILE:-vapor.logs}"
export VAPOR_DAEMON_LOG_FILE="${VAPOR_DAEMON_LOG_FILE:-vapord.logs}"
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

echo "[swift-lint] swift format lint --recursive $SWIFT_DIR"
swift format lint --recursive "$SWIFT_DIR"
