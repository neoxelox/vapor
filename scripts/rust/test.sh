#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

"$ROOT_DIR/scripts/version.sh" check-sync >/dev/null

MANIFEST="$ROOT_DIR/Cargo.toml"

echo "[rust-test] cargo test for ${MANIFEST}"
# --no-fail-fast: keep running the remaining crates after one fails, so a
# single red crate cannot hide failures in the crates tested after it.
cargo test --manifest-path "$MANIFEST" --all-targets --all-features --no-fail-fast
