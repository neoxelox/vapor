#!/usr/bin/env bash
# Virtual machine verification (Tier VM): builds the installer, then
# installs it in a macOS guest made with Tart and acts on the app as a
# user does. A run writes its screenshots, video, and report under
# `<repo>/.vapor/vm/`, and deletes every guest it made.
#
# The harness lives in `tools/vm` (crate `vapor-vm`) and the driver
# that works inside a guest in `tools/vm/driver`; this wrapper builds
# both and forwards arguments.
#
# Usage:
#   ./scripts/vm.sh [run options]   run the scenario suite
#   ./scripts/vm.sh --setup         pull the guest image (one time, 36 GB)
#   ./scripts/vm.sh --guest         start a guest for manual work
#   ./scripts/vm.sh --guest-stop    stop and delete the manual guests
#   ./scripts/vm.sh --list          list scenarios
#   ./scripts/vm.sh --clean         delete this checkout's guests and the
#                                   prepared guests; the image stays
#
# Run options (see `vapor-vm run --help`): --only V01,V10  --keep
# --skip-build  --json <path>
#
# Needs a Mac with Apple silicon, macOS 27, and Tart, which the
# contributor installs by hand. This script never runs Homebrew.
#
# Process doc: docs/development/vm-verification.md
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "$(uname -s 2>/dev/null || echo unknown)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
  echo "[vm] Tier VM runs macOS guests, which needs a Mac with Apple silicon."
  exit 2
fi

VM_ROOT="$ROOT_DIR/.vapor/vm"
VM_BIN="${CARGO_TARGET_DIR:-$ROOT_DIR/target}/debug/vapor-vm"

subcommand="run"
args=()
skip_build=0
for arg in "$@"; do
  case "$arg" in
    --setup) subcommand="setup" ;;
    --guest) subcommand="guest" ;;
    --guest-stop) subcommand="guest-stop" ;;
    --list) subcommand="list" ;;
    --clean) subcommand="clean" ;;
    --skip-build) skip_build=1 ;;
    *) args+=("$arg") ;;
  esac
done

cargo build --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p vapor-vm

build_driver() {
  mkdir -p "$VM_ROOT"
  swift build --quiet -c release \
    --package-path "$ROOT_DIR/tools/vm/driver" \
    --scratch-path "$VM_ROOT/driver-build"
}

case "$subcommand" in
  setup)
    build_driver
    ;;
  run | guest)
    if [[ "$skip_build" -eq 0 ]]; then
      echo "[vm] building the installer (./scripts/build.sh package)"
      "$ROOT_DIR/scripts/build.sh" package >"$VM_ROOT.build.log" 2>&1 || {
        tail -n 40 "$VM_ROOT.build.log"
        echo "[vm] the installer did not build; full log: $VM_ROOT.build.log"
        exit 2
      }
      # The service round trip runs the e2e harness inside a guest.
      cargo build --quiet --manifest-path "$ROOT_DIR/Cargo.toml" \
        -p vapor-e2e -p vapor-cli -p vapor-daemon
    fi
    build_driver
    ;;
esac

exec "$VM_BIN" --repo-root "$ROOT_DIR" "$subcommand" ${args[@]+"${args[@]}"}
