//! IPC channel between Vapor surfaces (`vapor` CLI, macOS app, future
//! Windows / Linux apps) and the daemon (`vapord`).
//!
//! Authoritative reference: `docs/architecture/ipc-contracts.md`.
//! Tasks: `docs/tasks/core.md` Phase C5.
//!
//! Wire format: JSON-RPC 2.0-style request/response objects framed with
//! a `u32` little-endian length prefix. The same handshake / skew
//! discipline applies on every transport (UDS on Unix, named pipe on
//! Windows when Wave 12 lands). Pre-GA the schema-skew tolerance is
//! `|app - daemon| <= 1`.
//!
//! Wave 6 phase 2 ships:
//!
//! - The wire-format types (`Hello`, `HelloAck`, `Request`, `Response`,
//!   `Error`).
//! - A length-prefixed framing codec.
//! - A Unix-domain-socket transport (server + client) on Unix; Windows
//!   gets a stub that returns `Unsupported` until Wave 12 lands the
//!   named-pipe transport.
//! - A `Service` trait the daemon implements to dispatch requests.
//! - The skew-matrix integration tests exercising every supported
//!   `app-N ↔ daemon-M` pair plus the `|N - M| = 2` negative case.

#![forbid(unsafe_code)]

pub mod client;
pub mod framing;
pub mod protocol;
pub mod server;
pub mod transport;

pub use client::{Client, ClientError};
pub use framing::{FrameError, read_frame, write_frame};
pub use protocol::{
    ErrorBody, Hello, HelloAck, IncompatibleVersion, Method, Request, Response, ResponseBody,
    StatusResponse, daemon_supported_versions,
};
pub use server::{ServeError, Service, serve_connection};
pub use transport::{ListenerHandle, TransportError};
