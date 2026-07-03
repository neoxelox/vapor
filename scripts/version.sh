#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION_FILE="$ROOT_DIR/VERSION"
CARGO_TOML="$ROOT_DIR/Cargo.toml"
CARGO_LOCK="$ROOT_DIR/Cargo.lock"
CHANGELOG_FILE="$ROOT_DIR/CHANGELOG.md"
RELEASE_BRANCH="main"

usage() {
  cat <<'EOF'
Usage: ./scripts/version.sh [command] [args]

Commands:
  current                    Print the current version from VERSION
  metadata                   Print build metadata as shell KEY=value pairs
  check-sync                 Fail if Cargo.toml does not match VERSION
  sync                       Sync Cargo.toml from VERSION
  set <version>              Prepare a release commit/tag for X.Y.Z or X.Y.Z-(alpha|beta|rc).N
  bump <major|minor|patch>   Prepare a stable release commit/tag after bumping the base version
  prerelease <alpha|beta|rc> [number]
                             Prepare a prerelease commit/tag by setting or incrementing that track
  release                    Prepare a stable release commit/tag by stripping the prerelease suffix
EOF
}

die() {
  printf '[version] %s\n' "$1" >&2
  exit 1
}

VERSION_MAJOR=""
VERSION_MINOR=""
VERSION_PATCH=""
VERSION_PRERELEASE_LABEL=""
VERSION_PRERELEASE_NUMBER=""
CURRENT_VERSION=""
CURRENT_BRANCH=""

parse_version() {
  local version="$1"

  if [[ ! "$version" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-((alpha|beta|rc)\.([0-9]+)))?$ ]]; then
    return 1
  fi

  VERSION_MAJOR="${BASH_REMATCH[1]}"
  VERSION_MINOR="${BASH_REMATCH[2]}"
  VERSION_PATCH="${BASH_REMATCH[3]}"
  VERSION_PRERELEASE_LABEL="${BASH_REMATCH[6]:-}"
  VERSION_PRERELEASE_NUMBER="${BASH_REMATCH[7]:-}"

  if [[ -n "$VERSION_PRERELEASE_NUMBER" ]] && (( VERSION_PRERELEASE_NUMBER < 1 || VERSION_PRERELEASE_NUMBER > 255 )); then
    return 1
  fi

  return 0
}

base_version() {
  printf '%s.%s.%s\n' "$VERSION_MAJOR" "$VERSION_MINOR" "$VERSION_PATCH"
}

load_current_version() {
  [[ -f "$VERSION_FILE" ]] || die "missing VERSION file at $VERSION_FILE"

  local version
  version="$(tr -d '[:space:]' < "$VERSION_FILE")"
  [[ -n "$version" ]] || die "VERSION file is empty"
  parse_version "$version" || die "invalid version '$version' in VERSION"
  CURRENT_VERSION="$version"
}

cargo_workspace_version() {
  python3 - "$CARGO_TOML" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
lines = path.read_text(encoding="utf-8").splitlines()
in_section = False

for line in lines:
    stripped = line.strip()
    if stripped == "[workspace.package]":
        in_section = True
        continue
    if in_section and stripped.startswith("[") and stripped.endswith("]"):
        break
    if in_section and stripped.startswith("version"):
        _, value = stripped.split("=", 1)
        print(value.strip().strip('"'))
        break
else:
    raise SystemExit("missing [workspace.package] version in Cargo.toml")
PY
}

cargo_lock_sync_error() {
  python3 - "$CARGO_LOCK" "$1" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
expected = sys.argv[2]
targets = {"vapor-daemon", "vapor-providers", "vapor-shared"}
found = {}
current_name = None

for line in path.read_text(encoding="utf-8").splitlines():
    stripped = line.strip()
    if stripped == "[[package]]":
        current_name = None
        continue
    if stripped.startswith("name = "):
        current_name = stripped.split("=", 1)[1].strip().strip('"')
        continue
    if current_name in targets and stripped.startswith("version = "):
        found[current_name] = stripped.split("=", 1)[1].strip().strip('"')

missing = sorted(targets - found.keys())
if missing:
    print(f"missing workspace packages in Cargo.lock: {', '.join(missing)}")
    raise SystemExit(0)

mismatched = {name: value for name, value in found.items() if value != expected}
if mismatched:
    details = ", ".join(f"{name}={value}" for name, value in sorted(mismatched.items()))
    print(f"workspace package versions in Cargo.lock do not match VERSION {expected}: {details}")
PY
}

check_cargo_lock_sync() {
  local version="$1"
  local error_message

  [[ -f "$CARGO_LOCK" ]] || return 0

  error_message="$(cargo_lock_sync_error "$version")"
  if [[ -n "$error_message" ]]; then
    die "$error_message"
  fi
}

