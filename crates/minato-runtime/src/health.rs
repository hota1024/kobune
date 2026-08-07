//! サービスが「受け付けられる」状態かを判定する。
//!
//! コンテナが起動したことと、中のアプリが応答できることは別。両者を
//! 混同すると `minato up` の直後の `curl` が connection refused になり、
//! エージェントは「サーバが壊れている」と誤って判断する。
//!
//! `minato.toml` の `health` が指定されていればそれに従い、無ければ
//! TCP 接続が確立できるかだけを見る。

use std::net::SocketAddr;
use std::time::Duration;

use minato_core::HealthCheck;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::error::{Result, RuntimeError};
use crate::event::EventSink;

/// 受け付け可能になるまで待つ既定の上限。
///
/// 開発サーバの初回起動（依存の解決やコンパイル）はこれより長くかかる
/// ことがある。その場合は待たずに進み、警告だけ出す。無限に待つと
/// `minato up` が返らなくなる。
pub const DEFAULT_READINESS_TIMEOUT: Duration = Duration::from_secs(15);

/// 判定の間隔。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 1 回の判定に許す時間。
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// 1 回だけ判定する。
///
/// `health` が `None` なら TCP 接続の可否で判断する。
pub async fn probe(endpoint: SocketAddr, health: Option<&HealthCheck>) -> Result<bool> {
    match health {
        None => Ok(probe_tcp(endpoint).await),
        Some(HealthCheck::Tcp(_)) => Ok(probe_tcp(endpoint).await),
        Some(HealthCheck::Http(url)) => Ok(probe_http(endpoint, url).await),
        Some(HealthCheck::Cmd(command)) => Err(RuntimeError::Unsupported(format!(
            "health = \"cmd:{command}\" はまだ対応していません。\
             `http://...` か `tcp://...` を指定してください"
        ))),
    }
}

/// TCP 接続が確立できるか。
async fn probe_tcp(endpoint: SocketAddr) -> bool {
    tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(endpoint))
        .await
        .is_ok_and(|result| result.is_ok())
}

/// HTTP で叩いて 2xx / 3xx が返るか。
///
/// `health` に書かれた URL のホストは無視し、**パスだけを使って
/// `endpoint` に対して発行する**。設定にはコンテナ内から見たアドレス
/// （`http://localhost:3000/healthz`）を書くが、ホスト側から届くのは
/// runtime が割り当てた別のアドレスであるため。
async fn probe_http(endpoint: SocketAddr, url: &str) -> bool {
    let path = path_of(url);

    let Ok(Ok(mut stream)) =
        tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(endpoint)).await
    else {
        return false;
    };

    // 依存を増やさないよう、最小限の HTTP/1.1 リクエストを手で書く。
    // health check は応答行だけ見れば足りる。
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {endpoint}\r\nConnection: close\r\nUser-Agent: minato-health\r\n\r\n"
    );

    if tokio::time::timeout(PROBE_TIMEOUT, stream.write_all(request.as_bytes()))
        .await
        .is_err()
    {
        return false;
    }

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();

    if tokio::time::timeout(PROBE_TIMEOUT, reader.read_line(&mut status_line))
        .await
        .is_err()
    {
        return false;
    }

    matches!(status_code(&status_line), Some(code) if (200..400).contains(&code))
}

/// `http://localhost:3000/healthz?x=1` から `/healthz?x=1` を取り出す。
fn path_of(url: &str) -> String {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    match without_scheme.find('/') {
        Some(index) => without_scheme[index..].to_string(),
        None => "/".to_string(),
    }
}

/// `HTTP/1.1 200 OK` から `200` を取り出す。
fn status_code(status_line: &str) -> Option<u16> {
    status_line.split_whitespace().nth(1)?.parse().ok()
}

