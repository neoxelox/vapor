#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MODE="${1:-build}"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
if [[ "$MODE" == "package" ]]; then
  export VAPOR_ENV="${VAPOR_ENV:-prod}"
else
  export VAPOR_ENV="${VAPOR_ENV:-dev}"
fi
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

if [[ "$MODE" != "build" && "$MODE" != "package" ]]; then
  echo "Usage: scripts/cli/build.sh [build|package]"
  exit 1
fi

if [[ ! -f "$ROOT_DIR/Cargo.toml" ]]; then
  echo "[cli-build] No Cargo.toml found. Skipping CLI build."
  exit 0
fi

echo "[cli-build] cargo build --release --bin vapor"
cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --bin vapor --release
