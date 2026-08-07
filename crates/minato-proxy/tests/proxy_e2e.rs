//! 実際に TCP を張ってプロキシの振る舞いを確かめる。
//!
//! 転送・エラー応答・WebSocket の upgrade・TLS は、単体テストでは
//! 検証しきれない。ここが M1 で最も壊れやすい部分なので実通信で押さえる。

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::header::{CONNECTION, HOST, UPGRADE};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use minato_proxy::{LocalCa, Route, Routes, serve_http, serve_https, server_config};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

type TestBody = BoxBody<Bytes, hyper::Error>;

fn text(body: &str) -> TestBody {
    Full::new(Bytes::from(body.to_string()))
        .map_err(|never| match never {})
        .boxed()
}

/// テスト用の upstream。パスを反射し、Upgrade 要求にはエコーで応じる。
async fn spawn_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };

            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = service_fn(|mut request: Request<Incoming>| async move {
                    if request.headers().contains_key(UPGRADE) {
                        // 101 を返したあと、生のバイト列をそのまま返す。
                        tokio::spawn(async move {
                            let Ok(upgraded) = hyper::upgrade::on(&mut request).await else {
                                return;
                            };
                            let mut io = TokioIo::new(upgraded);
                            let mut buffer = [0u8; 1024];
                            loop {
                                match io.read(&mut buffer).await {
                                    Ok(0) | Err(_) => return,
                                    Ok(n) => {
                                        if io.write_all(&buffer[..n]).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                            }
                        });

                        return Ok::<_, Infallible>(
                            Response::builder()
                                .status(StatusCode::SWITCHING_PROTOCOLS)
                                .header(UPGRADE, "echo")
                                .header(CONNECTION, "Upgrade")
                                .body(
                                    Empty::<Bytes>::new()
                                        .map_err(|never| match never {})
                                        .boxed(),
                                )
                                .expect("組み立てられる"),
                        );
                    }

                    let host = request
                        .headers()
                        .get(HOST)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("(none)")
                        .to_string();

                    Ok::<_, Infallible>(Response::new(text(&format!(
                        "upstream {} host={host}",
                        request.uri().path()
                    ))))
                });

                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .with_upgrades()
                    .await;
            });
        }
    });

    addr
}

/// プロキシを起動して待ち受けアドレスを返す。
async fn spawn_proxy(routes: Routes) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let shutdown = Arc::new(Notify::new());

    tokio::spawn(async move {
        let _ = serve_http(listener, routes, shutdown).await;
    });

    addr
}

/// プロキシに 1 リクエスト送って応答を受け取る。
async fn request_through(proxy: SocketAddr, host: &str, path: &str) -> (StatusCode, String) {
    let stream = TcpStream::connect(proxy).await.expect("接続できる");
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .expect("handshake");

    tokio::spawn(async move {
        let _ = connection.await;
    });

    let request = Request::builder()
        .uri(path)
        .header(HOST, host)
        .body(
            Empty::<Bytes>::new()
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("組み立てられる");

    let response = sender.send_request(request).await.expect("応答が返る");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("本文を読める")
        .to_bytes();

    (status, String::from_utf8_lossy(&body).to_string())
}

#[tokio::test]
async fn forwards_to_the_matching_upstream() {
    let upstream = spawn_upstream().await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::new(upstream, "myapp", "feat-1", "web"),
    );

    let proxy = spawn_proxy(routes).await;
    let (status, body) = request_through(proxy, "web.feat-1.myapp.localhost", "/hello").await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("upstream /hello"), "got: {body}");
}

#[tokio::test]
async fn preserves_the_original_host_header() {
    // Vite などは Host を見て許可判定をする。書き換えると弾かれる。
    let upstream = spawn_upstream().await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::new(upstream, "myapp", "feat-1", "web"),
    );

    let proxy = spawn_proxy(routes).await;
    let (_, body) = request_through(proxy, "web.feat-1.myapp.localhost", "/").await;

    assert!(
        body.contains("host=web.feat-1.myapp.localhost"),
        "ブラウザで開いた URL がそのままアプリに見える必要がある: {body}"
    );
}

