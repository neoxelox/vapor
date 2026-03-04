#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/apps/macos"

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
