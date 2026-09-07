#!/usr/bin/env bash
# End-to-end verification (Tier E2E): builds the `vapor-e2e` harness
# and the product binaries, then drives the real `vapor` + `vapord`
# black-box inside a disposable sandbox under `<repo>/.vapor/e2e/`.
#
# The harness itself lives in `tools/e2e` (crate `vapor-e2e`); this
# wrapper only sets the project environment and forwards arguments.
#
# Usage:
#   ./scripts/e2e.sh [run options]        run the scenario suite
#   ./scripts/e2e.sh --sandbox            provision a manual sandbox
#   ./scripts/e2e.sh --sandbox-stop       stop and remove manual sandboxes
#   ./scripts/e2e.sh --list               list scenarios
#   ./scripts/e2e.sh -- <subcommand> ...  any other vapor-e2e subcommand
#
# Run options (see `vapor-e2e run --help`): --only S16,S23  --keep
# --skip-build  --full  --provider filesystem|gdrive  --daemon
# vapor-run|vapord  --json <path>
#
# Process doc: docs/development/e2e-verification.md
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export VAPOR_ENV="dev"
# The harness scopes every daemon to its own sandbox home; VAPOR_DIR
# from the invoking shell must never leak into a scenario.
unset VAPOR_DIR

E2E_BIN="${CARGO_TARGET_DIR:-$ROOT_DIR/target}/debug/vapor-e2e"
if [[ "$(uname -s 2>/dev/null || echo unknown)" == MINGW* || "$(uname -s 2>/dev/null || echo unknown)" == MSYS* ]]; then
  E2E_BIN="$E2E_BIN.exe"
fi

subcommand="run"
args=()
skip_harness_build=0
for arg in "$@"; do
  case "$arg" in
    --sandbox) subcommand="sandbox" ;;
    --sandbox-stop) subcommand="sandbox-stop" ;;
    --list) subcommand="list" ;;
    --skip-build) skip_harness_build=1; args+=("$arg") ;;
    --) subcommand="" ;;
    *) args+=("$arg") ;;
  esac
done

if [[ "$skip_harness_build" -eq 0 || ! -x "$E2E_BIN" ]]; then
  cargo build --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p vapor-e2e
fi

if [[ -n "$subcommand" ]]; then
  exec "$E2E_BIN" --repo-root "$ROOT_DIR" "$subcommand" ${args[@]+"${args[@]}"}
fi
exec "$E2E_BIN" --repo-root "$ROOT_DIR" ${args[@]+"${args[@]}"}
