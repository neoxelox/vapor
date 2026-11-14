#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

fail() {
  echo "[notes-test] FAIL: $1"
  exit 1
}

# The script reads the CHANGELOG.md next to its own scripts/ directory,
# so a copy in a fixture reads the fixture's changelog.
mkdir -p "$fixture/scripts"
cp "$ROOT_DIR/scripts/notes.sh" "$fixture/scripts/notes.sh"

cat >"$fixture/CHANGELOG.md" <<'CHANGELOG'
# Changelog

## [Unreleased]

- Not released yet.

## [0.3.0] - 2026-03-10

### Added

- A short entry.

## [0.2.0] - 2026-02-01

- An older entry.
CHANGELOG

notes="$(bash "$fixture/scripts/notes.sh" 0.3.0 https://example.com/CHANGELOG.md)"
[[ "$notes" == "## Release 0.3.0"* ]] || fail "the body opens with the release heading"
[[ "$notes" == *"Apple silicon"* ]] || fail "the body says what the Mac app runs on"
[[ "$notes" == *"- A short entry."* ]] || fail "the section of the version is in the body"
[[ "$notes" != *"An older entry"* && "$notes" != *"Not released yet"* ]] ||
  fail "no other section is in the body"
[[ "$notes" != *"The list continues"* ]] || fail "a short section is not cut"

if bash "$fixture/scripts/notes.sh" 9.9.9 >/dev/null 2>&1; then
  fail "a version with no section must fail"
fi

# A section over GitHub's limit for a release body.
{
  printf '# Changelog\n\n## [Unreleased]\n\n## [0.4.0] - 2026-04-01\n\n'
  for index in $(seq 1 2000); do
    printf -- '- Entry %04d: a line of text that is long enough to make the section pass the limit of a release body.\n' "$index"
  done
  printf '\n## [0.3.0] - 2026-03-10\n\n- An older entry.\n'
} >"$fixture/CHANGELOG.md"

bash "$fixture/scripts/notes.sh" 0.4.0 https://example.com/CHANGELOG.md >"$fixture/notes.md"
size="$(wc -c <"$fixture/notes.md" | tr -d '[:space:]')"
[[ "$size" -le 125000 ]] || fail "a cut body fits GitHub's limit: got $size bytes"
[[ "$size" -ge 100000 ]] || fail "a cut body keeps what fits: got $size bytes"
[[ "$(tail -n 1 "$fixture/notes.md")" == "The list continues in [CHANGELOG.md](https://example.com/CHANGELOG.md)." ]] ||
  fail "a cut body ends with the pointer to the changelog"
last_entry="$(grep -- '^- Entry' "$fixture/notes.md" | tail -n 1)"
[[ "$last_entry" == *"release body." ]] || fail "the cut is at a line end: got '$last_entry'"
grep -q "An older entry" "$fixture/notes.md" && fail "the cut body holds another section"

echo "[notes-test] OK"
