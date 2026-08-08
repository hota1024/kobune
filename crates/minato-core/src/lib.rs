//! The types at the bottom of Minato's dependency graph: configuration,
//! naming, and state.
//!
//! This crate stays a mostly side-effect-free foundation. Container
//! operations and networking belong to `minato-runtime` / `minato-proxy`
//! and must not leak in here.

pub mod config;
pub mod env;
pub mod error;
pub mod git;
pub mod naming;
pub mod paths;
pub mod service;
pub mod state;

pub use config::{HealthCheck, MinatoConfig, ServiceConfig, ServiceScope};
pub use env::{EnvEntry, EnvLayers, EnvScope, SecretRef};
pub use error::{Error, Result};
pub use git::Repository;
pub use paths::Paths;
pub use service::ServiceState;
pub use state::{ProjectRecord, State, StateStore, TunnelRecord, WorkspaceRecord};
