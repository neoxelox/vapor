#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
# The app name, its executable, and the bundle identifier are fixed
# (AGENTS.md §2.2).
APP_NAME="Vapor"
EXECUTABLE_NAME="Vapor"
BUNDLE_ID="sh.arn.vapor"
# The release asset: one installer, named after the platform and the
# architecture it is for.
PACKAGE_ID="$BUNDLE_ID.pkg"
PACKAGE_NAME="$APP_NAME-macos-arm64.pkg"
MIN_MACOS="${MIN_MACOS:-26.0}"
ICON_DOCUMENT="${ICON_DOCUMENT:-assets/macos/Vapor.icon}"
ICONSET_DIR="${ICONSET_DIR:-assets/macos/Vapor.iconset}"
ASSET_CATALOG="${ASSET_CATALOG:-assets/macos/Vapor.xcassets}"
ACCENT_COLOR_NAME="${ACCENT_COLOR_NAME:-AccentColor}"
DIST_DIR="${DIST_DIR:-dist}"
VAPOR_SIGN_IDENTITY="${VAPOR_SIGN_IDENTITY:-}"
VAPOR_INSTALLER_SIGN_IDENTITY="${VAPOR_INSTALLER_SIGN_IDENTITY:-}"
VAPOR_ENTITLEMENTS="${VAPOR_ENTITLEMENTS:-}"
VAPOR_NOTARY_PROFILE="${VAPOR_NOTARY_PROFILE:-}"
VAPOR_NOTARY_KEYCHAIN="${VAPOR_NOTARY_KEYCHAIN:-}"
export VAPOR_ENV="${VAPOR_ENV:-prod}"

# The Mac app is built for Apple silicon, and the asset name says so.
if [[ "$(uname -m)" != "arm64" ]]; then
  echo "[package] The Mac app is built for Apple silicon; this host is $(uname -m)"
  exit 1
fi

# The app and its installer are signed together or not at all: an
# installer that is not signed is refused whatever the app inside is. An
# unsigned pair is what a prerelease ships.
if [[ -n "$VAPOR_SIGN_IDENTITY" && -z "$VAPOR_INSTALLER_SIGN_IDENTITY" ]] \
  || [[ -z "$VAPOR_SIGN_IDENTITY" && -n "$VAPOR_INSTALLER_SIGN_IDENTITY" ]]; then
  echo "[package] Set both VAPOR_SIGN_IDENTITY (Developer ID Application) and VAPOR_INSTALLER_SIGN_IDENTITY (Developer ID Installer), or neither"
  exit 1
fi

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
# shellcheck source=scripts/dotenv.sh
source "$ROOT_DIR/scripts/dotenv.sh"
vapor_load_build_env "$ROOT_DIR"
vapor_report_build_env "package"
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

# Inside-out signing: the nested daemon binary is signed first with its
# own invocation, then the app bundle (whose seal covers the already-signed
# vapord). Apple documents `--deep` as unsuitable for production signing:
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

mkdir -p "$DIST_DIR"

notary_auth_args=()
if [[ -n "$VAPOR_NOTARY_PROFILE" ]]; then
  if [[ -z "$VAPOR_SIGN_IDENTITY" ]]; then
    echo "[package] Notarization requires VAPOR_SIGN_IDENTITY with Developer ID Application certificate"
    exit 1
  fi
  notary_auth_args=(--keychain-profile "$VAPOR_NOTARY_PROFILE")
  if [[ -n "$VAPOR_NOTARY_KEYCHAIN" ]]; then
    notary_auth_args+=(--keychain "$VAPOR_NOTARY_KEYCHAIN")
  fi
fi

# What the steps below write outside dist/. Removed however the script
# ends: a refused notarization leaves no copy of the app behind.
package_work=""
app_submission_dir=""
cleanup_work() {
  local path
  for path in "$package_work" "$app_submission_dir"; do
    if [[ -n "$path" ]]; then
      rm -rf "$path"
    fi
  done
}
trap cleanup_work EXIT

# Submits one file to Apple and stops unless Apple accepts it.
# `notarytool submit --wait` exits 0 when Apple answers "Invalid", so
# the answer is read from its output, and a refusal prints Apple's log.
notarize() {
  local submission="$1"
  local notary_output notary_status notary_id
  notary_output="$(mktemp)"
  if ! xcrun notarytool submit "$submission" "${notary_auth_args[@]}" --wait 2>&1 | tee "$notary_output"; then
    echo "[package] notarytool submit failed for $submission"
    exit 1
  fi
  notary_status="$(sed -n 's/^ *status: *//p' "$notary_output" | tail -n 1)"
  notary_id="$(sed -n 's/^ *id: *//p' "$notary_output" | tail -n 1)"
  rm -f "$notary_output"
  if [[ "$notary_status" != "Accepted" ]]; then
    echo "[package] Apple did not accept $submission (status: ${notary_status:-unknown})"
    if [[ -n "$notary_id" ]]; then
      xcrun notarytool log "$notary_id" "${notary_auth_args[@]}" || true
    fi
    exit 1
  fi
}

