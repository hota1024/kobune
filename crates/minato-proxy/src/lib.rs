//! The reverse proxy, and the local CA it needs.
//!
//! The proxy knows nothing about runtimes. It forwards to the `SocketAddr`
//! in [`Routes`] without caring whether that is a forwarded host port or a
//! container's own IP.

pub mod activator;
pub mod ca;
pub mod proxy;
pub mod routes;
pub mod server;

pub use activator::{Activation, Activator, NoopActivator};
pub use ca::{CA_CERT_FILE, CA_KEY_FILE, CaError, DynamicCertResolver, LocalCa, server_config};
pub use routes::{Route, Routes, normalize_host};
pub use server::{serve_http, serve_https};

/// The default HTTP port.
pub const DEFAULT_HTTP_PORT: u16 = 80;

/// The default HTTPS port.
pub const DEFAULT_HTTPS_PORT: u16 = 443;