sync_cargo_lock() {
  [[ -f "$CARGO_LOCK" ]] || return 0
  # `cargo update --workspace` refreshes only the workspace members'
  # lockfile entries. (`cargo generate-lockfile` would re-resolve every
  # third-party dependency to its newest compatible version, silently
  # bundling a dependency bump into the release-prep commit.)
  cargo update --workspace --manifest-path "$CARGO_TOML" >/dev/null 2>&1
}

sync_cargo_from_version() {
  local version="$1"

  python3 - "$CARGO_TOML" "$version" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
version = sys.argv[2]
lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
in_section = False
updated = False

for index, line in enumerate(lines):
    stripped = line.strip()
    if stripped == "[workspace.package]":
        in_section = True
        continue
    if in_section and stripped.startswith("[") and stripped.endswith("]"):
        break
    if in_section and stripped.startswith("version"):
        new_line = f'version = "{version}"\n'
        if lines[index] != new_line:
            lines[index] = new_line
        updated = True
        break

if not updated:
    raise SystemExit("missing [workspace.package] version in Cargo.toml")

path.write_text("".join(lines), encoding="utf-8")
PY
}

check_sync() {
  local version cargo_version
  load_current_version
  version="$CURRENT_VERSION"
  cargo_version="$(cargo_workspace_version)"

  if [[ "$cargo_version" != "$version" ]]; then
    die "Cargo.toml version '$cargo_version' does not match VERSION '$version'; run ./scripts/version.sh sync"
  fi

  check_cargo_lock_sync "$version"
}

load_current_branch() {
  CURRENT_BRANCH="$(git -C "$ROOT_DIR" branch --show-current 2>/dev/null || true)"
  [[ -n "$CURRENT_BRANCH" ]] || die "release preparation requires a checked-out git branch"
}

ensure_release_branch() {
  load_current_branch

  if [[ "$CURRENT_BRANCH" != "$RELEASE_BRANCH" ]]; then
    die "release preparation must run from '$RELEASE_BRANCH' so pushed tags satisfy the release workflow"
  fi
}

ensure_clean_worktree_except_changelog() {
  local status_line path dirty_paths=()

  while IFS= read -r status_line; do
    [[ -z "$status_line" ]] && continue

    path="${status_line:3}"
    if [[ "$path" == *" -> "* ]]; then
      path="${path##* -> }"
    fi

    if [[ "$path" != "CHANGELOG.md" ]]; then
      dirty_paths+=("$path")
    fi
  done < <(git -C "$ROOT_DIR" status --porcelain=v1 --untracked-files=all)

  if (( ${#dirty_paths[@]} > 0 )); then
    printf '[version] release preparation requires a clean worktree except CHANGELOG.md\n' >&2
    printf '[version] dirty paths: %s\n' "${dirty_paths[*]}" >&2
    exit 1
  fi
}

ensure_changelog_entry_exists() {
  local version="$1"

  if ! python3 - "$CHANGELOG_FILE" "$version" <<'PY'
from pathlib import Path
import re
import sys

path = Path(sys.argv[1])
version = sys.argv[2]
text = path.read_text(encoding="utf-8")

if not re.search(rf"^## \[{re.escape(version)}\]", text, flags=re.MULTILINE):
    raise SystemExit(1)
PY
  then
    die "CHANGELOG.md missing section for [$version]"
  fi
}

ensure_tag_does_not_exist() {
  local version="$1"
  local tag_name="v$version"

  if git -C "$ROOT_DIR" rev-parse -q --verify "refs/tags/$tag_name" >/dev/null 2>&1; then
    die "tag '$tag_name' already exists"
  fi
}

write_version() {
  local version="$1"
  parse_version "$version" || die "invalid version '$version'"
  printf '%s\n' "$version" > "$VERSION_FILE"
}

set_version_and_sync() {
  local version="$1"
  write_version "$version"
  sync_cargo_from_version "$version"
  sync_cargo_lock
  printf '%s\n' "$version"
}

prepare_release_version() {
  local version="$1"
  local release_commit_message push_command
  local staged_paths=(VERSION Cargo.toml CHANGELOG.md)

  load_current_version
  parse_version "$version" || die "invalid version '$version'"

  if [[ "$version" == "$CURRENT_VERSION" ]]; then
    die "VERSION is already '$version'"
  fi

  ensure_release_branch
  ensure_clean_worktree_except_changelog
  ensure_changelog_entry_exists "$version"
  ensure_tag_does_not_exist "$version"

  set_version_and_sync "$version" >/dev/null

  if [[ -f "$CARGO_LOCK" ]]; then
    staged_paths+=(Cargo.lock)
  fi

  git -C "$ROOT_DIR" add "${staged_paths[@]}"

  if git -C "$ROOT_DIR" diff --cached --quiet; then
    die "no release changes were staged"
  fi

  release_commit_message="release: v$version"
  git -C "$ROOT_DIR" commit -m "$release_commit_message"
  git -C "$ROOT_DIR" tag -a "v$version" -m "$release_commit_message"

  push_command="git push origin \"$CURRENT_BRANCH\" --follow-tags"

  printf '[version] prepared %s and tag v%s\n' "$release_commit_message" "$version"
  printf '[version] push when ready: %s\n' "$push_command"
}

print_metadata() {
  local version release_version prerelease=false stable=true tag git_commit_short apple_build_version suffix

  load_current_version
  version="$CURRENT_VERSION"
  release_version="$(base_version)"
  tag="v$version"

  if [[ -n "$VERSION_PRERELEASE_LABEL" ]]; then
    prerelease=true
    stable=false
    case "$VERSION_PRERELEASE_LABEL" in
      alpha) suffix="a" ;;
      beta) suffix="b" ;;
      rc) suffix="fc" ;;
      *) die "unsupported prerelease label '$VERSION_PRERELEASE_LABEL'" ;;
    esac
    apple_build_version="${release_version}${suffix}${VERSION_PRERELEASE_NUMBER}"
  else
    apple_build_version="$release_version"
  fi

  git_commit_short="$(git -C "$ROOT_DIR" rev-parse --short=7 HEAD 2>/dev/null || printf 'unknown')"

  cat <<EOF
