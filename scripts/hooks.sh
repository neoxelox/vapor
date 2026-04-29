#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-install}"

if [[ "$MODE" != "install" && "$MODE" != "uninstall" ]]; then
  echo "Usage: scripts/hooks.sh [install|uninstall]"
  exit 1
fi

GIT_DIR="$ROOT_DIR/.git"
HOOKS_DIR="$GIT_DIR/hooks"
PRE_COMMIT_HOOK="$HOOKS_DIR/pre-commit"
MARKER="# vapor-managed-hook: pre-commit"

if [[ ! -d "$GIT_DIR" ]]; then
  echo "[hooks] $ROOT_DIR is not a git repository ($GIT_DIR is missing)"
  exit 1
fi

mkdir -p "$HOOKS_DIR"

if [[ "$MODE" == "uninstall" ]]; then
  if [[ -f "$PRE_COMMIT_HOOK" ]] && grep -qF "$MARKER" "$PRE_COMMIT_HOOK"; then
    rm -f "$PRE_COMMIT_HOOK"
    echo "[hooks] removed vapor-managed pre-commit hook"
  else
    echo "[hooks] no vapor-managed pre-commit hook to remove"
  fi
  exit 0
fi

if [[ -f "$PRE_COMMIT_HOOK" ]] && ! grep -qF "$MARKER" "$PRE_COMMIT_HOOK"; then
  BACKUP="$PRE_COMMIT_HOOK.bak"
  if [[ -e "$BACKUP" ]]; then
    echo "[hooks] refusing to overwrite existing $PRE_COMMIT_HOOK; $BACKUP already taken"
    echo "[hooks] move or delete one of those files and retry"
    exit 1
  fi
  mv "$PRE_COMMIT_HOOK" "$BACKUP"
  echo "[hooks] preserved existing pre-commit hook at $BACKUP"
fi

cat >"$PRE_COMMIT_HOOK" <<'HOOK'
#!/usr/bin/env bash
# vapor-managed-hook: pre-commit
#
# Installed by scripts/hooks.sh. Runs the full local validation pipeline
# before every commit so agentic contributors and humans share one bar:
# clean -> lint -> test -> build. Any failure aborts the commit.
#
# Re-run scripts/hooks.sh install to refresh after updates; remove via
# scripts/hooks.sh uninstall.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

echo "Cleaning build and distribution artifacts..."
"$REPO_ROOT/scripts/clean.sh"

echo "Linting source files..."
"$REPO_ROOT/scripts/lint.sh"

echo "Running test suite..."
"$REPO_ROOT/scripts/test.sh"

echo "Building release binaries..."
"$REPO_ROOT/scripts/build.sh"

echo "All checks passed."
HOOK

chmod +x "$PRE_COMMIT_HOOK"
echo "[hooks] installed pre-commit hook at $PRE_COMMIT_HOOK"
echo "[hooks] runs: clean -> lint -> test -> build"
