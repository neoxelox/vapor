#!/usr/bin/env bash
# Prints the GitHub Release body for a version: one line on what the
# Mac app runs on, then the version's CHANGELOG.md section. GitHub
# refuses a body over 125 000 characters, so a longer section is cut at
# a line end and closed with a pointer to the full changelog.
#
# Usage: scripts/notes.sh <version> [changelog-url]
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-}"
CHANGELOG_URL="${2:-}"

if [[ -z "$VERSION" ]]; then
  echo "Usage: scripts/notes.sh <version> [changelog-url]" >&2
  exit 1
fi

python3 - "$ROOT_DIR/CHANGELOG.md" "$VERSION" "$CHANGELOG_URL" <<'PY'
from pathlib import Path
import re
import sys

path, version, url = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
# Under GitHub's 125 000, with room for the closing pointer. Counted in
# bytes, which is the stricter reading of "characters".
LIMIT = 120_000

text = path.read_text(encoding="utf-8")
match = re.search(
    rf"^## \[{re.escape(version)}\][^\n]*\n(.*?)(?=^## \[|\Z)",
    text,
    re.MULTILINE | re.DOTALL,
)
if not match:
    raise SystemExit(f"[notes] CHANGELOG.md has no section for [{version}]")

head = (
    f"## Release {version}\n\n"
    "The Mac app is built for Apple silicon and needs macOS 26 or later.\n\n"
)
body = match.group(1).strip() + "\n"
target = f"[CHANGELOG.md]({url})" if url else "CHANGELOG.md"

if len((head + body).encode("utf-8")) > LIMIT:
    kept, size = [], len(head.encode("utf-8"))
    for line in body.splitlines(keepends=True):
        size += len(line.encode("utf-8"))
        if size > LIMIT:
            break
        kept.append(line)
    body = "".join(kept).rstrip() + f"\n\nThe list continues in {target}.\n"

sys.stdout.write(head + body)
PY
