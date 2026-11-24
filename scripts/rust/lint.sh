#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
export VAPOR_ENV="${VAPOR_ENV:-dev}"
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

"$ROOT_DIR/scripts/version.sh" check-sync >/dev/null

MANIFEST="$ROOT_DIR/Cargo.toml"

# CI installs the latest stable Rust at each run. A local toolchain
# that is older passes clippy on code that the newer one rejects, and
# the difference then costs a CI run. Say so before the lint and
# after it.
toolchain_notice=""
if command -v rustup >/dev/null 2>&1; then
  if rustup check 2>/dev/null | grep -q '^stable.*Update available'; then
    toolchain_notice="[rust-lint] A newer stable Rust is out, and CI lints with it. Run: rustup update stable"
    echo "$toolchain_notice"
  fi
fi

echo "[rust-lint] cargo clippy for ${MANIFEST}"
cargo clippy --manifest-path "$MANIFEST" --all-targets --all-features -- -D warnings

# The dependency check (deny.toml): RustSec advisories and yanked
# crates, licenses, and crate sources. The advisory database comes from
# the network, so this step needs a connection.
if ! cargo deny --version >/dev/null 2>&1; then
  echo "[rust-lint] 'cargo deny' is required but not installed."
  echo "[rust-lint] Install it with: cargo install cargo-deny --locked"
  exit 1
fi
echo "[rust-lint] cargo deny check for ${MANIFEST}"
cargo deny --manifest-path "$MANIFEST" check

if [[ -n "$toolchain_notice" ]]; then
  echo "$toolchain_notice"
fi
