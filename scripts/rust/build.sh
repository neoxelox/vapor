#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MODE="${1:-build}"

if [[ "$MODE" != "build" && "$MODE" != "package" ]]; then
  echo "Usage: scripts/rust/build.sh [build|package]"
  exit 1
fi

if [[ -f "$ROOT_DIR/Cargo.toml" ]]; then
  if [[ "$MODE" == "package" ]]; then
    echo "[rust-build] cargo build --workspace --release (packaged log level defaults to warning)"
    VAPOR_DEFAULT_LOG_LEVEL=warning cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --workspace --release
  else
    echo "[rust-build] cargo build --workspace --release"
    cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --workspace --release
  fi

  exit 0
fi

echo "[rust-build] No Cargo.toml found. Skipping Rust build."
