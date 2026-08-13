//! The types at the bottom of Minato's dependency graph: configuration,
//! naming, state, and what a program makes of a terminal.
//!
//! This crate stays a mostly side-effect-free foundation. Container
//! operations and networking belong to `minato-runtime` / `minato-proxy`
//! and must not leak in here.

pub mod apple;
pub mod config;
pub mod env;
pub mod error;
pub mod git;
pub mod launchd;
pub mod naming;
pub mod paths;
pub mod service;
pub mod state;
pub mod terminal;

pub use config::{HealthCheck, MinatoConfig, RuntimeSection, ServiceConfig, ServiceScope};
pub use env::{EnvEntry, EnvLayers, EnvScope, SecretRef};
pub use error::{Error, Result};
pub use git::Repository;
pub use paths::Paths;
pub use service::ServiceState;
pub use state::{ProjectRecord, State, StateStore, TunnelRecord, WorkspaceRecord};
pub use terminal::Modes;

/// The commit this was built from, or `unknown`.
///
/// Every nightly build carries version 0.1.0, so this is what tells one
/// build from another — and therefore the only thing the update check can
/// compare.
pub const BUILD_COMMIT: &str = env!("MINATO_BUILD_COMMIT");

/// The target triple this was built for, e.g. `aarch64-apple-darwin`.
///
/// Names the release archive that would replace this binary.
pub const BUILD_TARGET: &str = env!("MINATO_BUILD_TARGET");

/// [`BUILD_COMMIT`], shortened to the length `git log --oneline` uses.
pub const BUILD_COMMIT_SHORT: &str = env!("MINATO_BUILD_COMMIT_SHORT");

/// How the version is presented: `0.1.0 (abc1234)`.
///
/// The commit is in brackets rather than appended to the version so it does
/// not read as part of a semantic version.
pub fn version_string(crate_version: &str) -> String {
    if BUILD_COMMIT == "unknown" {
        crate_version.to_string()
    } else {
        format!("{crate_version} ({BUILD_COMMIT_SHORT})")
    }
}
