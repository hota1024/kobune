//! The router, and what is behind which door.
//!
//! Two halves, and the line between them is the whole security model:
//! `/api` is everything that can reach the daemon and is behind
//! [`crate::guard`]; everything else is the static bundle, which is an
//! empty page until it has a token.

use std::path::PathBuf;

use axum::routing::{get, post};
use axum::{Router, middleware};
use kobune_client::Client;

use crate::api::{self, Studio};
use crate::assets;
use crate::guard::{self, Session};

pub async fn serve(
    listener: tokio::net::TcpListener,
    session: Session,
    cwd: PathBuf,
) -> anyhow::Result<()> {
    let studio = Studio {
        client: Client::from_env()?,
        cwd,
    };

    let api = Router::new()
        .route("/state", get(api::state))
        .route("/doctor", get(api::doctor))
        .route("/env", get(api::env))
        .route("/logs", get(api::logs))
        .route("/act", post(api::act))
        .layer(middleware::from_fn_with_state(
            session.clone(),
            guard::require_session,
        ))
        .with_state(studio);

    let app = Router::new().nest("/api", api).fallback(get(assets::serve));

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;

    Ok(())
}
