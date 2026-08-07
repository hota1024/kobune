//! 停止しているサービスを起こす口。
//!
//! プロキシは `minato-runtime` に依存できない（`docs/DESIGN.md` §13）。
//! かといって「リクエストが来たら起動する」には runtime を動かす必要がある。
//! そこでこの trait を境界にし、実装は daemon 側に置く。
//! プロキシは「このホストを受け付けられる状態にしてくれ」と頼むだけでよい。

use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;

/// 起動要求の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activation {
    /// 受け付け可能。ここに転送する。
    Ready(SocketAddr),

    /// 起動処理を始めたが、まだ受け付けられない。
    ///
    /// ブラウザには待機ページを返し、それ以外は待たせる。
    Starting,

    /// そんなホストは知らない。
    Unknown,

    /// 起動を試みたが失敗した。
    Failed(String),
}

#[async_trait]
pub trait Activator: Send + Sync + 'static {
    /// ホストに対応するサービスを受け付け可能にする。
    ///
    /// 既に起動していれば何もせず転送先を返す。停止していれば起動する。
    ///
    /// `wait` は受け付け可能になるまで待つ上限。ブラウザには短く指定して
    /// 待機ページを見せ、それ以外は長く待たせる。時間内に上がらなければ
    /// [`Activation::Starting`] を返す（起動処理自体は続く）。
    async fn ensure_ready(&self, host: &str, wait: Duration) -> Activation;

    /// アクセスがあったことを記録する。
    ///
    /// アイドル判定の基準になる。リクエストごとに呼ばれるため、
    /// **速くなければならない**（ロックを長く持たない、I/O をしない）。
    fn touch(&self, host: &str);
}

/// 何も起こさない実装。scale-to-zero を使わない場合とテスト用。
pub struct NoopActivator;

#[async_trait]
impl Activator for NoopActivator {
    async fn ensure_ready(&self, _host: &str, _wait: Duration) -> Activation {
        Activation::Unknown
    }

    fn touch(&self, _host: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_knows_nothing() {
        let activator = NoopActivator;

        assert_eq!(
            activator
                .ensure_ready("web.myapp.localhost", Duration::from_millis(1))
                .await,
            Activation::Unknown
        );
        activator.touch("web.myapp.localhost");
    }
}
