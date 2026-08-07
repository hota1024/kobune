//! 待ち受けとコネクションの受け入れ。

use std::convert::Infallible;
use std::sync::Arc;

use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::Notify;

use crate::activator::Activator;
use crate::proxy;
use crate::routes::Routes;

/// 平文 HTTP で待ち受ける。
///
/// `with_upgrades` を付けないと WebSocket が確立できない。HMR が動かなくなる。
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
                    tracing::warn!("接続の受け入れに失敗しました: {err}");
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
                tracing::trace!("接続を終了しました: {err}");
            }
        });
    }

    Ok(())
}

/// TLS で待ち受ける。証明書は SNI ごとに [`crate::ca`] が発行する。
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
                    tracing::warn!("接続の受け入れに失敗しました: {err}");
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

            // 信頼されていない CA だとここで失敗する。よくある状況なので
            // エラーとして騒がず、trace に落とす（`minato doctor` が診断する）。
            let stream = match acceptor.accept(stream).await {
                Ok(stream) => stream,
                Err(err) => {
                    tracing::trace!("TLS handshake に失敗しました: {err}");
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
                tracing::trace!("接続を終了しました: {err}");
            }
        });
    }

    Ok(())
}
