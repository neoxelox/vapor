#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-check}"

if [[ "$MODE" != "check" && "$MODE" != "apply" ]]; then
  echo "Usage: scripts/format.sh [check|apply]"
  exit 1
fi

"$ROOT_DIR/scripts/rust/format.sh" "$MODE"
"$ROOT_DIR/scripts/swift/format.sh" "$MODE"
