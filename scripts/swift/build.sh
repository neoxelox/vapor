#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/apps/macos"
MODE="${1:-build}"

if [[ "$MODE" != "build" && "$MODE" != "package" ]]; then
  echo "Usage: scripts/swift/build.sh [build|package]"
  exit 1
fi

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
if [[ "$MODE" == "package" ]]; then
  export VAPOR_ENV="${VAPOR_ENV:-prod}"
else
  export VAPOR_ENV="${VAPOR_ENV:-dev}"
fi
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

if [[ ! -d "$SWIFT_DIR" ]]; then
  echo "[swift-build] apps/macos not found. Skipping Swift build."
  exit 0
fi

"$ROOT_DIR/scripts/swift/resources.sh"

if [[ "$MODE" == "package" ]]; then
  PACKAGE_SCRIPT="$SWIFT_DIR/scripts/package.sh"
  echo "[swift-build] package mode; running $PACKAGE_SCRIPT"
  bash "$PACKAGE_SCRIPT"
  exit 0
fi

echo "[swift-build] swift build --package-path $SWIFT_DIR -c release"
swift build \
  --package-path "$SWIFT_DIR" \
  -c release \
  --disable-index-store \
  -Xswiftc -whole-module-optimization \
  -Xswiftc -cross-module-optimization
