#!/usr/bin/env bash
set -euo pipefail

# Says what a CI job has to run for the change under test. Prints one
# line, `mode=<full|docs|skip>`, in the form of `$GITHUB_OUTPUT`.
#
#   usage: scope.sh <workflow> <os>
#     workflow  lint | test | build
#     os        macos | linux | windows, or the runner label of the
#               matrix (macos-latest, ubuntu-latest, windows-latest)
#
#   full  run every step of the job
#   docs  run the text checks only (`scripts/lint.sh docs`)
#   skip  run nothing: the change touches nothing that the job checks
#
# A job starts and reports whatever the answer is. The ruleset of
# `main` requires each check by its name, and a check that never
# reports blocks the merge, so a job with nothing to do passes in
# seconds and is not left out.
#
# Each changed file is of one class, and the classes decide:
#
#   docs  Markdown anywhere, `docs/`, the agent files, `LICENSE`.
#         No test reads them. The text checks do.
#   mac   `apps/macos/`, `assets/`, `tools/vm/driver/`: the Mac app,
#         what it bundles, and the Swift driver of Tier VM. Only the
#         macOS jobs build them.
#   core  everything else: the Rust workspace, the scripts, the
#         workflows, and any path this file does not name.
#
# The answer is `full` whenever the changed files are not known: a
# release, a run by hand, a checkout with no history. A wrong `full`
# costs minutes, and a wrong `skip` lets a break through.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  echo "usage: $0 <lint|test|build> <macos|linux|windows>" >&2
  exit 2
}

[[ $# -eq 2 ]] || usage
WORKFLOW="$1"
case "$WORKFLOW" in
  lint | test | build) ;;
  *) usage ;;
esac
case "$2" in
  macos | macos-*) OS=macos ;;
  linux | ubuntu-*) OS=linux ;;
  windows | windows-*) OS=windows ;;
  *) usage ;;
esac

class_of() {
  case "$1" in
    *.md | docs/* | .agents/* | .claude/* | LICENSE) echo docs ;;
    apps/macos/* | assets/* | tools/vm/driver/*) echo mac ;;
    *) echo core ;;
  esac
}

# Prints the changed files, one on a line. Fails when they are not
# known. `--no-renames` lists both names of a moved file, so a file
# that leaves a class still counts for it.
changed_files() {
  # The tests of this script give the list.
  if [[ -n "${VAPOR_SCOPE_FILES+set}" ]]; then
    printf '%s\n' "$VAPOR_SCOPE_FILES"
    return 0
  fi
  case "${GITHUB_EVENT_NAME:-}" in
    pull_request)
      # The checkout of a pull request is the merge of its head into
      # the base, and the first parent of that merge is the base.
      git -C "$ROOT_DIR" rev-parse --verify --quiet 'HEAD^2' >/dev/null || return 1
      git -C "$ROOT_DIR" diff --name-only --no-renames 'HEAD^1' HEAD
      ;;
    push)
      # A tag is a release, and a release runs everything.
      [[ "${GITHUB_REF:-}" == "refs/heads/main" ]] || return 1
      local before="${VAPOR_SCOPE_BEFORE:-}"
      [[ "$before" =~ ^[0-9a-f]{40}$ ]] || return 1
      git -C "$ROOT_DIR" cat-file -e "${before}^{commit}" 2>/dev/null || return 1
      git -C "$ROOT_DIR" diff --name-only --no-renames "$before" HEAD
      ;;
    *)
      return 1
      ;;
  esac
}

decide() {
  local files core=0 mac=0 docs=0 file
  if ! files="$(changed_files)"; then
    echo "[scope] the changed files are not known: full" >&2
    echo full
    return 0
  fi
  while IFS= read -r file; do
    [[ -n "$file" ]] || continue
    case "$(class_of "$file")" in
      core) core=$((core + 1)) ;;
      mac) mac=$((mac + 1)) ;;
      docs) docs=$((docs + 1)) ;;
    esac
  done <<<"$files"
  echo "[scope] changed files: $core core, $mac mac, $docs docs" >&2

  if [[ $((core + mac + docs)) -eq 0 || $core -gt 0 ]]; then
    echo full
  elif [[ $mac -gt 0 && "$OS" == macos ]]; then
    echo full
  elif [[ "$WORKFLOW" == lint && "$OS" == linux ]]; then
    # One job runs the text checks for every change that no full lint
    # covers on this runner.
    echo docs
  else
    echo skip
  fi
}

MODE="$(decide)"
echo "[scope] $WORKFLOW on $OS: $MODE" >&2
echo "mode=$MODE"
