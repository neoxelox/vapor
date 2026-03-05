#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MODE="${1:-check}"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

if [[ "$MODE" != "check" && "$MODE" != "apply" ]]; then
  echo "Usage: scripts/rust/format.sh [check|apply]"
  exit 1
fi

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
  echo "[rust-format] No Cargo.toml found. Skipping Rust format."
  exit 0
fi

while IFS= read -r manifest; do
  [[ -z "$manifest" ]] && continue
  if [[ "$MODE" == "check" ]]; then
    echo "[rust-format] cargo fmt --check for ${manifest}"
    cargo fmt --manifest-path "$manifest" --all -- --check
  else
    echo "[rust-format] cargo fmt for ${manifest}"
    cargo fmt --manifest-path "$manifest" --all
  fi
done <<< "$MANIFESTS"