/// 受け付け可能になるまで待つ。
///
/// 待てなかったこと自体は失敗として扱わない（アプリの起動が遅いだけの
/// 場合がある）。受け付けられるようになったかを `bool` で返す。
pub async fn wait_until_ready(
    endpoint: SocketAddr,
    health: Option<&HealthCheck>,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        match probe(endpoint, health).await {
            Ok(true) => return true,
            // 判定方法が未対応なら、待っても状況は変わらない。
            Err(_) => return false,
            Ok(false) => {}
        }

        if tokio::time::Instant::now() >= deadline {
            return false;
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// サービスが受け付けるまで待ち、待てなかった場合は警告を出す。
pub async fn await_service(
    service: &str,
    endpoint: Option<SocketAddr>,
    health: Option<&HealthCheck>,
    timeout: Duration,
    events: &EventSink,
) -> bool {
    let Some(addr) = endpoint else {
        // ポートを公開していないサービスは接続確認のしようがない。
        return true;
    };

    let label = format!("{service} の応答を待機");
    events.step_started("await", &label);

    if wait_until_ready(addr, health, timeout).await {
        events.step_done("await", &label);
        return true;
    }

    events.step_skipped(
        "await",
        &label,
        format!("{}秒以内に応答しませんでした", timeout.as_secs()),
    );
    events.warn(format!(
        "{service} はまだ {addr} で応答していません。\
         起動に時間がかかっている可能性があります"
    ));

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn extracts_the_path_from_a_health_url() {
        // 設定にはコンテナ内から見たアドレスを書く。ホスト側から届く
        // アドレスは別なので、パスだけを使う。
        assert_eq!(path_of("http://localhost:3000/healthz"), "/healthz");
        assert_eq!(path_of("https://localhost/ready?deep=1"), "/ready?deep=1");
        assert_eq!(path_of("http://localhost:3000"), "/");
        assert_eq!(path_of("http://127.0.0.1"), "/");
    }

    #[test]
    fn parses_status_codes() {
        assert_eq!(status_code("HTTP/1.1 200 OK"), Some(200));
        assert_eq!(status_code("HTTP/1.1 503 Service Unavailable"), Some(503));
        assert_eq!(status_code("garbage"), None);
        assert_eq!(status_code(""), None);
    }

    /// 指定のステータス行を返すだけの HTTP サーバ。
    async fn spawn_http(status: &'static str) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let response =
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });

        addr
    }

    async fn closed_port() -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        addr
    }

    #[tokio::test]
    async fn tcp_probe_follows_the_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        assert!(probe(addr, None).await.expect("判定できる"));

        drop(listener);
        assert!(!probe(closed_port().await, None).await.expect("判定できる"));
    }

    #[tokio::test]
    async fn http_probe_accepts_success_and_redirects() {
        for status in ["200 OK", "204 No Content", "302 Found"] {
            let addr = spawn_http(status).await;
            let health = HealthCheck::Http("http://localhost/healthz".into());

            assert!(
                probe(addr, Some(&health)).await.expect("判定できる"),
                "{status} は ready 扱いにする"
            );
        }
    }

    #[tokio::test]
    async fn http_probe_rejects_server_errors() {
        // 起動はしたが依存に繋がっていない、という状態を弾く。
        let addr = spawn_http("503 Service Unavailable").await;
        let health = HealthCheck::Http("http://localhost/healthz".into());

        assert!(!probe(addr, Some(&health)).await.expect("判定できる"));
    }

    #[tokio::test]
    async fn http_probe_fails_when_nothing_listens() {
        let addr = closed_port().await;
        let health = HealthCheck::Http("http://localhost/healthz".into());

        assert!(!probe(addr, Some(&health)).await.expect("判定できる"));
    }

    #[tokio::test]
    async fn tcp_health_only_needs_a_connection() {
        // TCP 指定なら HTTP を話さないサービスでも ready になる。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let health = HealthCheck::Tcp("localhost:5432".into());

        assert!(probe(addr, Some(&health)).await.expect("判定できる"));
    }

    #[tokio::test]
    async fn cmd_health_reports_that_it_is_unsupported() {
        let addr = closed_port().await;
        let health = HealthCheck::Cmd("pg_isready".into());

        let err = probe(addr, Some(&health)).await.unwrap_err();
        assert!(err.to_string().contains("対応していません"), "got: {err}");
    }

    #[tokio::test]
    async fn waiting_gives_up_on_unsupported_checks() {
        // 待っても状況が変わらないものを待ち続けない。
        let addr = closed_port().await;
        let health = HealthCheck::Cmd("pg_isready".into());

        let started = std::time::Instant::now();
        let ready = wait_until_ready(addr, Some(&health), Duration::from_secs(5)).await;

        assert!(!ready);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "即座に諦める必要がある"
        );
    }

    #[tokio::test]
    async fn waits_for_a_service_that_becomes_ready_late() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let late = tokio::net::TcpListener::bind(addr).await.expect("再 bind");
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(late);
        });

        assert!(wait_until_ready(addr, None, Duration::from_secs(3)).await);
    }

    #[tokio::test]
    async fn services_without_a_port_are_considered_ready() {
        let (sink, mut rx) = EventSink::channel();

        assert!(await_service("db", None, None, Duration::from_millis(100), &sink).await);
        drop(sink);

        assert!(rx.recv().await.is_none(), "待つ必要がないので何も出さない");
    }

    #[tokio::test]
    async fn warns_but_does_not_fail_when_the_service_is_slow() {
        let addr = closed_port().await;
        let (sink, mut rx) = EventSink::channel();

        let ready = await_service("web", Some(addr), None, Duration::from_millis(200), &sink).await;
        drop(sink);

        assert!(!ready, "応答しなかったことは呼び出し側に伝える");

        let mut saw_warning = false;
        while let Some(event) = rx.recv().await {
            if let minato_api::Event::Log {
                level: minato_api::LogLevel::Warn,
                ..
            } = event
            {
                saw_warning = true;
            }
        }

        assert!(saw_warning, "利用者に状況を伝える必要がある");
    }
}
