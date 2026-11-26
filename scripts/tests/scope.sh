#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCOPE="$ROOT_DIR/scripts/scope.sh"

# This test makes repositories of its own with `git init`. Inside a git
# hook, git exports GIT_DIR, GIT_INDEX_FILE, and others for the
# repository of the hook, and they win over `git -C`: every command
# below would then act on that repository, and `git init` would mark
# it as bare. Drop every variable that ties git to a repository. Git
# has the list; the names after it are for a git that has none.
# shellcheck disable=SC2046
unset $(git rev-parse --local-env-vars 2>/dev/null || true) \
  GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
  GIT_COMMON_DIR GIT_NAMESPACE GIT_PREFIX GIT_CONFIG_PARAMETERS \
  GIT_CONFIG_COUNT

# The settings of the contributor (a signing key, a hooks path) must
# not decide whether a commit of a fixture works.
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null

fail() {
  echo "[scope-test] FAIL: $1" >&2
  exit 1
}

# The mode for a list of changed files, with no event in the way.
mode_for() {
  local workflow="$1" os="$2" files="$3"
  env -u GITHUB_EVENT_NAME -u GITHUB_REF VAPOR_SCOPE_FILES="$files" \
    bash "$SCOPE" "$workflow" "$os" 2>/dev/null
}

expect() {
  local want="$1" workflow="$2" os="$3" files="$4" got
  got="$(mode_for "$workflow" "$os" "$files")"
  [[ "$got" == "mode=$want" ]] ||
    fail "$workflow on $os for [$(echo "$files" | tr '\n' ' ')]: want mode=$want, got $got"
}

# The nine jobs for one list of files: the modes of lint, test, and
# build, each on macos, linux, windows.
expect_all() {
  local files="$1"
  shift
  local workflow os
  for workflow in lint test build; do
    for os in macos linux windows; do
      expect "$1" "$workflow" "$os" "$files"
      shift
    done
  done
}

docs_only=$'AGENTS.md\ndocs/ci/overview.md\n.agents/skills/vapor-pr/SKILL.md\n.claude/skills/vapor-pr\nLICENSE'
expect_all "$docs_only" \
  skip docs skip \
  skip skip skip \
  skip skip skip

mac_only=$'apps/macos/Sources/VaporCore/AccessibilityID.swift\nassets/locales/en.json\ntools/vm/driver/Sources/main.swift'
expect_all "$mac_only" \
  full docs skip \
  full skip skip \
  full skip skip

expect_all "$mac_only"$'\n'"$docs_only" \
  full docs skip \
  full skip skip \
  full skip skip

# The app of one system runs the jobs of that system, and no others.
windows_only=$'apps/windows/App.xaml\napps/windows/README.md\nassets/windows/vapor.ico'
expect_all "$windows_only" \
  skip docs full \
  skip skip full \
  skip skip full

linux_only=$'apps/linux/src/main.c\nassets/linux/vapor.svg'
expect_all "$linux_only" \
  skip full skip \
  skip full skip \
  skip full skip

expect_all "$windows_only"$'\n'"$mac_only" \
  full docs full \
  full skip full \
  full skip full

# What the apps share counts for each system that has an app.
shared_assets=$'assets/locales/es.json\nassets/marks/mark.svg'
VAPOR_SCOPE_APPS="macos" expect_all "$shared_assets" \
  full docs skip \
  full skip skip \
  full skip skip
VAPOR_SCOPE_APPS="macos windows linux" expect_all "$shared_assets" \
  full full full \
  full full full \
  full full full
VAPOR_SCOPE_APPS="macos windows" expect_all "$shared_assets" \
  full docs full \
  full skip full \
  full skip full
# With no app at all, nothing says who reads them: everything runs.
VAPOR_SCOPE_APPS="" expect_all "$shared_assets" \
  full full full \
  full full full \
  full full full

