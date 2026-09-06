//! IPC channel between Vapor surfaces (`vapor` CLI, macOS app, future
//! Windows / Linux apps) and the daemon (`vapord`).
//!
//! Authoritative reference: `docs/architecture/ipc-contracts.md`.
//!
//! Wire format: Vapor's own tagged JSON envelopes (`{kind, payload}`
//! requests, `{outcome, value}` responses; not JSON-RPC) framed with a
//! `u32` little-endian length prefix. The same handshake / skew
//! discipline applies on every transport (UDS on Unix, named pipe on
//! Windows once it ships). Pre-GA the schema-skew tolerance is
//! `|app - daemon| <= 1`.
//!
//! This crate ships:
//!
//! - The wire-format types (`Hello`, `HelloAck`, `Request`, `Response`,
//!   `Error`).
//! - A length-prefixed framing codec.
//! - A Unix-domain-socket transport (server + client) on Unix; Windows
//!   gets a stub that returns `Unsupported` until the named-pipe
//!   transport lands.
//! - A `Service` trait the daemon implements to dispatch requests.
//! - The skew-matrix integration tests exercising every supported
//!   `app-N ↔ daemon-M` pair plus the `|N - M| = 2` negative case.

// `deny` rather than `forbid`: the Unix transport needs exactly one
// tightly-scoped FFI call (`geteuid`) to verify socket ownership before
// connecting. Every other module stays unsafe-free.
#![deny(unsafe_code)]

pub mod client;
pub mod framing;
pub mod protocol;
pub mod server;
pub mod transport;

pub use client::{Client, ClientError};
pub use framing::{FrameError, read_frame, write_frame};
pub use protocol::{
    AckResponse, DiagnosticsResponse, ErrorBody, Hello, HelloAck, IncompatibleVersion,
    IntentDiagnostic, Method, ProfileStatus, Request, ResourceBudgetStatus, Response, ResponseBody,
    StatusResponse, TimelineEntry, TimelineResponse, daemon_supported_versions,
};
pub use server::{ServeError, Service, serve_connection};
pub use transport::{ListenerHandle, TransportError};
