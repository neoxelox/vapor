#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/apps/macos"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

if [[ ! -d "$SWIFT_DIR" ]]; then
  echo "[swift-test] apps/macos not found. Skipping Swift tests."
  exit 0
fi

"$ROOT_DIR/scripts/swift/resources.sh"

if [[ ! -f "$SWIFT_DIR/Package.swift" ]]; then
  echo "[swift-test] Package.swift not found in apps/macos. Skipping Swift tests."
  exit 0
fi

echo "[swift-test] swift test --package-path $SWIFT_DIR"
swift test --package-path "$SWIFT_DIR"
