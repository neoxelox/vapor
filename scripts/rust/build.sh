#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MODE="${1:-build}"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_LOCAL_DEV="${VAPOR_LOCAL_DEV:-1}"
export VAPOR_LOG_DIR="${VAPOR_LOG_DIR:-$VAPOR_DIR/logs}"
export VAPOR_APP_LOG_FILE="${VAPOR_APP_LOG_FILE:-vapor.logs}"
export VAPOR_DAEMON_LOG_FILE="${VAPOR_DAEMON_LOG_FILE:-vapord.logs}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

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
