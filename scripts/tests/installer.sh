#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
POSTINSTALL="$ROOT_DIR/apps/macos/scripts/installer/postinstall"

fail() {
  echo "[installer-test] FAIL: $1"
  exit 1
}

# The script takes the volume it installs to as its third argument, so
# a directory stands in for the disk and nothing here needs root.
volume="$(mktemp -d)"
trap 'rm -rf "$volume"' EXIT
link="$volume/usr/local/bin/vapor"
app_cli="$volume/Applications/Vapor.app/Contents/Helpers/vapor"

run_postinstall() {
  sh "$POSTINSTALL" package "$volume" "$volume"
}

# A fresh system has no /usr/local/bin: the script makes it and links
# the CLI of the app there.
run_postinstall >/dev/null
[[ -L "$link" ]] || fail "a fresh install leaves no link"
[[ "$(readlink "$link")" == "$app_cli" ]] || fail "the link points at $(readlink "$link")"

# An update runs the script again over its own link.
run_postinstall >/dev/null
[[ "$(readlink "$link")" == "$app_cli" ]] || fail "a second install broke the link"

# A link of an install in another place is replaced: the app is in
# /Applications now.
rm "$link"
ln -s "/somewhere/else/Vapor.app/Contents/Helpers/vapor" "$link"
run_postinstall >/dev/null
[[ "$(readlink "$link")" == "$app_cli" ]] || fail "a link of an earlier install stays"

# A link that leads nowhere is replaced, whoever made it.
rm "$link"
ln -s "$volume/gone/vapor" "$link"
run_postinstall >/dev/null
[[ "$(readlink "$link")" == "$app_cli" ]] || fail "a dead link stays"

# Another program with the same name stays, as a file and as a link,
# and the install still succeeds and says where the CLI is.
rm "$link"
echo "another program" >"$link"
said="$(run_postinstall)" || fail "the install fails next to another program"
[[ "$(cat "$link")" == "another program" ]] || fail "another program was overwritten"
[[ "$said" == *"$app_cli"* ]] || fail "the script does not say where the CLI is: $said"

rm "$link"
echo "another program" >"$volume/other-vapor"
ln -s "$volume/other-vapor" "$link"
run_postinstall >/dev/null || fail "the install fails next to a link of another program"
[[ "$(readlink "$link")" == "$volume/other-vapor" ]] || fail "a link of another program was replaced"

echo "[installer-test] OK"
