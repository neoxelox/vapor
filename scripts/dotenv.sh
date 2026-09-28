#!/usr/bin/env bash
# Sourced by the scripts that compile `vapor` and `vapord`
# (`scripts/rust/build.sh`, `apps/macos/scripts/package.sh`); not an
# entrypoint.
#
# The Google Drive OAuth client is compiled into both binaries from the
# build environment. `vapor_load_build_env` reads it from the
# repository's gitignored `.env` so a local build carries the same
# client as a release. Only these two keys are read, and a value already
# exported wins over the file, so CI's environment is never overridden.

VAPOR_BUILD_ENV_KEYS=(VAPOR_GDRIVE_CLIENT_ID VAPOR_GDRIVE_CLIENT_SECRET)

vapor_load_build_env() {
  local root="$1"
  local file="$root/.env"
  local key line value
  [[ -f "$file" ]] || return 0
  for key in "${VAPOR_BUILD_ENV_KEYS[@]}"; do
    [[ -n "${!key:-}" ]] && continue
    line="$(grep -E "^[[:space:]]*(export[[:space:]]+)?${key}=" "$file" | tail -n 1 || true)"
    [[ -n "$line" ]] || continue
    value="${line#*=}"
    value="${value%$'\r'}"
    value="${value#\"}"
    value="${value%\"}"
    value="${value#\'}"
    value="${value%\'}"
    [[ -n "$value" ]] && export "$key=$value"
  done
  return 0
}

# One line saying which client the build compiles in. Never prints the
# secret.
vapor_report_build_env() {
  local tag="$1"
  if [[ -n "${VAPOR_GDRIVE_CLIENT_ID:-}" ]]; then
    local with_secret="without a client secret"
    [[ -n "${VAPOR_GDRIVE_CLIENT_SECRET:-}" ]] && with_secret="with a client secret"
    echo "[$tag] Google Drive OAuth client: $VAPOR_GDRIVE_CLIENT_ID ($with_secret)"
  else
    echo "[$tag] Google Drive OAuth client: none (Google Drive sign-in needs VAPOR_GDRIVE_CLIENT_ID at run time)"
  fi
}
