#!/usr/bin/env bash
set -euo pipefail

# Mirrors the source-of-truth files under assets/ into the macOS app's
# SwiftPM resource bundle before every build, test, and package run. The
# mirror is gitignored; edit assets/, never the copies.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOCALES_SOURCE_DIR="$ROOT_DIR/assets/locales"
MENUBAR_SOURCE_DIR="$ROOT_DIR/assets/macos/menubar"
RESOURCES_DIR="$ROOT_DIR/apps/macos/Sources/VaporCore/Resources"
LOCALES_TARGET_DIR="$RESOURCES_DIR/locales"

if [[ ! -d "$LOCALES_SOURCE_DIR" ]]; then
  echo "[swift-resources] Missing source locales directory at $LOCALES_SOURCE_DIR"
  exit 1
fi

if ! compgen -G "$LOCALES_SOURCE_DIR/*.json" >/dev/null; then
  echo "[swift-resources] No locale catalogs found at $LOCALES_SOURCE_DIR"
  exit 1
fi

mkdir -p "$LOCALES_TARGET_DIR"
rm -f "$LOCALES_TARGET_DIR"/*.json
cp "$LOCALES_SOURCE_DIR"/*.json "$LOCALES_TARGET_DIR/"
echo "[swift-resources] Synced locale catalogs to $LOCALES_TARGET_DIR"

# The menu bar template image lands at the bundle root, not in a
# subdirectory: AppKit's image lookup by name (which pairs the 1x and 2x
# files and marks a *Template image as a template) has no subdirectory
# parameter. Only the scales macOS renders are shipped.
menubar_images=(VaporMenuBarTemplate.png VaporMenuBarTemplate@2x.png)
for image in "${menubar_images[@]}"; do
  if [[ ! -f "$MENUBAR_SOURCE_DIR/$image" ]]; then
    echo "[swift-resources] Missing menu bar image at $MENUBAR_SOURCE_DIR/$image"
    exit 1
  fi
done

rm -f "$RESOURCES_DIR"/*.png
for image in "${menubar_images[@]}"; do
  cp "$MENUBAR_SOURCE_DIR/$image" "$RESOURCES_DIR/$image"
done
echo "[swift-resources] Synced menu bar images to $RESOURCES_DIR"
