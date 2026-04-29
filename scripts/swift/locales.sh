#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SOURCE_DIR="$ROOT_DIR/assets/locales"
TARGET_DIR="$ROOT_DIR/apps/macos/Sources/VaporCore/Resources/locales"

if [[ ! -d "$SOURCE_DIR" ]]; then
  echo "[swift-locales] Missing source locales directory at $SOURCE_DIR"
  exit 1
fi

if ! compgen -G "$SOURCE_DIR/*.json" >/dev/null; then
  echo "[swift-locales] No locale catalogs found at $SOURCE_DIR"
  exit 1
fi

mkdir -p "$TARGET_DIR"
rm -f "$TARGET_DIR"/*.json
cp "$SOURCE_DIR"/*.json "$TARGET_DIR/"

echo "[swift-locales] Synced locale catalogs to $TARGET_DIR"
