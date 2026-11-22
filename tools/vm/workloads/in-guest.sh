#!/bin/bash
# in-guest: runs a shell script in the manual guest of this checkout, as
# the user of the guest, with the tools' environment. Runs on the host.
#
#   in-guest.sh 'python3 "$R/tools/vt.py" st'
#   VT_PROFILE=fs in-guest.sh '...'      judge one profile of several
set -euo pipefail
GUEST="$(tart list --format json | python3 -c 'import json,sys; print(next(v["Name"] for v in json.load(sys.stdin) if "-manual" in v["Name"] and v["State"]=="running"))')"
PROFILE="${VT_PROFILE-}"
exec tart exec "$GUEST" /bin/zsh -c "export PATH=/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin; export R='/Volumes/My Shared Files/results'; export VT_PROFILE='$PROFILE'; [ -n \"\$VT_PROFILE\" ] && export VT_DB=\$HOME/.vapor/state/profiles/\$VT_PROFILE/vapor.sqlite; $*"
