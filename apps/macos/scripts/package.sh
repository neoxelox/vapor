#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
APP_NAME="${APP_NAME:-Vapor}"
EXECUTABLE_NAME="${EXECUTABLE_NAME:-Vapor}"
BUNDLE_ID="${BUNDLE_ID:-sh.arn.vapor}"
MIN_MACOS="${MIN_MACOS:-26.0}"
ICON_DOCUMENT="${ICON_DOCUMENT:-assets/macos/Vapor.icon}"
ICONSET_DIR="${ICONSET_DIR:-assets/macos/Vapor.iconset}"
ASSET_CATALOG="${ASSET_CATALOG:-assets/macos/Vapor.xcassets}"
ACCENT_COLOR_NAME="${ACCENT_COLOR_NAME:-AccentColor}"
DIST_DIR="${DIST_DIR:-dist}"
VAPOR_SIGN_IDENTITY="${VAPOR_SIGN_IDENTITY:-}"
VAPOR_ENTITLEMENTS="${VAPOR_ENTITLEMENTS:-}"
VAPOR_NOTARY_PROFILE="${VAPOR_NOTARY_PROFILE:-}"
VAPOR_NOTARY_KEYCHAIN="${VAPOR_NOTARY_KEYCHAIN:-}"
export VAPOR_ENV="${VAPOR_ENV:-prod}"

"$ROOT_DIR/scripts/version.sh" check-sync >/dev/null
eval "$("$ROOT_DIR/scripts/version.sh" metadata)"

"$ROOT_DIR/scripts/swift/resources.sh"

