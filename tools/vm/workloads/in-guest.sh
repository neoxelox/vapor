#!/bin/bash
# in-guest: runs a shell script in the manual guest of this checkout, as
# the user of the guest, with the tools' environment. Runs on the host.
#
#   in-guest.sh 'python3 "$R/tools/vt.py" st'
#   VT_PROFILE=fs in-guest.sh '...'      judge one profile of several
#   GUEST=<name> in-guest.sh '...'       when two manual guests run
#   ... | STDIN=1 in-guest.sh '...'      give the command the host's stdin
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
# The harness names a guest `vapor-vm-<tag>-manual...`, and the tag is
# the first four bytes of the SHA-256 of the checkout's path.
TAG="$(printf '%s' "$ROOT" | shasum -a 256 | cut -c1-8)"
GUEST="${GUEST:-$(tart list --format json | TAG="$TAG" python3 -c '
import json, os, sys
prefix = "vapor-vm-" + os.environ["TAG"] + "-manual"
names = [v["Name"] for v in json.load(sys.stdin) if v["Name"].startswith(prefix) and v["State"] == "running"]
if len(names) != 1:
    sys.exit(f"in-guest: {len(names)} manual guests of this checkout run; start one, or name one with GUEST=")
print(names[0])')}"
PROFILE="${VT_PROFILE-}"
# A secret goes to a command of the guest on its stdin, never in its
# arguments.
STDIN_FLAG=()
[ -n "${STDIN-}" ] && STDIN_FLAG=(-i)
exec tart exec ${STDIN_FLAG[@]+"${STDIN_FLAG[@]}"} "$GUEST" /bin/zsh -c "export PATH=/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin; export R='/Volumes/My Shared Files/results'; export VT_PROFILE='$PROFILE'; [ -n \"\$VT_PROFILE\" ] && export VT_DB=\$HOME/.vapor/state/profiles/\$VT_PROFILE/vapor.sqlite; $*"
