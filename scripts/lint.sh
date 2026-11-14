#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

"$ROOT_DIR/scripts/rust/lint.sh"

if [[ "$(uname -s 2>/dev/null || echo unknown)" == "Darwin" ]]; then
  "$ROOT_DIR/scripts/swift/lint.sh"
else
  echo "[lint] Host is not macOS. Skipping Swift lint (apps/macos is macOS-only)."
fi

"$ROOT_DIR/scripts/rust/format.sh" check

# The locale catalogs parse as JSON: a broken catalog fails here, not
# at the first build of the app.
for catalog in "$ROOT_DIR"/assets/locales/*.json; do
  [[ -f "$catalog" ]] || continue
  if ! python3 -m json.tool "$catalog" >/dev/null; then
    echo "[lint] $catalog is not valid JSON"
    exit 1
  fi
done

# No tracked text file holds an em dash (AGENTS.md §8.11). The
# character is written as bytes here so this file passes its own check.
em_dash=$'\xe2\x80\x94'
if git -C "$ROOT_DIR" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  if git -C "$ROOT_DIR" grep -n -I "$em_dash" -- .; then
    echo "[lint] The lines above hold an em dash. Use a comma, a colon, or two sentences."
    exit 1
  fi
fi
