//! Vapor's virtual machine verification harness (Tier VM).
//!
//! Installs the packaged app in a macOS guest made with Tart and acts
//! on it as a user does: a click on the menu bar item, a look at a
//! window, a restart of the Mac. A contributor's Mac holds nothing of
//! a run beyond the repository-local `.vapor/vm/` directory and the
//! clones in Tart's own store, which the run deletes.
//!
//! The binary half (`main.rs`) is the `vapor-vm` command that
//! `scripts/vm.sh` wraps.
//!
//! Process doc: `docs/development/vm-verification.md`.

pub mod guest;
pub mod runner;
pub mod scenario;
pub mod scenarios;
pub mod tart;
pub mod ui;