# The app is notarized on its own first, so the bundle that goes into
# the installer carries its ticket and opens with no network.
if [[ -n "$VAPOR_NOTARY_PROFILE" ]]; then
  app_submission_dir="$(mktemp -d)"
  ditto -c -k --keepParent "$app_bundle" "$app_submission_dir/$APP_NAME.zip"
  notarize "$app_submission_dir/$APP_NAME.zip"

  xcrun stapler staple "$app_bundle"
  # Check the ticket, then ask Gatekeeper the question a user's Mac
  # asks at first launch.
  xcrun stapler validate "$app_bundle"
  spctl -a -t exec -vv "$app_bundle"
fi

# The installer: Vapor.app into /Applications, and a script that links
# the CLI of the app onto the PATH.
package_work="$(mktemp -d)"
package_root="$package_work/root"
component_plist="$package_work/component.plist"
component_package="$package_work/packages/$APP_NAME.pkg"
distribution="$package_work/distribution.xml"
package_path="$DIST_DIR/$PACKAGE_NAME"
installer_scripts="$package_work/scripts"

mkdir -p "$package_root/Applications" "$package_work/packages"
ditto "$app_bundle" "$package_root/Applications/$APP_NAME.app"
# Copied without extended attributes: pkgbuild packs each attribute of
# a script as a second file next to it.
ditto --norsrc --noextattr "$ROOT_DIR/apps/macos/scripts/installer" "$installer_scripts"

# Installer moves a "relocatable" bundle to wherever a copy with the
# same identifier already is, a build directory included. Vapor goes to
# /Applications, where the link and the service definitions point. The
# version check goes too: the package the user opens is the version the
# user gets, an older one included.
pkgbuild --analyze --root "$package_root" "$component_plist" >/dev/null
# `-replace` sets a key that the analysis wrote and adds one that it
# left out; which keys it writes changes between macOS versions.
plutil -replace 0.BundleIsRelocatable -bool false "$component_plist"
plutil -replace 0.BundleIsVersionChecked -bool false "$component_plist"

pkgbuild \
  --root "$package_root" \
  --component-plist "$component_plist" \
  --identifier "$PACKAGE_ID" \
  --version "$short_version" \
  --install-location / \
  --scripts "$installer_scripts" \
  "$component_package" >/dev/null

# The distribution wrapper is what refuses an Intel Mac and an older
# macOS before anything is installed.
cat >"$distribution" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="2">
  <title>$APP_NAME</title>
  <options customize="never" require-scripts="false" hostArchitectures="arm64"/>
  <domains enable_localSystem="true" enable_currentUserHome="false" enable_anywhere="false"/>
  <volume-check>
    <allowed-os-versions>
      <os-version min="$MIN_MACOS"/>
    </allowed-os-versions>
  </volume-check>
  <choices-outline>
    <line choice="$PACKAGE_ID"/>
  </choices-outline>
  <choice id="$PACKAGE_ID" visible="false">
    <pkg-ref id="$PACKAGE_ID"/>
  </choice>
  <pkg-ref id="$PACKAGE_ID" version="$short_version" onConclusion="none">$APP_NAME.pkg</pkg-ref>
</installer-gui-script>
EOF

product_args=(--distribution "$distribution" --package-path "$package_work/packages")
if [[ -n "$VAPOR_INSTALLER_SIGN_IDENTITY" ]]; then
  product_args+=(--sign "$VAPOR_INSTALLER_SIGN_IDENTITY" --timestamp)
  if [[ -n "$VAPOR_NOTARY_KEYCHAIN" ]]; then
    product_args+=(--keychain "$VAPOR_NOTARY_KEYCHAIN")
  fi
fi
rm -f "$package_path"
productbuild "${product_args[@]}" "$package_path" >/dev/null

if [[ -n "$VAPOR_NOTARY_PROFILE" ]]; then
  notarize "$package_path"
  xcrun stapler staple "$package_path"
  xcrun stapler validate "$package_path"
  # The question Gatekeeper asks when the user opens the installer.
  spctl -a -t install -vv "$package_path"
  echo "[package] Notarized installer: $package_path"
fi

echo "[package] App bundle: $app_bundle"
echo "[package] Installer: $package_path"
