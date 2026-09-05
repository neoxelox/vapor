#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

"$ROOT_DIR/scripts/version.sh" check-sync >/dev/null

collect_manifests() {
  local manifests=""

  if [[ -f "$ROOT_DIR/Cargo.toml" ]]; then
    manifests="$ROOT_DIR/Cargo.toml"
  else
    local candidate
    for candidate in \
      "$ROOT_DIR/core/daemon/Cargo.toml" \
      "$ROOT_DIR/core/providers/Cargo.toml" \
      "$ROOT_DIR/core/shared/Cargo.toml"
    do
      if [[ -f "$candidate" ]]; then
        if [[ -n "$manifests" ]]; then
          manifests+=$'\n'
        fi
        manifests+="$candidate"
      fi
    done
  fi

  printf '%s\n' "$manifests"
}

MANIFESTS="$(collect_manifests)"

if [[ -z "$MANIFESTS" ]]; then
  echo "[rust-test] No Cargo.toml found. Skipping Rust tests."
  exit 0
fi

while IFS= read -r manifest; do
  [[ -z "$manifest" ]] && continue
  echo "[rust-test] cargo test for ${manifest}"
  # --no-fail-fast: keep running the remaining crates after one fails, so a
  # single red crate cannot hide failures in the crates tested after it.
  cargo test --manifest-path "$manifest" --all-targets --all-features --no-fail-fast
done <<< "$MANIFESTS"