VAPOR_VERSION=$version
VAPOR_RELEASE_VERSION=$release_version
VAPOR_VERSION_TAG=$tag
VAPOR_APPLE_BUILD_VERSION=$apple_build_version
VAPOR_VERSION_IS_PRERELEASE=$prerelease
VAPOR_VERSION_IS_STABLE=$stable
VAPOR_PRERELEASE_LABEL=$VERSION_PRERELEASE_LABEL
VAPOR_PRERELEASE_NUMBER=$VERSION_PRERELEASE_NUMBER
VAPOR_GIT_COMMIT_SHORT=$git_commit_short
EOF
}

bump_version() {
  local segment="$1" next_major next_minor next_patch
  load_current_version

  next_major="$VERSION_MAJOR"
  next_minor="$VERSION_MINOR"
  next_patch="$VERSION_PATCH"

  case "$segment" in
    major)
      next_major=$((VERSION_MAJOR + 1))
      next_minor=0
      next_patch=0
      ;;
    minor)
      next_minor=$((VERSION_MINOR + 1))
      next_patch=0
      ;;
    patch)
      next_patch=$((VERSION_PATCH + 1))
      ;;
    *)
      die "unsupported bump segment '$segment'; expected major, minor, or patch"
      ;;
  esac

  printf '%s\n' "${next_major}.${next_minor}.${next_patch}"
}

set_prerelease_version() {
  local label="$1" number="${2:-}"
  local base next_number

  load_current_version
  base="$(base_version)"

  case "$label" in
    alpha|beta|rc) ;;
    *) die "unsupported prerelease label '$label'; expected alpha, beta, or rc" ;;
  esac

  if [[ -n "$number" ]]; then
    [[ "$number" =~ ^[0-9]+$ ]] || die "prerelease number must be numeric"
    next_number="$number"
  elif [[ "$VERSION_PRERELEASE_LABEL" == "$label" && -n "$VERSION_PRERELEASE_NUMBER" ]]; then
    next_number=$((VERSION_PRERELEASE_NUMBER + 1))
  else
    next_number=1
  fi

  printf '%s\n' "$base-$label.$next_number"
}

release_version() {
  load_current_version
  printf '%s\n' "$(base_version)"
}

COMMAND="${1:-current}"

case "$COMMAND" in
  current)
    load_current_version
    printf '%s\n' "$CURRENT_VERSION"
    ;;
  metadata)
    print_metadata
    ;;
  check-sync)
    check_sync
    ;;
  sync)
    load_current_version
    sync_cargo_from_version "$CURRENT_VERSION"
    sync_cargo_lock
    printf '%s\n' "$CURRENT_VERSION"
    ;;
  set)
    [[ $# -eq 2 ]] || die "usage: ./scripts/version.sh set <version>"
    prepare_release_version "$2"
    ;;
  bump)
    [[ $# -eq 2 ]] || die "usage: ./scripts/version.sh bump <major|minor|patch>"
    prepare_release_version "$(bump_version "$2")"
    ;;
  prerelease)
    [[ $# -ge 2 && $# -le 3 ]] || die "usage: ./scripts/version.sh prerelease <alpha|beta|rc> [number]"
    prepare_release_version "$(set_prerelease_version "$2" "${3:-}")"
    ;;
  release)
    [[ $# -eq 1 ]] || die "usage: ./scripts/version.sh release"
    prepare_release_version "$(release_version)"
    ;;
  help|-h|--help)
    usage
    ;;
  *)
    usage
    exit 1
    ;;
esac