# One file of the core runs everything, whatever comes with it.
for core_file in \
  core/daemon/src/executor.rs \
  tools/e2e/src/scenarios/sync.rs \
  tools/vm/src/runner.rs \
  scripts/lint.sh \
  .github/workflows/test.yml \
  Cargo.lock \
  VERSION \
  .env.example \
  .gitignore \
  apps/README.txt \
  core/platform/src/fs_watch/windows.rs \
  a-new-file-at-the-root; do
  expect_all "$core_file"$'\n'"$mac_only"$'\n'"$docs_only" \
    full full full \
    full full full \
    full full full
done

# Markdown is docs, but not inside the targets of the app: the build
# reads the resources there. A locale catalog is not docs.
expect skip test macos "apps/macos/README.md"
expect full build macos "apps/macos/Sources/VaporCore/Resources/NOTES.md"
expect skip build linux "apps/macos/Sources/VaporCore/Resources/NOTES.md"
expect full test macos "assets/locales/en.json"
expect skip test linux "assets/locales/en.json"

# The two files of the app that a test of the Rust workspace reads on
# every system. A change of the content stays with the macOS jobs. A
# file that came or went, which is what another spelling of the name
# or of a folder above it is, runs everything: macOS folds case, and
# only Linux sees the old name gone.
tab=$'\t'
expect skip test linux "M${tab}apps/macos/Sources/VaporCore/AccessibilityID.swift"
expect full test linux "D${tab}apps/macos/Sources/VaporCore/AccessibilityID.swift"$'\n'"A${tab}apps/macos/Sources/VaporCore/AccessibilityId.swift"
expect full test windows "D${tab}assets/locales/en.json"$'\n'"A${tab}assets/Locales/en.json"
expect skip test linux "A${tab}apps/macos/Sources/VaporCore/NewView.swift"

# No changed file is no knowledge: everything runs.
expect full build windows ""

# The runner labels of the matrix work as they are.
expect docs lint ubuntu-latest "README.md"
expect full build macos-latest "apps/macos/Package.swift"
expect skip test windows-latest "README.md"

# Outside a pull request and a push to main, everything runs.
got="$(env -u VAPOR_SCOPE_FILES -u GITHUB_EVENT_NAME bash "$SCOPE" lint linux 2>/dev/null)"
[[ "$got" == "mode=full" ]] || fail "no event: want mode=full, got $got"
got="$(env -u VAPOR_SCOPE_FILES GITHUB_EVENT_NAME=workflow_dispatch bash "$SCOPE" test macos 2>/dev/null)"
[[ "$got" == "mode=full" ]] || fail "a run by hand: want mode=full, got $got"

if bash "$SCOPE" lint 2>/dev/null; then fail "one argument was accepted"; fi
if bash "$SCOPE" deploy linux 2>/dev/null; then fail "an unknown workflow was accepted"; fi
if bash "$SCOPE" lint solaris 2>/dev/null; then fail "an unknown system was accepted"; fi

# The changed files of a real checkout. The script reads the
# repository that it is in, so each case gets its own.
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

git_in() {
  local repo="$1"
  shift
  git -C "$repo" -c user.name=vapor-test -c user.email=vapor-test@example.com \
    -c commit.gpgsign=false -c init.defaultBranch=main "$@"
}

new_repo() {
  local repo="$fixture/$1"
  mkdir -p "$repo/scripts" "$repo/core" "$repo/docs"
  cp "$SCOPE" "$repo/scripts/scope.sh"
  git_in "$repo" init --quiet
  # Never go on with a git that looks at another repository: from the
  # fixture, the repository is `.git` right here, and the fixture is
  # the top of the work tree. Both answers are relative, so they read
  # the same on every system.
  [[ "$(cd "$repo" && git rev-parse --git-dir)" == ".git" &&
    -z "$(cd "$repo" && git rev-parse --show-cdup)" ]] ||
    fail "git does not use the fixture at $repo"
  echo "fn main() {}" >"$repo/core/main.rs"
  echo "# Guide" >"$repo/docs/guide.md"
  git_in "$repo" add -A
  git_in "$repo" commit --quiet -m base
  echo "$repo"
}

scope_in() {
  local repo="$1"
  shift
  (cd "$repo" && env -u VAPOR_SCOPE_FILES "$@" bash scripts/scope.sh test linux 2>/dev/null)
}

