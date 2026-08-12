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

/// `0.1.0 (abc1234)` — what a `Ping` is answered with, and what
/// `minatod --version` prints.
///
/// **The commit is the part that carries information.** Every nightly
/// reports the same crate version, so it is the commit that tells one
/// daemon from another — which is what lets a CLI notice that the process
/// answering it was started from a binary that has since been replaced.
pub fn version() -> String {
    minato_core::version_string(env!("CARGO_PKG_VERSION"))
}

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

#[cfg(test)]
mod tests {
    #[test]
    fn the_version_carries_the_commit_when_there_is_one() {
        let version = super::version();

        assert!(
            version.starts_with(env!("CARGO_PKG_VERSION")),
            "got: {version}"
        );

        // The CLI compares this string against its own to spot a daemon
        // left running from the build an update replaced. Both binaries
        // carry the same crate version and are only ever installed as a
        // pair, so the commit is the whole of what differs — and without
        // it the two builds would compare equal.
        assert_eq!(
            version.contains(minato_core::BUILD_COMMIT_SHORT),
            minato_core::BUILD_COMMIT != "unknown",
            "got: {version}",
        );
    }
}
