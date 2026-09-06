//! Vapor's soak driver (Tier S).
//!
//! Puts the real `vapor` + `vapord` in a sandbox, generates hours of
//! seeded file churn on the local root and on the cloud root, injects
//! faults, and checks after every phase that nothing the workload wrote
//! was lost and that both trees converged. Everything it knows is
//! written to `soak-status.json` (every few seconds) and
//! `soak-report.json` (at the end, or at the first violation), so an
//! agent can watch a run without touching the daemon.
//!
//! Process doc: `docs/development/soak-testing.md`.

pub mod driver;
pub mod faults;
pub mod health;
pub mod model;
pub mod report;
pub mod rng;
pub mod throttle_file;
pub mod workload;

pub use vapor_e2e::Failure;
