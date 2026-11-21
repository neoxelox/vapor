# Known-Good Local Baseline (Mar 2026)

- macOS: `26.3` (Tahoe)
- Rust: `rustc 1.93.1 (01f6ddf75 2026-02-11)`
- Cargo: `cargo 1.93.1 (083ac5135 2025-12-15)`
- Swift driver: `1.127.15`
- Swift: `6.2.4 (swiftlang-6.2.4.1.4 clang-1700.6.4.2)`
- Swift target: `arm64-apple-macosx26.0`

Tier VM (`docs/development/vm-verification.md`) was verified in
November 2026 with:

- Host: macOS `27.2.1` on Apple silicon. A macOS 27 guest needs a
  macOS 27 host.
- Tart: `2.42.0` (`2.33.0` is the minimum for macOS 27 guests)
- Guest image: `macos-golden-gate-base`, macOS `27.2`, pinned by digest
  in `tools/vm/src/tart.rs`

This baseline is a known-good contributor reference, not a hard pin.
Project policy remains latest stable by default unless a documented blocker requires temporary pinning.
