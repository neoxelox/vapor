#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

if [[ ! -f "$ROOT_DIR/Cargo.toml" ]]; then
  echo "[cli-test] No Cargo.toml found. Skipping CLI tests."
  exit 0
fi

echo "[cli-test] cargo test -p vapor-cli --all-targets"
cargo test --manifest-path "$ROOT_DIR/Cargo.toml" -p vapor-cli --all-targets
