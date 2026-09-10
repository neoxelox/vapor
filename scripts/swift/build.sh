#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/apps/macos"
MODE="${1:-build}"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
if [[ "$MODE" == "package" ]]; then
  export VAPOR_ENV="${VAPOR_ENV:-prod}"
else
  export VAPOR_ENV="${VAPOR_ENV:-dev}"
fi
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

if [[ "$MODE" != "build" && "$MODE" != "package" ]]; then
  echo "Usage: scripts/swift/build.sh [build|package]"
  exit 1
fi

if [[ ! -d "$SWIFT_DIR" ]]; then
  echo "[swift-build] apps/macos not found. Skipping Swift build."
  exit 0
fi

"$ROOT_DIR/scripts/swift/resources.sh"

if [[ -f "$SWIFT_DIR/Package.swift" ]]; then
  if [[ "$MODE" == "package" ]]; then
    PACKAGE_SCRIPT="$SWIFT_DIR/scripts/package.sh"
    if [[ ! -f "$PACKAGE_SCRIPT" ]]; then
      echo "[swift-build] Package mode requested but packaging script is missing at $PACKAGE_SCRIPT"
      exit 1
    fi

    echo "[swift-build] package mode; running $PACKAGE_SCRIPT"
    bash "$PACKAGE_SCRIPT"
    exit 0
  fi

  echo "[swift-build] swift build --package-path $SWIFT_DIR -c release"
  swift build \
    --package-path "$SWIFT_DIR" \
    -c release \
    --disable-index-store \
    -Xswiftc -whole-module-optimization \
    -Xswiftc -cross-module-optimization

  exit 0
fi

if compgen -G "$SWIFT_DIR/*.xcodeproj" >/dev/null || compgen -G "$SWIFT_DIR/*.xcworkspace" >/dev/null; then
  if [[ -z "${VAPOR_XCODE_SCHEME:-}" ]]; then
    echo "[swift-build] Xcode project detected but VAPOR_XCODE_SCHEME is not set. Skipping Swift build."
    echo "[swift-build] Set VAPOR_XCODE_SCHEME to run xcodebuild build."
    exit 0
  fi

  if compgen -G "$SWIFT_DIR/*.xcworkspace" >/dev/null; then
    workspace_path="$(ls "$SWIFT_DIR"/*.xcworkspace | head -n 1)"
    echo "[swift-build] xcodebuild build -workspace $workspace_path -scheme $VAPOR_XCODE_SCHEME -configuration Release"
    xcodebuild build -workspace "$workspace_path" -scheme "$VAPOR_XCODE_SCHEME" -configuration Release -destination 'platform=macOS'
    exit 0
  fi

  project_path="$(ls "$SWIFT_DIR"/*.xcodeproj | head -n 1)"
  echo "[swift-build] xcodebuild build -project $project_path -scheme $VAPOR_XCODE_SCHEME -configuration Release"
  xcodebuild build -project "$project_path" -scheme "$VAPOR_XCODE_SCHEME" -configuration Release -destination 'platform=macOS'
  exit 0
fi

echo "[swift-build] No Swift package or Xcode project found in apps/macos. Skipping Swift build."
