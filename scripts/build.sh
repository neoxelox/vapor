#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-build}"

if [[ "$MODE" != "build" && "$MODE" != "package" ]]; then
  echo "Usage: scripts/build.sh [build|package]"
  exit 1
fi

"$ROOT_DIR/scripts/rust/build.sh" "$MODE"
"$ROOT_DIR/scripts/swift/build.sh" "$MODE"
