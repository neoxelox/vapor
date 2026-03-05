#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/apps/macos"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_LOCAL_DEV="${VAPOR_LOCAL_DEV:-1}"
export VAPOR_LOG_DIR="${VAPOR_LOG_DIR:-$VAPOR_DIR/logs}"
export VAPOR_APP_LOG_FILE="${VAPOR_APP_LOG_FILE:-vapor.logs}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

if [[ ! -d "$SWIFT_DIR" ]]; then
  echo "[swift-test] apps/macos not found. Skipping Swift tests."
  exit 0
fi

if [[ ! -f "$SWIFT_DIR/Package.swift" ]]; then
  echo "[swift-test] Package.swift not found in apps/macos. Skipping Swift tests."
  exit 0
fi

echo "[swift-test] swift test --package-path $SWIFT_DIR"
swift test --package-path "$SWIFT_DIR"
