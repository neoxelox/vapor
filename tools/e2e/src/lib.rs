//! Vapor's end-to-end verification harness (Tier E2E).
//!
//! Drives the real `vapor` and `vapord` binaries black-box, through the
//! CLI only, inside a disposable sandbox under the repository-local
//! `.vapor/e2e/` directory. Every scenario gets its own runtime home,
//! local root, and cloud root; observes the product only through its
//! own surfaces (`--json` output, exit codes, the daemon log, read-only
//! state-DB queries, the two trees on disk); and ends with the tree
//! oracle and the log-hygiene check unless it opts out with a reason.
//!
//! The library half is shared with the soak driver; the binary half
//! (`main.rs`) is the `vapor-e2e` command `scripts/e2e.sh` wraps.
//!
//! Process doc: `docs/development/e2e-verification.md`.

pub mod cli;
pub mod daemon;
pub mod db;
pub mod diskimage;
pub mod failure;
pub mod host;
pub mod logs;
pub mod oracle;
pub mod report;
pub mod runner;
pub mod sandbox;
pub mod scenario;
pub mod scenarios;
pub mod wait;

pub use failure::Failure;