# A pull request: the checkout is the merge of the head into the base.
repo="$(new_repo pull-request-docs)"
git_in "$repo" checkout --quiet -b change
echo "More." >>"$repo/docs/guide.md"
git_in "$repo" commit --quiet -am "docs change"
git_in "$repo" checkout --quiet main
echo "// base moved" >>"$repo/core/main.rs"
git_in "$repo" commit --quiet -am "base moves"
git_in "$repo" merge --quiet --no-ff -m merge change
got="$(scope_in "$repo" GITHUB_EVENT_NAME=pull_request)"
[[ "$got" == "mode=skip" ]] ||
  fail "a pull request that changes docs, on a base that moved in core: want mode=skip, got $got"

repo="$(new_repo pull-request-core)"
git_in "$repo" checkout --quiet -b change
echo "// change" >>"$repo/core/main.rs"
git_in "$repo" commit --quiet -am "core change"
git_in "$repo" checkout --quiet main
git_in "$repo" merge --quiet --no-ff -m merge change
got="$(scope_in "$repo" GITHUB_EVENT_NAME=pull_request)"
[[ "$got" == "mode=full" ]] || fail "a pull request that changes core: want mode=full, got $got"

# A file that moves from the core to the docs still counts for the core.
repo="$(new_repo pull-request-move)"
git_in "$repo" checkout --quiet -b change
git_in "$repo" mv core/main.rs docs/main.md
git_in "$repo" commit --quiet -m "move"
git_in "$repo" checkout --quiet main
git_in "$repo" merge --quiet --no-ff -m merge change
got="$(scope_in "$repo" GITHUB_EVENT_NAME=pull_request)"
[[ "$got" == "mode=full" ]] || fail "a file moved out of core: want mode=full, got $got"

# A name with a letter outside ASCII keeps its class.
repo="$(new_repo pull-request-accent)"
git_in "$repo" checkout --quiet -b change
# The name is written as bytes, so that it is the same on every system.
accent_name="$(printf 'caf\303\251.md')"
echo "Texto." >"$repo/docs/$accent_name"
git_in "$repo" add -A
git_in "$repo" commit --quiet -m "docs with an accent"
git_in "$repo" checkout --quiet main
git_in "$repo" merge --quiet --no-ff -m merge change
got="$(scope_in "$repo" GITHUB_EVENT_NAME=pull_request)"
[[ "$got" == "mode=skip" ]] || fail "a document with an accent in its name: want mode=skip, got $got"

# A checkout that is not a merge says nothing about the change.
repo="$(new_repo pull-request-no-merge)"
echo "More." >>"$repo/docs/guide.md"
git_in "$repo" commit --quiet -am "docs change"
got="$(scope_in "$repo" GITHUB_EVENT_NAME=pull_request)"
[[ "$got" == "mode=full" ]] || fail "a checkout with one parent: want mode=full, got $got"

# A push to main: the change is what came after the commit before it.
repo="$(new_repo push-docs)"
before="$(git_in "$repo" rev-parse HEAD)"
echo "More." >>"$repo/docs/guide.md"
git_in "$repo" commit --quiet -am "docs change"
got="$(scope_in "$repo" GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/main VAPOR_SCOPE_BEFORE="$before")"
[[ "$got" == "mode=skip" ]] || fail "a push of docs to main: want mode=skip, got $got"
got="$(scope_in "$repo" GITHUB_EVENT_NAME=push GITHUB_REF=refs/tags/v1.0.0 VAPOR_SCOPE_BEFORE="$before")"
[[ "$got" == "mode=full" ]] || fail "a tag: want mode=full, got $got"
got="$(scope_in "$repo" GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/main VAPOR_SCOPE_BEFORE=0000000000000000000000000000000000000000)"
[[ "$got" == "mode=full" ]] || fail "a push with no commit before it: want mode=full, got $got"
got="$(scope_in "$repo" GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/main)"
[[ "$got" == "mode=full" ]] || fail "a push with nothing known before it: want mode=full, got $got"

echo "[scope-test] OK"