#[tokio::test]
async fn routes_different_hosts_to_different_upstreams() {
    let one = spawn_upstream().await;
    let two = spawn_upstream().await;

    let routes = Routes::new();
    routes.insert(
        "web.a.myapp.localhost",
        Route::new(one, "myapp", "a", "web"),
    );
    routes.insert(
        "web.b.myapp.localhost",
        Route::new(two, "myapp", "b", "web"),
    );

    let proxy = spawn_proxy(routes).await;

    let (_, first) = request_through(proxy, "web.a.myapp.localhost", "/one").await;
    let (_, second) = request_through(proxy, "web.b.myapp.localhost", "/two").await;

    assert!(first.contains("/one"));
    assert!(second.contains("/two"));
}

#[tokio::test]
async fn unknown_host_explains_itself() {
    let proxy = spawn_proxy(Routes::new()).await;
    let (status, body) = request_through(proxy, "nope.myapp.localhost", "/").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body.contains("minato ls"),
        "何をすればよいか書かれている必要がある: {body}"
    );
}

#[tokio::test]
async fn dead_upstream_returns_bad_gateway() {
    // 誰も listen していないアドレス。
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let dead = listener.local_addr().expect("addr");
    drop(listener);

    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::new(dead, "myapp", "feat-1", "web"),
    );

    let proxy = spawn_proxy(routes).await;
    let (status, body) = request_through(proxy, "web.feat-1.myapp.localhost", "/").await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        body.contains("minato status"),
        "切り分けの手掛かりを出す: {body}"
    );
}

#[tokio::test]
async fn passes_websocket_style_upgrades_through() {
    // HMR はこれに依存している。通らないと Minato は使いものにならない。
    let upstream = spawn_upstream().await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::new(upstream, "myapp", "feat-1", "web"),
    );

    let proxy = spawn_proxy(routes).await;

    let stream = TcpStream::connect(proxy).await.expect("接続できる");
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .expect("handshake");

    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });

    let request = Request::builder()
        .uri("/ws")
        .header(HOST, "web.feat-1.myapp.localhost")
        .header(UPGRADE, "echo")
        .header(CONNECTION, "Upgrade")
        .body(
            Empty::<Bytes>::new()
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("組み立てられる");

    let response = sender.send_request(request).await.expect("応答が返る");
    assert_eq!(
        response.status(),
        StatusCode::SWITCHING_PROTOCOLS,
        "101 が返らないと WebSocket は張れない"
    );

    let upgraded = hyper::upgrade::on(response).await.expect("upgrade できる");
    let mut io = TokioIo::new(upgraded);

    io.write_all(b"hello over websocket").await.expect("送れる");

    let mut buffer = vec![0u8; 20];
    io.read_exact(&mut buffer).await.expect("返ってくる");

    assert_eq!(
        &buffer, b"hello over websocket",
        "双方向にバイトが流れる必要がある"
    );
}

#[tokio::test]
async fn serves_https_with_a_certificate_for_the_sni_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ca = Arc::new(LocalCa::load_or_create(dir.path()).expect("CA を作れる"));
    let ca_der = rustls::pki_types::CertificateDer::from(
        rustls_pemfile::certs(&mut std::io::BufReader::new(
            ca.certificate_pem().as_bytes(),
        ))
        .next()
        .expect("証明書がある")
        .expect("読める")
        .to_vec(),
    );

    let upstream = spawn_upstream().await;
    let routes = Routes::new();
    routes.insert(
        "web.feat-1.myapp.localhost",
        Route::new(upstream, "myapp", "feat-1", "web"),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let shutdown = Arc::new(Notify::new());

    tokio::spawn(async move {
        let _ = serve_https(listener, routes, server_config(ca), shutdown).await;
    });

    // CA だけを信頼したクライアントで繋ぐ。実際のブラウザと同じ条件。
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der).expect("CA を信頼できる");

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name =
        rustls::pki_types::ServerName::try_from("web.feat-1.myapp.localhost").expect("名前");

    let tcp = TcpStream::connect(addr).await.expect("接続できる");
    let tls = connector
        .connect(server_name, tcp)
        .await
        .expect("動的に発行された証明書で検証が通る");

    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .expect("handshake");

    tokio::spawn(async move {
        let _ = connection.await;
    });

    let request = Request::builder()
        .uri("/secure")
        .header(HOST, "web.feat-1.myapp.localhost")
        .body(
            Empty::<Bytes>::new()
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("組み立てられる");

    let response = sender.send_request(request).await.expect("応答が返る");
    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .expect("本文を読める")
        .to_bytes();

    assert!(String::from_utf8_lossy(&body).contains("upstream /secure"));
}
