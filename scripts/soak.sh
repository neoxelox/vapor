#!/usr/bin/env bash
# Soak verification (Tier S): hours of seeded file churn on both sides
# of a real `vapor` + `vapord` daemon, fault injection, and a model
# that checks after every phase that nothing was lost and both trees
# converged. Everything lives under `<repo>/.vapor/e2e/soak-<id>/`.
#
# The driver lives in `tools/soak` (crate `vapor-soak`); this wrapper
# only sets the project environment and forwards arguments.
#
# Usage:
#   ./scripts/soak.sh [run options]          run a soak (see `vapor-soak run --help`)
#   ./scripts/soak.sh --status <status.json> one-line status of a run
#   ./scripts/soak.sh --verify <sandbox>     re-run the oracle on a preserved sandbox
#
# Run options: --duration 2h  --seed 7  --mode two-way|pull-only|push-only
# --load mixed|trickle|coding|bulk|large  --faults none|crash|all|<list>
# --throttle static|walk  --daemon vapor-run|vapord  --cloud-image-mb 256
# --cloud-image-fs apfs|apfs-case-sensitive|exfat  --release  --keep
# --continue-on-violation  --converge-deadline 20m  --skip-build
#
# Process doc: docs/development/soak-testing.md
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export VAPOR_ENV="dev"
# The driver scopes the daemon to its own sandbox home; VAPOR_DIR from
# the invoking shell must never leak into it.
unset VAPOR_DIR

SOAK_BIN="${CARGO_TARGET_DIR:-$ROOT_DIR/target}/debug/vapor-soak"

subcommand="run"
args=()
skip_build=0
for arg in "$@"; do
  case "$arg" in
    --status) subcommand="status" ;;
    --verify) subcommand="verify" ;;
    --skip-build) skip_build=1; args+=("$arg") ;;
    *) args+=("$arg") ;;
  esac
done
# `status` and `verify` take no build flag; drop it for them.
if [[ "$subcommand" != "run" ]]; then
  filtered=()
  for arg in ${args[@]+"${args[@]}"}; do
    [[ "$arg" == "--skip-build" ]] || filtered+=("$arg")
  done
  args=(${filtered[@]+"${filtered[@]}"})
  skip_build=1
fi

if [[ "$skip_build" -eq 0 || ! -x "$SOAK_BIN" ]]; then
  cargo build --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p vapor-soak
fi

exec "$SOAK_BIN" --repo-root "$ROOT_DIR" "$subcommand" ${args[@]+"${args[@]}"}
