#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-install}"

if [[ "$MODE" != "install" && "$MODE" != "uninstall" ]]; then
  echo "Usage: scripts/hooks.sh [install|uninstall]"
  exit 1
fi

# Resolve the hooks directory through git itself so worktrees (where
# `.git` is a file pointing at the shared common dir) work too.
if ! HOOKS_DIR="$(git -C "$ROOT_DIR" rev-parse --git-path hooks 2>/dev/null)"; then
  echo "[hooks] $ROOT_DIR is not a git repository"
  exit 1
fi
if [[ "$HOOKS_DIR" != /* ]]; then
  HOOKS_DIR="$ROOT_DIR/$HOOKS_DIR"
fi
PRE_COMMIT_HOOK="$HOOKS_DIR/pre-commit"
MARKER="# vapor-managed-hook: pre-commit"

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
# Installed by scripts/hooks.sh. Runs the local validation pipeline
# before every commit so agentic contributors and humans share one bar:
# lint -> test. Incremental on purpose — a clean/cold rebuild per commit
# would cost minutes for no additional signal (CI runs the full matrix);
# run scripts/clean.sh + scripts/build.sh manually when needed.
#
# Re-run scripts/hooks.sh install to refresh after updates; remove via
# scripts/hooks.sh uninstall.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

echo "Linting source files..."
"$REPO_ROOT/scripts/lint.sh"

echo "Running test suite..."
"$REPO_ROOT/scripts/test.sh"

echo "All checks passed."
HOOK

chmod +x "$PRE_COMMIT_HOOK"
echo "[hooks] installed pre-commit hook at $PRE_COMMIT_HOOK"
echo "[hooks] runs: lint -> test"
