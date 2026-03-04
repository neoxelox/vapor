#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

export VAPOR_LOG_DIR="${VAPOR_LOG_DIR:-$ROOT_DIR/.vapor/logs}"
export VAPOR_DAEMON_LOG_FILE="${VAPOR_DAEMON_LOG_FILE:-vapord.logs}"
mkdir -p "$VAPOR_LOG_DIR"

collect_manifests() {
  local manifests=""

  if [[ -f "$ROOT_DIR/Cargo.toml" ]]; then
    manifests="$ROOT_DIR/Cargo.toml"
  else
    local candidate
    for candidate in \
      "$ROOT_DIR/daemon/Cargo.toml" \
      "$ROOT_DIR/providers/Cargo.toml" \
      "$ROOT_DIR/shared/Cargo.toml"
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
  cargo test --manifest-path "$manifest" --all-targets --all-features
done <<< "$MANIFESTS"
