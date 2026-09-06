#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

"$ROOT_DIR/scripts/version.sh" check-sync >/dev/null

MANIFEST="$ROOT_DIR/Cargo.toml"

echo "[rust-lint] cargo clippy for ${MANIFEST}"
cargo clippy --manifest-path "$MANIFEST" --all-targets --all-features -- -D warnings
