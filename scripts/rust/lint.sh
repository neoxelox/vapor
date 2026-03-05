#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

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
  echo "[rust-lint] No Cargo.toml found. Skipping Rust lint."
  exit 0
fi

while IFS= read -r manifest; do
  [[ -z "$manifest" ]] && continue
  echo "[rust-lint] cargo clippy for ${manifest}"
  cargo clippy --manifest-path "$manifest" --all-targets --all-features -- -D warnings
done <<< "$MANIFESTS"
