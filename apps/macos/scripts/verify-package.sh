#!/usr/bin/env bash
# Checks the installer that package.sh wrote.
#
#   verify-package.sh [package]            what is inside it
#   verify-package.sh [package] --install  and what it leaves on the host
#
# The install needs root and changes the host: it puts Vapor.app in
# /Applications and a link in /usr/local/bin. It is for a disposable
# GitHub Actions runner, and it refuses to run anywhere else. It never
# opens the app.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
PACKAGE="$ROOT_DIR/dist/Vapor-macos-arm64.pkg"
INSTALL=false
for argument in "$@"; do
  case "$argument" in
    --install) INSTALL=true ;;
    *) PACKAGE="$argument" ;;
  esac
done

fail() {
  echo "[verify-package] $1"
  exit 1
}

[[ -f "$PACKAGE" ]] || fail "no installer at $PACKAGE"

payload="$(pkgutil --payload-files "$PACKAGE")"
for executable in \
  ./Applications/Vapor.app/Contents/MacOS/Vapor \
  ./Applications/Vapor.app/Contents/MacOS/vapord \
  ./Applications/Vapor.app/Contents/Helpers/vapor; do
  grep -qxF "$executable" <<<"$payload" || fail "the installer does not hold $executable"
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
pkgutil --expand "$PACKAGE" "$work/expanded"
[[ -x "$work/expanded/Vapor.pkg/Scripts/postinstall" ]] \
  || fail "the installer has no postinstall script, so nothing links the CLI"
grep -q 'hostArchitectures="arm64"' "$work/expanded/Distribution" \
  || fail "the installer does not refuse an Intel Mac"
# A relocatable bundle is installed over any copy of the app that the
# machine has, wherever it is, and not into /Applications.
if ! grep -q 'relocatable="false"' "$work/expanded/Vapor.pkg/PackageInfo" \
  || grep -q "<relocate>" "$work/expanded/Vapor.pkg/PackageInfo"; then
  fail "the app bundle is relocatable"
fi

# An unsigned installer is what a prerelease ships; the line says which
# one this is.
signature="$( (pkgutil --check-signature "$PACKAGE" 2>&1 || true) | sed -n 's/^ *Status: *//p' | head -n 1)"
echo "[verify-package] $PACKAGE holds the app, the daemon, and the CLI (signature: ${signature:-unknown})"

if [[ "$INSTALL" != "true" ]]; then
  exit 0
fi

# `GITHUB_ACTIONS`, not `CI`: a developer shell or an agent harness can
# carry `CI`, and this would replace the Vapor that the machine runs.
[[ "${GITHUB_ACTIONS:-}" == "true" ]] \
  || fail "--install changes the host; it runs on a GitHub Actions runner only"

sudo installer -pkg "$PACKAGE" -target /

app="/Applications/Vapor.app"
link="/usr/local/bin/vapor"
for executable in "$app/Contents/MacOS/Vapor" "$app/Contents/MacOS/vapord" "$app/Contents/Helpers/vapor"; do
  [[ -x "$executable" ]] || fail "the install left no $executable"
done
[[ -L "$link" ]] || fail "the install left no link at $link"
[[ "$(readlink "$link")" == "$app/Contents/Helpers/vapor" ]] \
  || fail "$link points at $(readlink "$link")"

# The link is on the PATH of a new shell, and the CLI behind it is this
# version.
resolved="$(PATH="/usr/local/bin:/usr/bin:/bin" command -v vapor)"
[[ "$resolved" == "$link" ]] || fail "vapor on the PATH is $resolved"
expected_version="$(tr -d '[:space:]' <"$ROOT_DIR/VERSION")"
installed_version="$("$link" version)"
[[ "$installed_version" == *"$expected_version"* ]] \
  || fail "vapor version says '$installed_version', expected $expected_version"

# Started through the link, the CLI finds the daemon inside the app.
daemon="$(VAPOR_DIR="$work/runtime" "$link" doctor --json |
  python3 -c 'import json,sys; print(next(c["detail"] for c in json.load(sys.stdin)["checks"] if c["name"] == "vapord-binary"))' || true)"
[[ "$daemon" == *"$app/Contents/MacOS/vapord"* ]] \
  || fail "through the link the CLI does not find the daemon in the app: ${daemon:-no answer}"

echo "[verify-package] installed: $link -> $(readlink "$link"), $installed_version"
