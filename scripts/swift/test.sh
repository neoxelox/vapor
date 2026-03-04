#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SWIFT_DIR="$ROOT_DIR/apps/macos"

if [[ ! -d "$SWIFT_DIR" ]]; then
  echo "[swift-test] apps/macos not found. Skipping Swift tests."
  exit 0
fi

if [[ -f "$SWIFT_DIR/Package.swift" ]]; then
  echo "[swift-test] swift test --package-path $SWIFT_DIR"
  swift test --package-path "$SWIFT_DIR"
  exit 0
fi

if compgen -G "$SWIFT_DIR/*.xcodeproj" >/dev/null || compgen -G "$SWIFT_DIR/*.xcworkspace" >/dev/null; then
  if [[ -z "${VAPOR_XCODE_SCHEME:-}" ]]; then
    echo "[swift-test] Xcode project detected but VAPOR_XCODE_SCHEME is not set. Skipping Swift tests."
    echo "[swift-test] Set VAPOR_XCODE_SCHEME to run xcodebuild test."
    exit 0
  fi

  if compgen -G "$SWIFT_DIR/*.xcworkspace" >/dev/null; then
    local_workspace="$(ls "$SWIFT_DIR"/*.xcworkspace | head -n 1)"
    echo "[swift-test] xcodebuild test -workspace $local_workspace -scheme $VAPOR_XCODE_SCHEME"
    xcodebuild test -workspace "$local_workspace" -scheme "$VAPOR_XCODE_SCHEME" -destination 'platform=macOS'
    exit 0
  fi

  local_project="$(ls "$SWIFT_DIR"/*.xcodeproj | head -n 1)"
  echo "[swift-test] xcodebuild test -project $local_project -scheme $VAPOR_XCODE_SCHEME"
  xcodebuild test -project "$local_project" -scheme "$VAPOR_XCODE_SCHEME" -destination 'platform=macOS'
  exit 0
fi

echo "[swift-test] No Swift package or Xcode project found in apps/macos. Skipping Swift tests."
