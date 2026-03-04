#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/apps/macos"
MODE="${1:-check}"

if [[ "$MODE" != "check" && "$MODE" != "apply" ]]; then
  echo "Usage: scripts/swift/format.sh [check|apply]"
  exit 1
fi

if [[ ! -d "$SWIFT_DIR" ]]; then
  echo "[swift-format] apps/macos not found. Skipping Swift format."
  exit 0
fi

if ! find "$SWIFT_DIR" -type f -name "*.swift" -print -quit | grep -q .; then
  echo "[swift-format] No Swift files found in apps/macos. Skipping Swift format."
  exit 0
fi

if ! swift format --help >/dev/null 2>&1; then
  echo "[swift-format] 'swift format' is required but not available."
  exit 1
fi

if [[ "$MODE" == "check" ]]; then
  echo "[swift-format] swift format lint --recursive $SWIFT_DIR"
  swift format lint --recursive "$SWIFT_DIR"
else
  echo "[swift-format] swift format format --in-place --recursive $SWIFT_DIR"
  swift format format --in-place --recursive "$SWIFT_DIR"
fi
