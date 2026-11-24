# Known-Good Local Baseline (Nov 2026)

- macOS: `27.2.1`
- Rust: `rustc 1.100.0 (0ef1bc386 2026-11-09)`
- Cargo: `cargo 1.100.0 (3acc79c0c 2026-11-05)`
- Swift driver: `1.168.6`
- Swift: `6.4.2 (swiftlang-6.4.2.1.9 clang-2100.4.1.1)`
- Swift target: `arm64-apple-macosx27.2.0`

## Keep Rust in step with CI

CI installs the latest stable Rust at each run, and
`rust-toolchain.toml` names the channel, not a version. A local
toolchain that is some months older passes `./scripts/lint.sh` on code
that the newer clippy rejects, and the difference costs a CI run.

- Check: `rustup check`. The line for `stable` says `up to date` or
  `Update available`.
- Update: `rustup update stable`.
- `./scripts/lint.sh` runs the check itself and prints one line when a
  newer stable is out. It does not fail on it: the lint of the old
  toolchain is still worth its result.

Tier VM (`docs/development/vm-verification.md`) was verified in
November 2026 with:

- Host: macOS `27.2.1` on Apple silicon. A macOS 27 guest needs a
  macOS 27 host.
- Tart: `2.42.0` (`2.33.0` is the minimum for macOS 27 guests)
- Guest image: `macos-golden-gate-base`, macOS `27.2`, pinned by digest
  in `tools/vm/src/tart.rs`

This baseline is a known-good contributor reference, not a hard pin.
Project policy remains latest stable by default unless a documented blocker requires temporary pinning.
