#!/usr/bin/env bash
# The release gate: everything a release must pass, run by hand on the
# maintainer's machine before the version bump is committed. Format,
# lint, the Tier 1 suite, and the e2e suite once per provider the
# harness knows. The Google Drive leg needs the dedicated test account
# signed in (`vapor auth login gdrive`) and VAPOR_GDRIVE_CLIENT_ID set;
# it never runs in CI and never against a personal account. A missing
# leg fails the gate: a release is not good to go until every provider
# has run. Process: docs/operations/release-process.md.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export VAPOR_ENV="dev"

step() {
  echo
  echo "[release-gate] $1"
}

step "format"
"$ROOT_DIR/scripts/format.sh"
step "lint"
"$ROOT_DIR/scripts/lint.sh"
step "test (Tier 1)"
"$ROOT_DIR/scripts/test.sh"

step "building the e2e harness"
"$ROOT_DIR/scripts/e2e.sh" --list >/dev/null
providers="$("$ROOT_DIR/scripts/e2e.sh" --providers)"

for provider in $providers; do
  if [[ "$provider" == "gdrive" ]]; then
    if [[ -z "${VAPOR_GDRIVE_CLIENT_ID:-}" ]]; then
      echo "[release-gate] the Google Drive leg needs VAPOR_GDRIVE_CLIENT_ID (and the test account signed in with 'vapor auth login gdrive')"
      exit 1
    fi
  fi
  step "e2e --provider $provider"
  "$ROOT_DIR/scripts/e2e.sh" --provider "$provider"
done

echo
echo "[release-gate] every gate passed for providers: $(echo $providers | tr '\n' ' ')"
