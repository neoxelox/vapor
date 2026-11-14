#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MODE="${1:-apply}"

if [[ "$MODE" != "check" && "$MODE" != "apply" ]]; then
  echo "Usage: scripts/rust/format.sh [check|apply]"
  exit 1
fi

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

"$ROOT_DIR/scripts/version.sh" check-sync >/dev/null

MANIFEST="$ROOT_DIR/Cargo.toml"

if [[ "$MODE" == "check" ]]; then
  echo "[rust-format] cargo fmt --check for ${MANIFEST}"
  cargo fmt --manifest-path "$MANIFEST" --all -- --check
else
  echo "[rust-format] cargo fmt for ${MANIFEST}"
  cargo fmt --manifest-path "$MANIFEST" --all
fi
