//! minatod — Minato's resident process, as a library.
//!
//! **This exists so the daemon can have integration tests.** A binary
//! crate cannot: `tests/` has nothing to import. Everything the daemon is
//! lives here and `main.rs` is the thin binary that starts it.
//!
//! Nothing is published from this crate, so the modules being `pub` is
//! about reach within the workspace and nothing more. What belongs in
//! `main.rs` is argument parsing, logging and the start-up order — the
//! parts that only make sense once, in a process.

pub mod activation;
pub mod activator;
pub mod carry;
pub mod env;
pub mod gateway;
pub mod idle;
pub mod paths;
pub mod resolve;
pub mod secrets;
pub mod server;
pub mod spec;
pub mod supervisor;
pub mod tunnel;
