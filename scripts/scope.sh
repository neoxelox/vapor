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
#   docs     Markdown, `docs/`, the agent files, `LICENSE`. No test
#            and no build reads them. The text checks do.
#   macos    `apps/macos/`, `assets/macos/`, `tools/vm/driver/`: the
#            Mac app, what it bundles, and the Swift driver of Tier VM.
#            Everything in the targets of the app is of this class,
#            Markdown too: the build reads its resources.
#   windows  `apps/windows/`, `assets/windows/`.
#   linux    `apps/linux/`, `assets/linux/`.
#   apps     every other asset (`assets/locales/` and the rest): what
#            the apps share. It counts for each system that has an app
#            under `apps/`.
#   core     everything else: the Rust workspace, the scripts, the
#            workflows, and any path this file does not name.
#
# The app of one system is built by the jobs of that system only, so
# a change to it runs those jobs and no others. The core runs on every
# system, so a change to it runs everything.
#
# A Markdown file that a build or a test starts to read is no longer
# docs: name it here in the same change.
#
# The answer is `full` whenever the changed files are not known: a
# release, a checkout with no history, an event that is neither a pull
# request nor a push to `main`. A wrong `full` costs minutes, and a
# wrong `skip` lets a break through.

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

# The class of one changed file. `status` is the letter of
# `git diff --name-status`: M for a change of the content, A and D for
# a file that came or went. A file under another name is both.
class_of() {
  local file="$1" status="$2"
  case "$file" in
    # Two files of the Mac app that a test of the Rust workspace reads
    # on every system (`tools/vm/src/scenario.rs`). The macOS jobs run
    # that test, so they check a change of the content. They do not
    # check a change of the name: macOS folds case and Linux does not,
    # so a new spelling passes there and breaks here.
    apps/macos/Sources/VaporCore/AccessibilityID.swift)
      if [[ "$status" == M ]]; then echo macos; else echo core; fi
      ;;
    assets/locales/en.json)
      if [[ "$status" == M ]]; then echo apps; else echo core; fi
      ;;
    apps/macos/Sources/* | apps/macos/Tests/*) echo macos ;;
    # The layout of these two apps is not known yet, so everything in
    # them is the app, Markdown too.
    apps/windows/*) echo windows ;;
    apps/linux/*) echo linux ;;
    *.md | docs/* | .agents/* | .claude/* | LICENSE) echo docs ;;
    apps/macos/* | assets/macos/* | tools/vm/driver/*) echo macos ;;
    assets/windows/*) echo windows ;;
    assets/linux/*) echo linux ;;
    assets/*) echo apps ;;
    *) echo core ;;
  esac
}

# Prints the changed files, one on a line, as `<status><tab><path>`.
# Fails when they are not known. `--no-renames` lists both names of a
# moved file, so a file that leaves a class still counts for it. With
# `core.quotePath` off, a name with a letter outside ASCII is printed
# as it is; a name that git still quotes matches no class and is core.
changed_files() {
  # The tests of this script give the list. A line with no status is a
  # change of the content.
  if [[ -n "${VAPOR_SCOPE_FILES+set}" ]]; then
    printf '%s\n' "$VAPOR_SCOPE_FILES"
    return 0
  fi
  case "${GITHUB_EVENT_NAME:-}" in
    pull_request)
      # The checkout of a pull request is the merge of its head into
      # the base, and the first parent of that merge is the base.
      git -C "$ROOT_DIR" rev-parse --verify --quiet 'HEAD^2' >/dev/null || return 1
      git -C "$ROOT_DIR" -c core.quotePath=false diff --name-status --no-renames 'HEAD^1' HEAD
      ;;
    push)
      # A tag is a release, and a release runs everything.
      [[ "${GITHUB_REF:-}" == "refs/heads/main" ]] || return 1
      local before="${VAPOR_SCOPE_BEFORE:-}"
      [[ "$before" =~ ^[0-9a-f]{40}$ ]] || return 1
      git -C "$ROOT_DIR" cat-file -e "${before}^{commit}" 2>/dev/null || return 1
      git -C "$ROOT_DIR" -c core.quotePath=false diff --name-status --no-renames "$before" HEAD
      ;;
    *)
      return 1
      ;;
  esac
}

# The systems that have an app: the folders under `apps/`. What the
# apps share counts for each of them.
app_systems() {
  # The tests of this script give the list.
  if [[ -n "${VAPOR_SCOPE_APPS+set}" ]]; then
    echo "$VAPOR_SCOPE_APPS"
    return 0
  fi
  local system found=""
  for system in macos windows linux; do
    [[ -d "$ROOT_DIR/apps/$system" ]] && found="$found $system"
  done
  echo "$found"
}

decide() {
  local files core=0 docs=0 shared=0 line status file tab=$'\t' system
  local need_macos=0 need_windows=0 need_linux=0
  if ! files="$(changed_files)"; then
    echo "[scope] the changed files are not known: full" >&2
    echo full
    return 0
  fi
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    if [[ "$line" == *"$tab"* ]]; then
      status="${line%%"$tab"*}"
      file="${line#*"$tab"}"
    else
      status=M
      file="$line"
    fi
    case "$(class_of "$file" "$status")" in
      core) core=$((core + 1)) ;;
      docs) docs=$((docs + 1)) ;;
      apps) shared=$((shared + 1)) ;;
      macos) need_macos=$((need_macos + 1)) ;;
      windows) need_windows=$((need_windows + 1)) ;;
      linux) need_linux=$((need_linux + 1)) ;;
    esac
  done <<<"$files"

  if [[ $shared -gt 0 ]]; then
    local systems
    systems="$(app_systems)"
    # With no app to build them, nothing says who reads these files.
    [[ -n "${systems// /}" ]] || core=$((core + shared))
    for system in $systems; do
      case "$system" in
        macos) need_macos=$((need_macos + shared)) ;;
        windows) need_windows=$((need_windows + shared)) ;;
        linux) need_linux=$((need_linux + shared)) ;;
      esac
    done
  fi
  echo "[scope] changed files: $core core, $docs docs, $shared shared by the apps;" \
    "for macos $need_macos, windows $need_windows, linux $need_linux" >&2

  local need_here
  case "$OS" in
    macos) need_here=$need_macos ;;
    windows) need_here=$need_windows ;;
    linux) need_here=$need_linux ;;
  esac
  if [[ $((core + docs + need_macos + need_windows + need_linux)) -eq 0 || $core -gt 0 ]]; then
    echo full
  elif [[ $need_here -gt 0 ]]; then
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
