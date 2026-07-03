#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

paths=(
  "$ROOT_DIR/target"
  "$ROOT_DIR/debug"
  "$ROOT_DIR/dist"
  "$ROOT_DIR/.vapor"
  "$ROOT_DIR/apps/macos/.build"
  "$ROOT_DIR/apps/macos/.swiftpm"
  "$ROOT_DIR/apps/macos/dist"
)

for path in "${paths[@]}"; do
  if [[ -e "$path" ]]; then
    echo "[clean] removing $path"
    rm -rf "$path"
  else
    echo "[clean] skip missing $path"
  fi
done

echo "[clean] done"
