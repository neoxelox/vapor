#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
APP_NAME="${APP_NAME:-Vapor}"
EXECUTABLE_NAME="${EXECUTABLE_NAME:-Vapor}"
BUNDLE_ID="${BUNDLE_ID:-sh.arn.vapor}"
MIN_MACOS="${MIN_MACOS:-26.0}"
ICON_PNG="${ICON_PNG:-assets/icon.png}"
DIST_DIR="${DIST_DIR:-dist}"
VAPOR_SIGN_IDENTITY="${VAPOR_SIGN_IDENTITY:-}"
VAPOR_ENTITLEMENTS="${VAPOR_ENTITLEMENTS:-}"
VAPOR_NOTARY_PROFILE="${VAPOR_NOTARY_PROFILE:-}"
VAPOR_NOTARY_KEYCHAIN="${VAPOR_NOTARY_KEYCHAIN:-}"
export VAPOR_ENV="${VAPOR_ENV:-prod}"

"$ROOT_DIR/scripts/version.sh" check-sync >/dev/null
eval "$("$ROOT_DIR/scripts/version.sh" metadata)"

"$ROOT_DIR/scripts/swift/sync-locales.sh"

if [[ "$ICON_PNG" != /* ]]; then
  ICON_PNG="$ROOT_DIR/$ICON_PNG"
fi

if [[ "$DIST_DIR" != /* ]]; then
  DIST_DIR="$ROOT_DIR/$DIST_DIR"
fi

if [[ -n "$VAPOR_ENTITLEMENTS" && "$VAPOR_ENTITLEMENTS" != /* ]]; then
  VAPOR_ENTITLEMENTS="$ROOT_DIR/$VAPOR_ENTITLEMENTS"
fi

if [[ ! -f "$ICON_PNG" ]]; then
  echo "[package] Missing icon PNG at $ICON_PNG"
  echo "[package] Expected source-of-truth icon at assets/icon.png"
  exit 1
fi

if ! icon_width="$(sips -g pixelWidth "$ICON_PNG" 2>/dev/null | awk '/pixelWidth/ { print $2 }')"; then
  echo "[package] Failed to inspect icon width at $ICON_PNG"
  exit 1
fi

if ! icon_height="$(sips -g pixelHeight "$ICON_PNG" 2>/dev/null | awk '/pixelHeight/ { print $2 }')"; then
  echo "[package] Failed to inspect icon height at $ICON_PNG"
  exit 1
fi

if [[ "$icon_width" != "1024" || "$icon_height" != "1024" ]]; then
  echo "[package] Icon must be 1024x1024 but is ${icon_width}x${icon_height}"
  exit 1
fi

short_version="$VAPOR_RELEASE_VERSION"
build_version="$VAPOR_APPLE_BUILD_VERSION"

echo "[package] Building release executable"
echo "[package] Version: $VAPOR_VERSION"
echo "[package] Build version: $build_version"
echo "[package] Git commit: $VAPOR_GIT_COMMIT_SHORT"
swift build \
  --package-path "$ROOT_DIR/apps/macos" \
  -c release \
  --disable-index-store \
  -Xswiftc -whole-module-optimization \
  -Xswiftc -cross-module-optimization

echo "[package] Building release daemon executable"
cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --package vapor-daemon --bin vapord --release

binary_path="$ROOT_DIR/apps/macos/.build/release/$EXECUTABLE_NAME"
if [[ ! -f "$binary_path" ]]; then
  echo "[package] Expected release binary missing at $binary_path"
  exit 1
fi

daemon_binary_path="$ROOT_DIR/target/release/vapord"
if [[ ! -f "$daemon_binary_path" ]]; then
  echo "[package] Expected daemon binary missing at $daemon_binary_path"
  exit 1
fi

app_bundle="$DIST_DIR/$APP_NAME.app"
contents_dir="$app_bundle/Contents"
macos_dir="$contents_dir/MacOS"
resources_dir="$contents_dir/Resources"
info_plist="$contents_dir/Info.plist"

rm -rf "$app_bundle"
mkdir -p "$macos_dir" "$resources_dir"

cp "$binary_path" "$macos_dir/$EXECUTABLE_NAME"
chmod +x "$macos_dir/$EXECUTABLE_NAME"

cp "$daemon_binary_path" "$macos_dir/vapord"
chmod +x "$macos_dir/vapord"

cat >"$info_plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>$APP_NAME</string>
  <key>CFBundleDisplayName</key>
  <string>$APP_NAME</string>
  <key>CFBundleIdentifier</key>
  <string>$BUNDLE_ID</string>
  <key>CFBundleExecutable</key>
  <string>$EXECUTABLE_NAME</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$short_version</string>
  <key>CFBundleVersion</key>
  <string>$build_version</string>
  <key>VaporVersion</key>
  <string>$VAPOR_VERSION</string>
  <key>VaporGitCommit</key>
  <string>$VAPOR_GIT_COMMIT_SHORT</string>
  <key>LSMinimumSystemVersion</key>
  <string>$MIN_MACOS</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
EOF

temp_dir="$(mktemp -d)"
iconset_dir="$temp_dir/AppIcon.iconset"
mkdir -p "$iconset_dir"

resize_icon() {
  local size="$1"
  local output_file="$2"
  sips -z "$size" "$size" "$ICON_PNG" --out "$iconset_dir/$output_file" >/dev/null
}

resize_icon 16 "icon_16x16.png"
resize_icon 32 "icon_16x16@2x.png"
resize_icon 32 "icon_32x32.png"
resize_icon 64 "icon_32x32@2x.png"
resize_icon 128 "icon_128x128.png"
resize_icon 256 "icon_128x128@2x.png"
resize_icon 256 "icon_256x256.png"
resize_icon 512 "icon_256x256@2x.png"
resize_icon 512 "icon_512x512.png"
resize_icon 1024 "icon_512x512@2x.png"

iconutil -c icns "$iconset_dir" -o "$resources_dir/AppIcon.icns"
rm -rf "$temp_dir"

if [[ -d "$ROOT_DIR/apps/macos/Resources" ]]; then
  ditto "$ROOT_DIR/apps/macos/Resources" "$resources_dir"
fi

codesign_args=(--force --deep)
if [[ -n "$VAPOR_SIGN_IDENTITY" ]]; then
  codesign_args+=(--options runtime --timestamp --sign "$VAPOR_SIGN_IDENTITY")
else
  codesign_args+=(--sign -)
fi

if [[ -n "$VAPOR_ENTITLEMENTS" ]]; then
  if [[ ! -f "$VAPOR_ENTITLEMENTS" ]]; then
    echo "[package] Entitlements file not found: $VAPOR_ENTITLEMENTS"
    exit 1
  fi
  codesign_args+=(--entitlements "$VAPOR_ENTITLEMENTS")
fi

codesign "${codesign_args[@]}" "$app_bundle"

plutil -lint "$info_plist"
codesign --verify --deep --verbose=4 "$app_bundle"

if ! spctl -a -vv "$app_bundle"; then
  echo "[package] Warning: spctl assessment failed for $app_bundle"
fi

mkdir -p "$DIST_DIR"
zip_path="$DIST_DIR/$APP_NAME.zip"
rm -f "$zip_path"
ditto -c -k --keepParent "$app_bundle" "$zip_path"

if [[ -n "$VAPOR_NOTARY_PROFILE" ]]; then
  if [[ -z "$VAPOR_SIGN_IDENTITY" ]]; then
    echo "[package] Notarization requires VAPOR_SIGN_IDENTITY with Developer ID Application certificate"
    exit 1
  fi

  notarytool_args=(submit "$zip_path" --keychain-profile "$VAPOR_NOTARY_PROFILE" --wait)
  if [[ -n "$VAPOR_NOTARY_KEYCHAIN" ]]; then
    notarytool_args+=(--keychain "$VAPOR_NOTARY_KEYCHAIN")
  fi

  xcrun notarytool "${notarytool_args[@]}"
  xcrun stapler staple "$app_bundle"

  rm -f "$zip_path"
  ditto -c -k --keepParent "$app_bundle" "$zip_path"

  echo "[package] Notarized zip artifact: $zip_path"
fi

echo "[package] App bundle: $app_bundle"
echo "[package] Zip artifact: $zip_path"
