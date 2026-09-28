#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

fail() {
  echo "[dotenv-test] FAIL: $1"
  exit 1
}

# Each case runs in a subshell so exports never leak between cases or
# into the caller.
run_case() {
  # shellcheck disable=SC2016 # expanded by the inner shell
  env -u VAPOR_GDRIVE_CLIENT_ID -u VAPOR_GDRIVE_CLIENT_SECRET "$@" bash -c '
    set -euo pipefail
    source "$0/scripts/dotenv.sh"
    vapor_load_build_env "$1"
    printf "%s|%s|%s\n" "${VAPOR_GDRIVE_CLIENT_ID:-}" "${VAPOR_GDRIVE_CLIENT_SECRET:-}" "${VAPOR_DIR:-}"
  ' "$ROOT_DIR" "$fixture"
}

cat >"$fixture/.env" <<'DOTENV'
# a comment
VAPOR_DIR=/should/not/load
export VAPOR_GDRIVE_CLIENT_ID="123-abc.apps.googleusercontent.com"
VAPOR_GDRIVE_CLIENT_SECRET='GOCSPX-fixture'
DOTENV

got="$(run_case env -u VAPOR_DIR)"
[[ "$got" == "123-abc.apps.googleusercontent.com|GOCSPX-fixture|" ]] ||
  fail "quoted and export-prefixed values load, other keys do not: got '$got'"

got="$(run_case env -u VAPOR_DIR VAPOR_GDRIVE_CLIENT_ID=from-ci)"
[[ "$got" == "from-ci|GOCSPX-fixture|" ]] ||
  fail "an exported value wins over the file: got '$got'"

rm "$fixture/.env"
got="$(run_case env -u VAPOR_DIR)"
[[ "$got" == "||" ]] || fail "no .env is a no-op: got '$got'"

echo "[dotenv-test] OK"
