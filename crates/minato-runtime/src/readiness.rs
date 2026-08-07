//! 起動したサービスが実際に接続を受け付けるまで待つ。
//!
//! コンテナが「起動した」ことと「受け付けられる」ことは別。両者を混同すると
//! `minato new` の直後の `curl` が connection refused になり、エージェントは
//! 「サーバが壊れている」と誤って判断する。
//!
//! ここでは TCP 接続が確立できるかだけを見る。`minato.toml` の `health`
//! （HTTP のステータスやコマンドの終了コード）を使った本来の判定は M2 で入れる。

use std::net::SocketAddr;
use std::time::Duration;

use crate::event::EventSink;

/// 接続できるまで待つ既定の上限。
///
/// 開発サーバの初回起動（依存の解決やコンパイル）はこれより長くかかることがある。
/// その場合は待たずに進み、警告だけ出す。無限に待つと `minato up` が返らなくなる。
pub const DEFAULT_READINESS_TIMEOUT: Duration = Duration::from_secs(15);

/// 接続を試みる間隔。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 1 回の接続試行に許す時間。
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// `addr` が TCP 接続を受け付けるまで待つ。
///
/// 受け付けるようになったら `true`、`timeout` を過ぎたら `false` を返す。
/// 待てなかったこと自体は失敗として扱わない（アプリの起動が遅いだけの場合がある）。
pub async fn wait_for_tcp(addr: SocketAddr, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        if tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr))
            .await
            .is_ok_and(|result| result.is_ok())
        {
            return true;
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
    timeout: Duration,
    events: &EventSink,
) -> bool {
    let Some(addr) = endpoint else {
        // ポートを公開していないサービスは接続確認のしようがない。
        return true;
    };

    let label = format!("{service} の応答を待機");
    events.step_started("await", &label);

    if wait_for_tcp(addr, timeout).await {
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
    use minato_api::{Event, StepStatus};

    #[tokio::test]
    async fn detects_a_listening_port() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        assert!(wait_for_tcp(addr, Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn times_out_on_a_closed_port() {
        // bind してすぐ閉じたポートは誰も listen していない。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);

        assert!(!wait_for_tcp(addr, Duration::from_millis(300)).await);
    }

    #[tokio::test]
    async fn waits_for_a_port_that_opens_late() {
        // ここが本題。コンテナは起動したがアプリがまだ listen していない状態。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let late = tokio::net::TcpListener::bind(addr).await.expect("再 bind");
            // 待ち側が接続するまで保持する。
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(late);
        });

        assert!(
            wait_for_tcp(addr, Duration::from_secs(3)).await,
            "遅れて開いたポートも検出できる必要がある"
        );
    }

    #[tokio::test]
    async fn services_without_a_port_are_considered_ready() {
        let (sink, mut rx) = EventSink::channel();

        assert!(await_service("db", None, Duration::from_millis(100), &sink).await);
        drop(sink);

        assert!(rx.recv().await.is_none(), "待つ必要がないので何も出さない");
    }

    #[tokio::test]
    async fn reports_done_when_the_service_answers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        let (sink, mut rx) = EventSink::channel();
        assert!(await_service("web", Some(addr), Duration::from_secs(2), &sink).await);
        drop(sink);

        let mut statuses = Vec::new();
        while let Some(event) = rx.recv().await {
            if let Event::Step { status, .. } = event {
                statuses.push(status);
            }
        }

        assert_eq!(statuses, vec![StepStatus::Started, StepStatus::Done]);
    }

    #[tokio::test]
    async fn warns_but_does_not_fail_when_the_service_is_slow() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);

        let (sink, mut rx) = EventSink::channel();
        let ready = await_service("web", Some(addr), Duration::from_millis(200), &sink).await;
        drop(sink);

        assert!(!ready, "応答しなかったことは呼び出し側に伝える");

        let mut saw_warning = false;
        while let Some(event) = rx.recv().await {
            if let Event::Log {
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
