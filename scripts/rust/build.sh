#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MODE="${1:-build}"

if [[ "$MODE" != "build" && "$MODE" != "package" ]]; then
  echo "Usage: scripts/rust/build.sh [build|package]"
  exit 1
fi

export VAPOR_DIR="${VAPOR_DIR:-$ROOT_DIR/.vapor}"
if [[ "$MODE" == "package" ]]; then
  export VAPOR_ENV="${VAPOR_ENV:-prod}"
else
  export VAPOR_ENV="${VAPOR_ENV:-dev}"
fi
mkdir -p "$VAPOR_DIR/logs" "$VAPOR_DIR/state"

"$ROOT_DIR/scripts/version.sh" check-sync >/dev/null

if [[ -f "$ROOT_DIR/Cargo.toml" ]]; then
  # shellcheck source=scripts/dotenv.sh
  source "$ROOT_DIR/scripts/dotenv.sh"
  vapor_load_build_env "$ROOT_DIR"
  vapor_report_build_env "rust-build"
  echo "[rust-build] cargo build --workspace --release"
  cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --workspace --release

  # The shipping binaries stay small: each one under 10 MiB, stripped.
  # A dependency that doubles one of them fails here, and not after a
  # release that every install has to download. Windows ships no
  # surface yet and links SQLite from source, so its sizes are printed
  # and not judged.
  size_limit=$((10 * 1024 * 1024))
  case "$(uname -s 2>/dev/null || echo unknown)" in
    MINGW* | MSYS* | CYGWIN*) size_is_judged=0 ;;
    *) size_is_judged=1 ;;
  esac
  target_dir="${CARGO_TARGET_DIR:-$ROOT_DIR/target}/release"
  for binary in vapor vapord; do
    file="$target_dir/$binary"
    [[ -f "$file" ]] || file="$file.exe"
    if [[ ! -f "$file" ]]; then
      echo "[rust-build] $binary was not built: nothing at $target_dir/$binary" >&2
      exit 1
    fi
    size="$(wc -c <"$file" | tr -d '[:space:]')"
    echo "[rust-build] $binary is $((size / 1024)) KiB"
    if [[ "$size_is_judged" -eq 1 && "$size" -gt "$size_limit" ]]; then
      echo "[rust-build] $binary is over the limit of $((size_limit / 1024)) KiB." >&2
      echo "[rust-build] Find what grew it (cargo tree, cargo bloat) before raising the limit." >&2
      exit 1
    fi
  done

  exit 0
fi

echo "[rust-build] No Cargo.toml found. Skipping Rust build."
