#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# A manual e2e sandbox leaves its daemon running on purpose; stop it
# before its home disappears from under it. The pid file can outlive
# its daemon, and the OS can give the number to another process: to the
# installed daemon too, whose name is the same. The signal goes only to
# a `vapor` or `vapord` that runs from this repository's build.
ROOT_DIR_PHYSICAL="$(cd "$ROOT_DIR" && pwd -P)"
is_repository_daemon() {
  local command root inside
  command="$(ps -p "$1" -o args= 2>/dev/null || true)"
  for root in "$ROOT_DIR" "$ROOT_DIR_PHYSICAL"; do
    [[ "$command" == "$root"/* ]] || continue
    inside="${command#"$root"/}"
    inside="${inside%% *}"
    case "${inside##*/}" in
      vapord|vapor) return 0 ;;
    esac
  done
  return 1
}

for pid_file in "$ROOT_DIR"/.vapor/e2e/*/daemon.pid; do
  [[ -f "$pid_file" ]] || continue
  pid="$(tr -d '[:space:]' < "$pid_file")"
  [[ "$pid" =~ ^[0-9]+$ ]] || continue
  if ! is_repository_daemon "$pid"; then
    continue
  fi
  if kill -0 "$pid" 2>/dev/null; then
    echo "[clean] stopping sandbox daemon pid $pid"
    kill -TERM "$pid" 2>/dev/null || true
    for _ in $(seq 1 40); do
      kill -0 "$pid" 2>/dev/null || break
      sleep 0.25
    done
    kill -KILL "$pid" 2>/dev/null || true
  fi
done

# A Tier VM run that was interrupted, or a manual guest, leaves a test
# guest in Tart's store. The store is shared by every checkout, so only
# the guests that this checkout made go: their names carry a tag made
# from its path. `./scripts/vm.sh --clean` also removes the prepared
# guests; the pulled image stays either way.
if command -v tart >/dev/null 2>&1 && command -v shasum >/dev/null 2>&1; then
  checkout_tag="$(printf '%s' "$ROOT_DIR_PHYSICAL" | shasum -a 256 | cut -c1-8)"
  while IFS= read -r vm; do
    [[ "$vm" == "vapor-vm-$checkout_tag-"* ]] || continue
    echo "[clean] deleting the test guest $vm"
    tart stop "$vm" >/dev/null 2>&1 || true
    tart delete "$vm" >/dev/null 2>&1 || true
  done < <(tart list --source local --quiet 2>/dev/null || true)
fi

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
