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
  echo "Usage: scripts/rust/build.sh [build|package]"
  exit 1
fi

if [[ -f "$ROOT_DIR/Cargo.toml" ]]; then
  echo "[rust-build] cargo build --workspace --release"
  cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --workspace --release

  exit 0
fi

echo "[rust-build] No Cargo.toml found. Skipping Rust build."
