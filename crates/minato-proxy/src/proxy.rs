//! リバースプロキシ本体。
//!
//! upstream への接続はリクエストごとに張る。プール化していないのは、
//! WebSocket の upgrade を素直に扱えることと、開発環境では接続数が
//! 問題にならないため。
//!
//! **WebSocket と SSE は必ず通す。** 開発サーバの HMR がこれに依存しており、
//! ここが動かないと Minato は使いものにならない。

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::header::{HOST, HeaderValue};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;

use crate::routes::Routes;

/// upstream への接続を諦めるまでの時間。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub type ProxyBody = BoxBody<Bytes, hyper::Error>;

/// 1 リクエストを処理する。
///
/// ルートが見つからなければ 404、upstream に繋がらなければ 502 を返す。
/// どちらも本文に理由を書く。ブラウザに空白のページを出しても、
/// エージェントに空の応答を返しても、原因の切り分けができないため。
pub async fn handle(request: Request<Incoming>, routes: Routes) -> Response<ProxyBody> {
    let host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        // HTTP/2 では :authority を使う。
        .or_else(|| request.uri().host().map(str::to_string))
        .unwrap_or_default();

    let Some(route) = routes.get(&host) else {
        tracing::debug!("ルートが見つかりません: {host}");
        return error_response(
            StatusCode::NOT_FOUND,
            format!(
                "Minato: `{host}` に対応する環境がありません。\n\
                 `minato ls` で起動している workspace を確認してください。\n"
            ),
        );
    };

    match forward(request, route.endpoint).await {
        Ok(response) => response,
        Err(err) => {
            tracing::debug!("転送に失敗しました ({}): {err}", route.endpoint);
            error_response(
                StatusCode::BAD_GATEWAY,
                format!(
                    "Minato: {}/{} の {} に転送できませんでした ({}).\n\
                     サービスが起動しているか `minato status` で確認してください。\n\
                     詳細: {err}\n",
                    route.project, route.workspace, route.service, route.endpoint
                ),
            )
        }
    }
}

/// upstream に転送する。upgrade（WebSocket）にも対応する。
async fn forward(
    mut request: Request<Incoming>,
    upstream: SocketAddr,
) -> Result<Response<ProxyBody>, ProxyError> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(upstream))
        .await
        .map_err(|_| ProxyError::ConnectTimeout(upstream))?
        .map_err(|source| ProxyError::Connect { upstream, source })?;

    // Nagle を切る。HMR の小さなメッセージが遅延するのを避ける。
    let _ = stream.set_nodelay(true);

    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(ProxyError::Handshake)?;

    // upgrade の際にコネクションの所有権が必要になるため、
    // `with_upgrades` で駆動しておく。
    let connection = connection.with_upgrades();
    let connection_task = tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::trace!("upstream との接続が終了しました: {err}");
        }
    });

    // upgrade 要求かどうかは転送前に見ておく。転送後は request を触れない。
    let upgrade_requested = request.headers().get(hyper::header::UPGRADE).is_some();

    // Host は upstream から見た値に書き換えない。
    // Vite などは Host を見て許可判定をするため、元の値を保つ方が
    // 「ブラウザで開いた URL がそのままアプリに見える」形になる。
    // ただし Host が無いリクエストには補う。
    if !request.headers().contains_key(HOST) {
        if let Ok(value) = HeaderValue::from_str(&upstream.to_string()) {
            request.headers_mut().insert(HOST, value);
        }
    }

    // upgrade する場合は元のリクエストの upgrade ハンドルを先に確保する。
    let client_upgrade = if upgrade_requested {
        Some(hyper::upgrade::on(&mut request))
    } else {
        None
    };

    let response = sender
        .send_request(request)
        .await
        .map_err(ProxyError::Upstream)?;

    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        return Ok(splice_upgrade(response, client_upgrade, connection_task).await);
    }

    // 通常のレスポンスはそのまま流す。接続は本文を読み終えるまで生かす。
    tokio::spawn(async move {
        let _ = connection_task.await;
    });

    Ok(response.map(|body| body.boxed()))
}

/// 101 応答を受けたら、両側の upgrade を繋いで双方向にコピーする。
async fn splice_upgrade(
    response: Response<Incoming>,
    client_upgrade: Option<hyper::upgrade::OnUpgrade>,
    connection_task: tokio::task::JoinHandle<()>,
) -> Response<ProxyBody> {
    let (parts, body) = response.into_parts();
    let upstream_upgrade = hyper::upgrade::on(Response::from_parts(parts.clone(), body));

    let Some(client_upgrade) = client_upgrade else {
        // upgrade を要求していないのに 101 が返るのは upstream の異常。
        connection_task.abort();
        return error_response(
            StatusCode::BAD_GATEWAY,
            "Minato: upgrade を要求していないのに 101 が返りました\n".to_string(),
        );
    };

    tokio::spawn(async move {
        let (client, upstream) = match tokio::try_join!(client_upgrade, upstream_upgrade) {
            Ok(pair) => pair,
            Err(err) => {
                tracing::debug!("upgrade を確立できませんでした: {err}");
                connection_task.abort();
                return;
            }
        };

        let mut client = TokioIo::new(client);
        let mut upstream = TokioIo::new(upstream);

        // WebSocket はどちらからでもデータが流れる。
        // 片方が閉じたらもう片方も閉じる。
        match tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
            Ok((to_upstream, to_client)) => {
                tracing::trace!("upgrade 終了: ↑{to_upstream}B ↓{to_client}B");
            }
            Err(err) => tracing::trace!("upgrade の中継が終了しました: {err}"),
        }

        connection_task.abort();
    });

    Response::from_parts(parts, empty_body())
}

fn error_response(status: StatusCode, message: String) -> Response<ProxyBody> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(
            Full::new(Bytes::from(message))
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("固定のレスポンスは常に組み立てられる")
}

fn empty_body() -> ProxyBody {
    Full::new(Bytes::new())
        .map_err(|never| match never {})
        .boxed()
}

#[derive(Debug, thiserror::Error)]
enum ProxyError {
    #[error("{0} への接続がタイムアウトしました")]
    ConnectTimeout(SocketAddr),

    #[error("{upstream} に接続できません: {source}")]
    Connect {
        upstream: SocketAddr,
        #[source]
        source: std::io::Error,
    },

    #[error("upstream との handshake に失敗しました: {0}")]
    Handshake(#[source] hyper::Error),

    #[error("upstream がエラーを返しました: {0}")]
    Upstream(#[source] hyper::Error),
}