if [[ "$ICON_DOCUMENT" != /* ]]; then
  ICON_DOCUMENT="$ROOT_DIR/$ICON_DOCUMENT"
fi

if [[ "$ICONSET_DIR" != /* ]]; then
  ICONSET_DIR="$ROOT_DIR/$ICONSET_DIR"
fi

if [[ "$ASSET_CATALOG" != /* ]]; then
  ASSET_CATALOG="$ROOT_DIR/$ASSET_CATALOG"
fi

if [[ "$DIST_DIR" != /* ]]; then
  DIST_DIR="$ROOT_DIR/$DIST_DIR"
fi

if [[ -n "$VAPOR_ENTITLEMENTS" && "$VAPOR_ENTITLEMENTS" != /* ]]; then
  VAPOR_ENTITLEMENTS="$ROOT_DIR/$VAPOR_ENTITLEMENTS"
fi

assert_bundle_executable() {
  local executable_path="$1"
  local description="$2"

  if [[ ! -f "$executable_path" ]]; then
    echo "[package] Missing bundled $description at $executable_path"
    exit 1
  fi

  if [[ ! -x "$executable_path" ]]; then
    echo "[package] Bundled $description is not executable at $executable_path"
    exit 1
  fi
}

# The app icon has two sources (see assets/README.md). The Icon Composer
# document is what macOS 26 renders: actool compiles its layers into the
# bundle's asset catalog, and the system draws the glass, the dark and the
# tinted variants from them. The iconset is the flat fallback, packed into
# an .icns unchanged for anything that still reads CFBundleIconFile. Both
# share one name so Info.plist points at both.
icon_name="$(basename "$ICON_DOCUMENT" .icon)"

if [[ ! -f "$ICON_DOCUMENT/icon.json" ]]; then
  echo "[package] Missing app icon document at $ICON_DOCUMENT (expected icon.json inside)"
  exit 1
fi

# The accent colour (the brand orange, assets/README.md) ships as a
# colour set in the same asset catalog compile; NSAccentColorName in the
# Info.plist points controls at it whenever the user's system accent is
# the default multicolour.
if [[ ! -f "$ASSET_CATALOG/$ACCENT_COLOR_NAME.colorset/Contents.json" ]]; then
  echo "[package] Missing accent colour set at $ASSET_CATALOG/$ACCENT_COLOR_NAME.colorset"
  exit 1
fi

iconset_slots=(
  "icon_16x16.png:16"
  "icon_16x16@2x.png:32"
  "icon_32x32.png:32"
  "icon_32x32@2x.png:64"
  "icon_128x128.png:128"
  "icon_128x128@2x.png:256"
  "icon_256x256.png:256"
  "icon_256x256@2x.png:512"
  "icon_512x512.png:512"
  "icon_512x512@2x.png:1024"
)

if [[ ! -d "$ICONSET_DIR" ]]; then
  echo "[package] Missing app iconset at $ICONSET_DIR"
  exit 1
fi

for slot in "${iconset_slots[@]}"; do
  slot_file="${slot%%:*}"
  slot_pixels="${slot##*:}"
  slot_path="$ICONSET_DIR/$slot_file"

  if [[ ! -f "$slot_path" ]]; then
    echo "[package] Missing iconset slot $slot_file in $ICONSET_DIR"
    exit 1
  fi

  if ! slot_width="$(sips -g pixelWidth "$slot_path" 2>/dev/null | awk '/pixelWidth/ { print $2 }')"; then
    echo "[package] Failed to inspect $slot_path"
    exit 1
  fi
  slot_height="$(sips -g pixelHeight "$slot_path" 2>/dev/null | awk '/pixelHeight/ { print $2 }')"

  if [[ "$slot_width" != "$slot_pixels" || "$slot_height" != "$slot_pixels" ]]; then
    echo "[package] Iconset slot $slot_file must be ${slot_pixels}x${slot_pixels} but is ${slot_width}x${slot_height}"
    exit 1
  fi
done

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

echo "[package] Building release daemon + CLI executables"
cargo build --manifest-path "$ROOT_DIR/Cargo.toml" \
  --package vapor-daemon --bin vapord \
  --package vapor-cli --bin vapor \
  --release

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

cli_binary_path="$ROOT_DIR/target/release/vapor"
if [[ ! -f "$cli_binary_path" ]]; then
  echo "[package] Expected CLI binary missing at $cli_binary_path"
  exit 1
fi

resource_bundle_path="$ROOT_DIR/apps/macos/.build/release/Vapor_VaporCore.bundle"
if [[ ! -d "$resource_bundle_path" ]]; then
  echo "[package] Expected SwiftPM resource bundle missing at $resource_bundle_path"
  exit 1
fi

app_bundle="$DIST_DIR/$APP_NAME.app"
contents_dir="$app_bundle/Contents"
macos_dir="$contents_dir/MacOS"
# The `vapor` CLI cannot live in Contents/MacOS: the default macOS
# filesystem is case-insensitive, so `vapor` would collide with the
# `Vapor` app executable. Helpers/ is the conventional home for bundled
# helper tools; the CLI resolves vapord from ../MacOS/vapord.
helpers_dir="$contents_dir/Helpers"
resources_dir="$contents_dir/Resources"
info_plist="$contents_dir/Info.plist"

rm -rf "$app_bundle"
mkdir -p "$macos_dir" "$helpers_dir" "$resources_dir"

cp "$binary_path" "$macos_dir/$EXECUTABLE_NAME"
chmod +x "$macos_dir/$EXECUTABLE_NAME"

cp "$daemon_binary_path" "$macos_dir/vapord"
chmod +x "$macos_dir/vapord"

cp "$cli_binary_path" "$helpers_dir/vapor"
chmod +x "$helpers_dir/vapor"

assert_bundle_executable "$macos_dir/$EXECUTABLE_NAME" "app executable"
assert_bundle_executable "$macos_dir/vapord" "daemon executable"
assert_bundle_executable "$helpers_dir/vapor" "CLI executable"

ditto "$resource_bundle_path" "$resources_dir/$(basename "$resource_bundle_path")"

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
  <string>$icon_name</string>
  <key>CFBundleIconName</key>
  <string>$icon_name</string>
  <key>NSAccentColorName</key>
  <string>$ACCENT_COLOR_NAME</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>LSUIElement</key>
  <true/>
</dict>
</plist>
EOF
# LSUIElement=true makes launch menubar-first declaratively (no Dock-icon
# flash before the runtime activation-policy flip); opening the main window
# switches the activation policy to .regular at runtime.

icon_compile_dir="$(mktemp -d)"
actool_log="$icon_compile_dir/actool.log"
if ! xcrun actool "$ICON_DOCUMENT" "$ASSET_CATALOG" \
  --compile "$icon_compile_dir" \
  --platform macosx \
  --minimum-deployment-target "$MIN_MACOS" \
  --app-icon "$icon_name" \
  --accent-color "$ACCENT_COLOR_NAME" \
  --output-partial-info-plist "$icon_compile_dir/icon.plist" \
  --output-format human-readable-text >"$actool_log" 2>&1 \
  || grep -q "error:" "$actool_log" \
  || [[ ! -f "$icon_compile_dir/Assets.car" ]]; then
  cat "$actool_log"
  echo "[package] Failed to compile the app icon document at $ICON_DOCUMENT and the asset catalog at $ASSET_CATALOG"
  exit 1
fi

compiled_icon_name="$(/usr/libexec/PlistBuddy -c "Print :CFBundleIconName" "$icon_compile_dir/icon.plist")"
if [[ "$compiled_icon_name" != "$icon_name" ]]; then
  echo "[package] actool registered the app icon as '$compiled_icon_name', expected '$icon_name'"
  exit 1
fi

compiled_accent_name="$(/usr/libexec/PlistBuddy -c "Print :NSAccentColorName" "$icon_compile_dir/icon.plist" 2>/dev/null || true)"
if [[ "$compiled_accent_name" != "$ACCENT_COLOR_NAME" ]]; then
  echo "[package] actool registered the accent colour as '$compiled_accent_name', expected '$ACCENT_COLOR_NAME'"
  exit 1
fi

cp "$icon_compile_dir/Assets.car" "$resources_dir/Assets.car"
rm -rf "$icon_compile_dir"

iconutil -c icns "$ICONSET_DIR" -o "$resources_dir/$icon_name.icns"

if [[ -d "$ROOT_DIR/apps/macos/Resources" ]]; then
  ditto "$ROOT_DIR/apps/macos/Resources" "$resources_dir"
fi

# Inside-out signing: the nested daemon binary is signed first with its
# own invocation, then the app bundle (whose seal covers the already-signed
# vapord). Apple documents `--deep` as unsuitable for production signing —
# it forces one entitlements file onto every nested binary, and the app vs
# daemon entitlement sets will diverge.
sign_args_base=(--force)
if [[ -n "$VAPOR_SIGN_IDENTITY" ]]; then
  sign_args_base+=(--options runtime --timestamp --sign "$VAPOR_SIGN_IDENTITY")
else
  sign_args_base+=(--sign -)
fi

if [[ -n "$VAPOR_ENTITLEMENTS" && ! -f "$VAPOR_ENTITLEMENTS" ]]; then
  echo "[package] Entitlements file not found: $VAPOR_ENTITLEMENTS"
  exit 1
fi

# 1. The bundled daemon + CLI (no app entitlements; hardened runtime only).
codesign "${sign_args_base[@]}" "$macos_dir/vapord"
codesign "${sign_args_base[@]}" "$helpers_dir/vapor"

# 2. The app bundle, with the app entitlements when provided.
app_sign_args=("${sign_args_base[@]}")
if [[ -n "$VAPOR_ENTITLEMENTS" ]]; then
  app_sign_args+=(--entitlements "$VAPOR_ENTITLEMENTS")
fi
codesign "${app_sign_args[@]}" "$app_bundle"

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
