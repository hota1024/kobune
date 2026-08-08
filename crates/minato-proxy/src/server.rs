//! Listening and accepting connections.

use std::convert::Infallible;
use std::sync::Arc;

use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::Notify;

use crate::activator::Activator;
use crate::proxy;
use crate::routes::Routes;

/// Listens for plain HTTP.
///
/// Without `with_upgrades` no WebSocket can be established, and HMR
/// stops working.
pub async fn serve_http(
    listener: TcpListener,
    routes: Routes,
    activator: Arc<dyn Activator>,
    shutdown: Arc<Notify>,
) -> std::io::Result<()> {
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(err) => {
                    tracing::warn!("cannot accept the connection: {err}");
                    continue;
                }
            },
            _ = shutdown.notified() => break,
        };

        let routes = routes.clone();
        let activator = activator.clone();
        tokio::spawn(async move {
            let _ = stream.set_nodelay(true);
            let io = TokioIo::new(stream);

            let service = service_fn(move |request| {
                let routes = routes.clone();
                let activator = activator.clone();
                async move { Ok::<_, Infallible>(proxy::handle(request, routes, activator).await) }
            });

            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                tracing::trace!("connection closed: {err}");
            }
        });
    }

    Ok(())
}

/// Listens for TLS. [`crate::ca`] issues a certificate per SNI name.
pub async fn serve_https(
    listener: TcpListener,
    routes: Routes,
    activator: Arc<dyn Activator>,
    tls: Arc<rustls::ServerConfig>,
    shutdown: Arc<Notify>,
) -> std::io::Result<()> {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);

    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(err) => {
                    tracing::warn!("cannot accept the connection: {err}");
                    continue;
                }
            },
            _ = shutdown.notified() => break,
        };

        let routes = routes.clone();
        let activator = activator.clone();
        let acceptor = acceptor.clone();

        tokio::spawn(async move {
            let _ = stream.set_nodelay(true);

            // An untrusted CA fails right here. That is common enough not
            // to shout about; `minato doctor` diagnoses it.
            let stream = match acceptor.accept(stream).await {
                Ok(stream) => stream,
                Err(err) => {
                    tracing::trace!("TLS handshake failed: {err}");
                    return;
                }
            };

            let io = TokioIo::new(stream);
            let service = service_fn(move |request| {
                let routes = routes.clone();
                let activator = activator.clone();
                async move { Ok::<_, Infallible>(proxy::handle(request, routes, activator).await) }
            });

            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                tracing::trace!("connection closed: {err}");
            }
        });
    }

    Ok(())
}
