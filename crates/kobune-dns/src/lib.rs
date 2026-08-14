//! A DNS server for the domains Kobune manages.
//!
//! macOS does not resolve `*.localhost` at the system level. Chrome maps it
//! to 127.0.0.1 on its own, but `curl`, Safari and Node's fetch do not.
//! **Agents check with curl**, so nothing works without this.
//!
//! Writing `nameserver 127.0.0.1` and a `port` into
//! `/etc/resolver/localhost` lets this run on an unprivileged port — no
//! root needed to claim :53.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use hickory_proto::op::{Header, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::rdata::{A, AAAA};
use hickory_proto::rr::{RData, Record, RecordType};
use hickory_server::ServerFuture;
use hickory_server::authority::MessageResponseBuilder;
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};

/// The TTL to put on answers, in seconds.
///
/// Kept short. Names come and go with worktrees, and a cached answer held
/// too long keeps pointing at an environment that is gone.
const DEFAULT_TTL: u32 = 5;

/// How long a TCP connection may idle.
const TCP_TIMEOUT: Duration = Duration::from_secs(10);

/// The domain served by default.
pub const DEFAULT_SUFFIX: &str = "localhost";

#[derive(Debug, Clone)]
pub struct DnsConfig {
    /// The suffixes served. Only matching queries are answered.
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
    /// Whether a name falls within scope.
    ///
    /// Matches `localhost` itself and everything beneath it.
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

/// Points Kobune's domains at 127.0.0.1.
///
/// **Answers regardless of whether a route exists.** Unknown hostnames also
/// resolve to 127.0.0.1 so the proxy can return a 404. A DNS failure only
/// says "the name does not resolve"; reaching the proxy can say which
/// workspaces are actually running.
pub struct KobuneDns {
    config: DnsConfig,
}

impl KobuneDns {
    pub fn new(config: DnsConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &DnsConfig {
        &self.config
    }
}

#[async_trait::async_trait]
impl RequestHandler for KobuneDns {
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
            // Anything other than a query — an update, say — is not handled.
            header.set_response_code(ResponseCode::NotImp);
        } else {
            let query = request.query();
            let name = query.name().to_string();

            if !self.config.serves(&name) {
                tracing::trace!("query outside our scope: {name}");
                header.set_response_code(ResponseCode::NXDomain);
            } else {
                match query.query_type() {
                    RecordType::A => answers.push(Record::from_rdata(
                        query.name().into(),
                        self.config.ttl,
                        RData::A(A(Ipv4Addr::LOCALHOST)),
                    )),
                    // The proxy also listens on ::1, so answer AAAA too.
                    // Staying silent costs a round trip while the client
                    // falls back to A.
                    RecordType::AAAA => answers.push(Record::from_rdata(
                        query.name().into(),
                        self.config.ttl,
                        RData::AAAA(AAAA(Ipv6Addr::LOCALHOST)),
                    )),
                    other => tracing::trace!("{other} is not handled: {name}"),
                }
            }
        }

        let empty: [Record; 0] = [];
        let response = builder.build(header, answers.iter(), &empty, &empty, &empty);

        match response_handle.send_response(response).await {
            Ok(info) => info,
            Err(err) => {
                tracing::warn!("cannot send the DNS response: {err}");
                ResponseInfo::from(header)
            }
        }
    }
}

/// Listens on both UDP and TCP.
///
/// TCP is required: answers can exceed 512 bytes, and some resolvers query
/// over TCP regardless.
pub async fn serve(
    addr: SocketAddr,
    config: DnsConfig,
    shutdown: Arc<tokio::sync::Notify>,
) -> std::io::Result<()> {
    let udp = tokio::net::UdpSocket::bind(addr).await?;
    let tcp = tokio::net::TcpListener::bind(addr).await?;

    serve_sockets(vec![udp], vec![tcp], config, shutdown).await
}

/// Runs on sockets that are already listening.
///
/// Used when the descriptors come from launchd's socket activation. :53 is
/// privileged, so a non-root daemon cannot bind it itself.
pub async fn serve_sockets(
    udp: Vec<tokio::net::UdpSocket>,
    tcp: Vec<tokio::net::TcpListener>,
    config: DnsConfig,
    shutdown: Arc<tokio::sync::Notify>,
) -> std::io::Result<()> {
    if udp.is_empty() && tcp.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no sockets to listen on",
        ));
    }

    let mut server = ServerFuture::new(KobuneDns::new(config));

    for socket in udp {
        server.register_socket(socket);
    }
    for listener in tcp {
        server.register_listener(listener, TCP_TIMEOUT);
    }

    tokio::select! {
        result = server.block_until_done() => {
            if let Err(err) = result {
                tracing::warn!("the DNS server stopped: {err}");
            }
        }
        _ = shutdown.notified() => {
            tracing::info!("stopping the DNS server");
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
            "suffix match, not substring match"
        );
        assert!(!config.serves(""));
        assert!(!config.serves("."));
    }

    #[test]
    fn honours_additional_suffixes() {
        let config = DnsConfig {
            suffixes: vec!["localhost".into(), "kobune.test".into()],
            ttl: 5,
        };

        assert!(config.serves("web.myapp.kobune.test"));
        assert!(config.serves("kobune.test"));
        assert!(!config.serves("kobune.test.evil.com"));
    }

    #[test]
    fn ttl_is_short_enough_to_follow_worktree_churn() {
        // Too long and clients keep reaching a workspace that is gone.
        assert!(DnsConfig::default().ttl <= 30);
    }
}
