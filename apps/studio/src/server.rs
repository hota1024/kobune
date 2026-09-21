//! The router, and what is behind which door.
//!
//! Two halves, and the line between them is the whole security model:
//! `/api` is everything that can reach the daemon and is behind
//! [`crate::guard`]; everything else is the static bundle, which is an
//! empty page until it has a token.

use std::path::PathBuf;
use std::time::Duration;

use axum::routing::{get, post};
use axum::{Router, middleware};
use kobune_client::Client;

use crate::api::{self, Studio};
use crate::assets;
use crate::guard::{self, Session};

/// How long a connection has to let go after ctrl-c before the process
/// stops waiting for it.
///
/// Long enough that an `up` mid-flight finishes and answers, short enough
/// that nobody reaches for `kill -9`.
const GRACE: Duration = Duration::from_secs(5);

pub async fn serve(
    listener: tokio::net::TcpListener,
    session: Session,
    client: Client,
    cwd: PathBuf,
) -> anyhow::Result<()> {
    // **Level-triggered, not a `Notify`.** A follow that registers its
    // interest after the signal has already been sent would miss a
    // wakeup and hold the shutdown open; a `watch` that is already `true`
    // resolves immediately for every later reader.
    let (stopping, watch) = tokio::sync::watch::channel(false);

    let studio = Studio {
        client,
        cwd,
        stopping: watch.clone(),
    };

    let api = Router::new()
        .route("/state", get(api::state))
        .route("/doctor", get(api::doctor))
        .route("/env", get(api::env))
        .route("/logs", get(api::logs))
        .route("/act", post(api::act))
        .fallback(guard::nowhere)
        .layer(middleware::from_fn_with_state(
            session.clone(),
            guard::require_session,
        ))
        .with_state(studio);

    let app = Router::new().nest("/api", api).fallback(get(assets::serve));

    let server = async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal(stopping))
            .await
    };

    // The backstop for the case the first half did not cover: a daemon
    // that has stopped answering leaves `call_until` waiting on a reply
    // that is not coming, and a body that cannot end is a process that
    // cannot exit. Five seconds, then say so and go.
    let grace = async move {
        let mut watch = watch;
        let _ = watch.wait_for(|stopping| *stopping).await;
        tokio::time::sleep(GRACE).await;
    };

    tokio::pin!(server, grace);

    tokio::select! {
        result = &mut server => result?,
        _ = &mut grace => {
            tracing::warn!("a connection did not let go within {GRACE:?}; stopping anyway");
        }
    }

    Ok(())
}

/// Ctrl-c, and then telling everything that is streaming.
///
/// **Every open follow is told, not just the listener.** Graceful
/// shutdown stops accepting and then waits for the response bodies
/// already in flight, and a log follow is a body that by design never
/// ends — one open log pane would otherwise make ctrl-c do nothing at
/// all, for ever.
async fn shutdown_signal(stopping: tokio::sync::watch::Sender<bool>) {
    let _ = tokio::signal::ctrl_c().await;
    let _ = stopping.send(true);
}
