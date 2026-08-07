//! リバースプロキシ本体。
//!
//! upstream への接続はリクエストごとに張る。プール化していないのは、
//! WebSocket の upgrade を素直に扱えることと、開発環境では接続数が
//! 問題にならないため。
//!
//! **WebSocket と SSE は必ず通す。** 開発サーバの HMR がこれに依存しており、
//! ここが動かないと Minato は使いものにならない。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::header::{HOST, HeaderValue};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;

use crate::activator::{Activation, Activator};
use crate::routes::{Routes, normalize_host};

/// upstream への接続を諦めるまでの時間。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub type ProxyBody = BoxBody<Bytes, hyper::Error>;

/// ブラウザからの遷移で、待機ページを出す前にどれだけ待つか。
///
/// 温まったコンテナならこの間に上がる。待機ページを一瞬見せて
/// すぐ消える方が、体験としては悪い。
const BROWSER_GRACE: Duration = Duration::from_millis(1500);

/// ブラウザ以外（curl / fetch / エージェント）を待たせる上限。
///
/// **中途半端にエラーを返さない。** 起動中に 503 を返すと、
/// エージェントは「サーバが壊れている」と誤って判断する。
const CLIENT_WAIT: Duration = Duration::from_secs(120);

/// 待機ページの自動リロード間隔（秒）。
const RETRY_AFTER_SECS: u32 = 2;

/// 1 リクエストを処理する。
///
/// ルートが見つからなければ 404、upstream に繋がらなければ 502 を返す。
/// どちらも本文に理由を書く。ブラウザに空白のページを出しても、
/// エージェントに空の応答を返しても、原因の切り分けができないため。
pub async fn handle(
    request: Request<Incoming>,
    routes: Routes,
    activator: Arc<dyn Activator>,
) -> Response<ProxyBody> {
    let raw_host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        // HTTP/2 では :authority を使う。
        .or_else(|| request.uri().host().map(str::to_string))
        .unwrap_or_default();

    // **必ず正規化してから使う。** `Host` にはポートが付くことがあり、
    // 生のまま Activator に渡すと、アイドル判定のキーが
    // ルーティングのキー（正規化済み）と食い違う。その状態では
    // アクセスがアクセスとして数えられず、使用中のサービスが停止される。
    let Some(host) = normalize_host(&raw_host) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            format!("Minato: Host ヘッダ `{raw_host}` を解釈できません。\n"),
        );
    };

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

    // 起動済みなら最短経路で転送する。ここがほとんどのリクエストの通り道。
    let endpoint = match route.endpoint {
        Some(endpoint) => {
            activator.touch(&host);
            endpoint
        }
        None => match wake(&host, &request, activator.as_ref()).await {
            Ok(endpoint) => endpoint,
            Err(response) => return response,
        },
    };

    match forward(request, endpoint).await {
        Ok(response) => response,
        Err(err) => {
            tracing::debug!("転送に失敗しました ({endpoint}): {err}");
            error_response(
                StatusCode::BAD_GATEWAY,
                format!(
                    "Minato: {}/{} の {} に転送できませんでした ({endpoint}).\n\
                     サービスが起動しているか `minato status` で確認してください。\n\
                     詳細: {err}\n",
                    route.project, route.workspace, route.service
                ),
            )
        }
    }
}

/// 停止しているサービスを起こす。
///
/// 待ち方をクライアントで変える。ブラウザには待機ページを見せて
/// 自動リロードさせ、それ以外は受け付けられるまで待たせる。
async fn wake(
    host: &str,
    request: &Request<Incoming>,
    activator: &dyn Activator,
) -> Result<SocketAddr, Response<ProxyBody>> {
    let browser = wants_html(request);
    let wait = if browser { BROWSER_GRACE } else { CLIENT_WAIT };

    match activator.ensure_ready(host, wait).await {
        Activation::Ready(endpoint) => {
            activator.touch(host);
            Ok(endpoint)
        }
        Activation::Starting => Err(if browser {
            starting_page(host)
        } else {
            // ここに来るのは CLIENT_WAIT を使い切った場合だけ。
            error_response(
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "Minato: `{host}` の起動が {}秒 以内に終わりませんでした。\n\
                     `minato status` で状態を、`minato logs` で原因を確認してください。\n",
                    CLIENT_WAIT.as_secs()
                ),
            )
        }),
        Activation::Unknown => Err(error_response(
            StatusCode::NOT_FOUND,
            format!("Minato: `{host}` に対応する環境がありません。\n"),
        )),
        Activation::Failed(reason) => Err(error_response(
            StatusCode::BAD_GATEWAY,
            format!(
                "Minato: `{host}` を起動できませんでした。\n\
                 {reason}\n\
                 `minato logs` で詳細を確認してください。\n"
            ),
        )),
    }
}

/// ブラウザからの遷移か。
///
/// `Accept` に `text/html` があるかで判断する。API 呼び出しや
/// エージェントの curl はこれを送らない。
fn wants_html(request: &Request<Incoming>) -> bool {
    request
        .headers()
        .get(hyper::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"))
}

/// 起動中であることを伝え、自動で読み直すページ。
fn starting_page(host: &str) -> Response<ProxyBody> {
    let body = format!(
        r#"<!doctype html>
<html lang="ja">
<head>
<meta charset="utf-8">
<meta http-equiv="refresh" content="{RETRY_AFTER_SECS}">
<title>起動中 — {host}</title>
<style>
  body {{ font-family: ui-sans-serif, system-ui, sans-serif; display: grid;
         place-items: center; min-height: 100vh; margin: 0; color: #333; }}
  main {{ text-align: center; }}
  code {{ background: #f4f4f5; padding: .2em .4em; border-radius: .25em; }}
  .spinner {{ width: 2rem; height: 2rem; margin: 0 auto 1.5rem;
             border: 3px solid #e4e4e7; border-top-color: #71717a;
             border-radius: 50%; animation: spin 800ms linear infinite; }}
  @keyframes spin {{ to {{ transform: rotate(360deg); }} }}
  @media (prefers-color-scheme: dark) {{
    body {{ background: #18181b; color: #e4e4e7; }}
    code {{ background: #27272a; }}
    .spinner {{ border-color: #3f3f46; border-top-color: #a1a1aa; }}
  }}
</style>
</head>
<body>
<main>
  <div class="spinner"></div>
  <p><code>{host}</code> を起動しています</p>
  <p>準備ができ次第このページは自動で切り替わります</p>
</main>
</body>
</html>
"#
    );

    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(hyper::header::RETRY_AFTER, RETRY_AFTER_SECS.to_string())
        // 起動中の応答をキャッシュされると、上がった後も出続ける。
        .header(hyper::header::CACHE_CONTROL, "no-store")
        .body(
            Full::new(Bytes::from(body))
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("固定のレスポンスは常に組み立てられる")
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
