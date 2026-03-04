#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export VAPOR_LOG_DIR="${VAPOR_LOG_DIR:-$ROOT_DIR/.vapor/logs}"
export VAPOR_APP_LOG_FILE="${VAPOR_APP_LOG_FILE:-vapor.logs}"
export VAPOR_DAEMON_LOG_FILE="${VAPOR_DAEMON_LOG_FILE:-vapord.logs}"
mkdir -p "$VAPOR_LOG_DIR"

"$ROOT_DIR/scripts/rust/test.sh"
"$ROOT_DIR/scripts/swift/test.sh"
