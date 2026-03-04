#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if [[ -f "$ROOT_DIR/Cargo.toml" ]]; then
  echo "[rust-build] cargo build --workspace --release"
  cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --workspace --release
  exit 0
fi

echo "[rust-build] No Cargo.toml found. Skipping Rust build."
