#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SOURCE_SCRIPT="$ROOT_DIR/scripts/version.sh"

# These tests build isolated fixture repos via `git init`. When the test runs
# inside a parent `git` operation (e.g. the project's pre-commit hook), git
# exports GIT_DIR / GIT_WORK_TREE / GIT_INDEX_FILE / etc. into the hook's
# environment. Those override `git -C "$repo"` and cause `git init` to
# "re-init" the parent repo instead of the temp dir, which breaks every
# fixture-based assertion. Drop them so child `git` invocations resolve
# their repo from the cwd / -C path like a fresh shell would.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
  GIT_COMMON_DIR GIT_NAMESPACE GIT_PREFIX

# Isolate config-driven git behavior too: a contributor's global
# `commit.gpgsign = true` (without a usable key) or a `core.hooksPath`
# that rejects these commits would fail every fixture commit — and thus
# the required `./scripts/test.sh` gate — for reasons unrelated to the
# code under test. (git >= 2.32 honors these overrides.)
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null

export GIT_AUTHOR_NAME="Vapor Test"
export GIT_AUTHOR_EMAIL="vapor-tests@example.com"
export GIT_COMMITTER_NAME="$GIT_AUTHOR_NAME"
export GIT_COMMITTER_EMAIL="$GIT_AUTHOR_EMAIL"

TEMP_REPOS=()

cleanup() {
  local repo
  for repo in "${TEMP_REPOS[@]:-}"; do
    rm -rf "$repo"
  done
}

trap cleanup EXIT

fail() {
  printf '[version-test] %s\n' "$1" >&2
  exit 1
}

assert_equals() {
  local actual="$1"
  local expected="$2"
  local context="$3"

  if [[ "$actual" != "$expected" ]]; then
    fail "$context: expected '$expected' but got '$actual'"
  fi
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local context="$3"

  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$context: missing '$needle'"
  fi
}

make_repo() {
  local version="$1"
  local repo

  repo="$(mktemp -d)"
  TEMP_REPOS+=("$repo")

  mkdir -p "$repo/scripts"
  cp "$SOURCE_SCRIPT" "$repo/scripts/version.sh"
  chmod +x "$repo/scripts/version.sh"

  cat > "$repo/VERSION" <<EOF
$version
EOF

  cat > "$repo/Cargo.toml" <<EOF
[workspace]
members = []
resolver = "2"

[workspace.package]
version = "$version"
EOF

  cat > "$repo/CHANGELOG.md" <<'EOF'
# Changelog

## [Unreleased]
EOF

  git -C "$repo" init -b main >/dev/null
  git -C "$repo" add VERSION Cargo.toml CHANGELOG.md scripts/version.sh
  git -C "$repo" commit -m "chore: seed version test fixture" >/dev/null

  printf '%s\n' "$repo"
}

write_changelog_entry() {
  local repo="$1"
  local version="$2"

  cat > "$repo/CHANGELOG.md" <<EOF
# Changelog

## [Unreleased]

## [$version] - 2026-03-10

- Release preparation.
EOF
}

cargo_version() {
  python3 - "$1/Cargo.toml" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
for line in path.read_text(encoding="utf-8").splitlines():
    stripped = line.strip()
    if stripped.startswith("version = "):
        print(stripped.split("=", 1)[1].strip().strip('"'))
        break
PY
}

test_rejects_dirty_worktree_except_changelog() {
  local repo output version
  repo="$(make_repo "0.1.0")"
  write_changelog_entry "$repo" "0.2.0"
  printf 'extra\n' > "$repo/notes.txt"

  if output="$(cd "$repo" && ./scripts/version.sh set 0.2.0 2>&1)"; then
    fail "dirty worktree release unexpectedly succeeded"
  fi

  assert_contains "$output" "clean worktree except CHANGELOG.md" "dirty worktree error"
  version="$(tr -d '[:space:]' < "$repo/VERSION")"
  assert_equals "$version" "0.1.0" "dirty worktree VERSION"
}

test_requires_matching_changelog_entry() {
  local repo output version
  repo="$(make_repo "0.1.0")"

  if output="$(cd "$repo" && ./scripts/version.sh set 0.2.0 2>&1)"; then
    fail "release without changelog entry unexpectedly succeeded"
  fi

  assert_contains "$output" "CHANGELOG.md missing section for [0.2.0]" "changelog gate"
  version="$(tr -d '[:space:]' < "$repo/VERSION")"
  assert_equals "$version" "0.1.0" "missing changelog VERSION"
}

test_set_creates_release_commit_and_tag() {
  local repo output version subject tag status
  repo="$(make_repo "0.1.0")"
  write_changelog_entry "$repo" "0.2.0"

  output="$(cd "$repo" && ./scripts/version.sh set 0.2.0)"

  version="$(tr -d '[:space:]' < "$repo/VERSION")"
  assert_equals "$version" "0.2.0" "stable set VERSION"
  assert_equals "$(cargo_version "$repo")" "0.2.0" "stable set Cargo.toml"

  subject="$(git -C "$repo" log -1 --format=%s)"
  assert_equals "$subject" "release: v0.2.0" "stable set commit subject"

  tag="$(git -C "$repo" tag --list "v0.2.0")"
  assert_equals "$tag" "v0.2.0" "stable set tag"

  status="$(git -C "$repo" status --short)"
  assert_equals "$status" "" "stable set worktree"
  assert_contains "$output" "git push origin \"main\" --follow-tags" "stable set push command"
}

test_prerelease_command_creates_release_commit_and_tag() {
  local repo version tag
  repo="$(make_repo "0.2.0")"
  write_changelog_entry "$repo" "0.2.0-rc.1"

  cd "$repo"
  ./scripts/version.sh prerelease rc >/dev/null
  cd "$ROOT_DIR"

  version="$(tr -d '[:space:]' < "$repo/VERSION")"
  assert_equals "$version" "0.2.0-rc.1" "prerelease VERSION"
  assert_equals "$(cargo_version "$repo")" "0.2.0-rc.1" "prerelease Cargo.toml"
  tag="$(git -C "$repo" tag --list "v0.2.0-rc.1")"
  assert_equals "$tag" "v0.2.0-rc.1" "prerelease tag"
}

test_release_command_strips_prerelease_and_tags_stable() {
  local repo version subject tag
  repo="$(make_repo "0.2.0-rc.1")"
  write_changelog_entry "$repo" "0.2.0"

  cd "$repo"
  ./scripts/version.sh release >/dev/null
  cd "$ROOT_DIR"

  version="$(tr -d '[:space:]' < "$repo/VERSION")"
  assert_equals "$version" "0.2.0" "release VERSION"
  subject="$(git -C "$repo" log -1 --format=%s)"
  assert_equals "$subject" "release: v0.2.0" "release commit subject"
  tag="$(git -C "$repo" tag --list "v0.2.0")"
  assert_equals "$tag" "v0.2.0" "release tag"
}

printf '[version-test] running version script checks\n'
test_rejects_dirty_worktree_except_changelog
test_requires_matching_changelog_entry
test_set_creates_release_commit_and_tag
test_prerelease_command_creates_release_commit_and_tag
test_release_command_strips_prerelease_and_tags_stable
printf '[version-test] all checks passed\n'
