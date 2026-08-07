//! Minato の依存グラフの底にある型と、設定・命名・状態の取り扱い。
//!
//! この crate は副作用の少ない土台に徹する。コンテナ操作やネットワークは
//! `minato-runtime` / `minato-proxy` などが担当し、ここには持ち込まない。

pub mod config;
pub mod error;
pub mod git;
pub mod naming;
pub mod paths;
pub mod service;
pub mod state;

pub use config::{HealthCheck, MinatoConfig, ServiceConfig, ServiceScope};
pub use error::{Error, Result};
pub use git::Repository;
pub use paths::Paths;
pub use service::ServiceState;
pub use state::{ProjectRecord, State, StateStore, WorkspaceRecord};
