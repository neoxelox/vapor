#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# A manual e2e sandbox leaves its daemon running on purpose; stop it
# before its home disappears from under it.
for pid_file in "$ROOT_DIR"/.vapor/e2e/*/daemon.pid; do
  [[ -f "$pid_file" ]] || continue
  pid="$(tr -d '[:space:]' < "$pid_file")"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    echo "[clean] stopping sandbox daemon pid $pid"
    kill -TERM "$pid" 2>/dev/null || true
    for _ in $(seq 1 40); do
      kill -0 "$pid" 2>/dev/null || break
      sleep 0.25
    done
    kill -KILL "$pid" 2>/dev/null || true
  fi
done

paths=(
  "$ROOT_DIR/target"
  "$ROOT_DIR/debug"
  "$ROOT_DIR/dist"
  "$ROOT_DIR/.vapor"
  "$ROOT_DIR/apps/macos/.build"
  "$ROOT_DIR/apps/macos/.swiftpm"
  "$ROOT_DIR/apps/macos/dist"
)

# A preserved soak or e2e sandbox may still have a throwaway disk image
# mounted; detach it before the directory goes.
for mount in "$ROOT_DIR"/.vapor/e2e/*/*-mnt; do
  [[ -d "$mount" ]] || continue
  if mount | grep -q " on $mount "; then
    echo "[clean] detaching disk image at $mount"
    hdiutil detach -quiet "$mount" 2>/dev/null || hdiutil detach -quiet -force "$mount" 2>/dev/null || true
  fi
done

for path in "${paths[@]}"; do
  if [[ -e "$path" ]]; then
    echo "[clean] removing $path"
    rm -rf "$path"
  else
    echo "[clean] skip missing $path"
  fi
done

echo "[clean] done"
