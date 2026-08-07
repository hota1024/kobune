//! Minato が管理するドメインを解決する DNS サーバ。
//!
//! macOS では `*.localhost` がシステムレベルで解決されない。Chrome だけが
//! 独自に 127.0.0.1 へ解決するが、`curl` / Safari / Node の fetch は解決しない。
//! **エージェントは curl で疎通確認する**ため、これが無いと成立しない。
//!
//! `/etc/resolver/localhost` に `nameserver 127.0.0.1` と `port` を書けば、
//! 非特権ポートで動かせる（:53 を取るのに root が要らない）。

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use hickory_proto::op::{Header, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::rdata::{A, AAAA};
use hickory_proto::rr::{RData, Record, RecordType};
use hickory_server::ServerFuture;
use hickory_server::authority::MessageResponseBuilder;
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};

/// 応答に載せる TTL（秒）。
///
/// 短くしておく。worktree の増減で名前が入れ替わるため、解決結果を
/// 長く握られると古い環境に繋ぎ続けることになる。
const DEFAULT_TTL: u32 = 5;

/// TCP 接続を切るまでのアイドル時間。
const TCP_TIMEOUT: Duration = Duration::from_secs(10);

/// 既定で受け持つドメイン。
pub const DEFAULT_SUFFIX: &str = "localhost";

#[derive(Debug, Clone)]
pub struct DnsConfig {
    /// 受け持つドメインの接尾辞。これに一致する問い合わせだけ答える。
    pub suffixes: Vec<String>,
    pub ttl: u32,
}

impl Default for DnsConfig {
    fn default() -> Self {
        Self {
            suffixes: vec![DEFAULT_SUFFIX.to_string()],
            ttl: DEFAULT_TTL,
        }
    }
}

impl DnsConfig {
    /// 名前が受け持ち範囲かどうか。
    ///
    /// `localhost` 自身と、その下のすべてに一致する。
    pub fn serves(&self, name: &str) -> bool {
        let normalized = name.trim_end_matches('.').to_ascii_lowercase();
        if normalized.is_empty() {
            return false;
        }

        self.suffixes.iter().any(|suffix| {
            let suffix = suffix.trim_end_matches('.').to_ascii_lowercase();
            normalized == suffix || normalized.ends_with(&format!(".{suffix}"))
        })
    }
}

/// Minato のドメインを 127.0.0.1 に向ける。
///
/// **ルートの有無を見ずに応答する。** 未知のホスト名も 127.0.0.1 に解決し、
/// プロキシに 404 を返させる。DNS の解決に失敗させると「名前が引けない」と
/// しか分からないが、プロキシまで届けば「どの workspace が動いているか」まで
/// 案内できる。
pub struct MinatoDns {
    config: DnsConfig,
}

impl MinatoDns {
    pub fn new(config: DnsConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &DnsConfig {
        &self.config
    }
}

#[async_trait::async_trait]
impl RequestHandler for MinatoDns {
    async fn handle_request<R: ResponseHandler>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        let builder = MessageResponseBuilder::from_message_request(request);

        let mut header = Header::response_from_request(request.header());
        header.set_authoritative(true);

        let mut answers: Vec<Record> = Vec::new();

        if request.op_code() != OpCode::Query || request.message_type() != MessageType::Query {
            // 問い合わせ以外（更新など）は扱わない。
            header.set_response_code(ResponseCode::NotImp);
        } else {
            let query = request.query();
            let name = query.name().to_string();

            if !self.config.serves(&name) {
                tracing::trace!("受け持ち範囲外の問い合わせ: {name}");
                header.set_response_code(ResponseCode::NXDomain);
            } else {
                match query.query_type() {
                    RecordType::A => answers.push(Record::from_rdata(
                        query.name().into(),
                        self.config.ttl,
                        RData::A(A(Ipv4Addr::LOCALHOST)),
                    )),
                    // プロキシは ::1 でも待ち受けるので AAAA も答える。
                    // 答えないとクライアントが A にフォールバックするまで
                    // 一往復ぶん遅れる。
                    RecordType::AAAA => answers.push(Record::from_rdata(
                        query.name().into(),
                        self.config.ttl,
                        RData::AAAA(AAAA(Ipv6Addr::LOCALHOST)),
                    )),
                    other => tracing::trace!("{other} は扱いません: {name}"),
                }
            }
        }

        let empty: [Record; 0] = [];
        let response = builder.build(header, answers.iter(), &empty, &empty, &empty);

        match response_handle.send_response(response).await {
            Ok(info) => info,
            Err(err) => {
                tracing::warn!("DNS 応答を送れませんでした: {err}");
                ResponseInfo::from(header)
            }
        }
    }
}

/// UDP と TCP の両方で待ち受ける。
///
/// TCP も要る。応答が 512 バイトを超える場合や、resolver によっては
/// TCP で問い合わせてくるため。
pub async fn serve(
    addr: SocketAddr,
    config: DnsConfig,
    shutdown: Arc<tokio::sync::Notify>,
) -> std::io::Result<()> {
    let udp = tokio::net::UdpSocket::bind(addr).await?;
    let tcp = tokio::net::TcpListener::bind(addr).await?;

    serve_sockets(vec![udp], vec![tcp], config, shutdown).await
}

/// 既に待ち受けているソケットで動かす。
///
/// launchd（socket activation）から fd を受け取る場合に使う。
/// :53 は特権ポートなので、非 root の daemon は自分で bind できない。
pub async fn serve_sockets(
    udp: Vec<tokio::net::UdpSocket>,
    tcp: Vec<tokio::net::TcpListener>,
    config: DnsConfig,
    shutdown: Arc<tokio::sync::Notify>,
) -> std::io::Result<()> {
    if udp.is_empty() && tcp.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "待ち受けるソケットがありません",
        ));
    }

    let mut server = ServerFuture::new(MinatoDns::new(config));

    for socket in udp {
        server.register_socket(socket);
    }
    for listener in tcp {
        server.register_listener(listener, TCP_TIMEOUT);
    }

    tokio::select! {
        result = server.block_until_done() => {
            if let Err(err) = result {
                tracing::warn!("DNS サーバが終了しました: {err}");
            }
        }
        _ = shutdown.notified() => {
            tracing::info!("DNS サーバを停止します");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_localhost_and_everything_under_it() {
        let config = DnsConfig::default();

        assert!(config.serves("localhost"));
        assert!(config.serves("localhost."));
        assert!(config.serves("web.feat-1.myapp.localhost"));
        assert!(config.serves("WEB.MyApp.LOCALHOST."));
    }

    #[test]
    fn ignores_names_outside_its_scope() {
        let config = DnsConfig::default();

        assert!(!config.serves("example.com"));
        assert!(!config.serves("notlocalhost"));
        assert!(
            !config.serves("localhost.example.com"),
            "接尾辞の一致であって部分一致ではない"
        );
        assert!(!config.serves(""));
        assert!(!config.serves("."));
    }

    #[test]
    fn honours_additional_suffixes() {
        let config = DnsConfig {
            suffixes: vec!["localhost".into(), "minato.test".into()],
            ttl: 5,
        };

        assert!(config.serves("web.myapp.minato.test"));
        assert!(config.serves("minato.test"));
        assert!(!config.serves("minato.test.evil.com"));
    }

    #[test]
    fn ttl_is_short_enough_to_follow_worktree_churn() {
        // 長いと消えた workspace に繋ぎ続けることになる。
        assert!(DnsConfig::default().ttl <= 30);
    }
}
