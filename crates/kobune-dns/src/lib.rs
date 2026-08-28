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

use hickory_proto::op::{Header, HeaderCounts, MessageType, Metadata, OpCode, ResponseCode};
use hickory_proto::rr::rdata::{A, AAAA};
use hickory_proto::rr::{RData, Record, RecordType};
use hickory_server::Server;
use hickory_server::net::runtime::Time;
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use hickory_server::zone_handler::MessageResponseBuilder;

/// The TTL to put on answers, in seconds.
///
/// Kept short. Names come and go with worktrees, and a cached answer held
/// too long keeps pointing at an environment that is gone.
const DEFAULT_TTL: u32 = 5;

/// How long a TCP connection may idle.
const TCP_TIMEOUT: Duration = Duration::from_secs(10);

/// How many answers may be queued for one TCP connection.
///
/// Counted in responses, not bytes. Every answer here is a handful of
/// records built without any I/O, so the queue only ever holds what a
/// client has not yet read; this is the depth `hickory` itself uses.
const TCP_RESPONSE_QUEUE: usize = 32;

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
    async fn handle_request<R: ResponseHandler, T: Time>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        // `Request` derefs to the message, which is where the question and
        // the flags live. In 0.24 they were accessors on `Request` itself.
        let builder = MessageResponseBuilder::from_message_request(request);

        // 0.26 splits the old `Header` in two: `Metadata` is the identifier,
        // the flags and the codes, and the record counts are filled in by
        // whoever encodes the message. The builder wants the metadata.
        let mut metadata = Metadata::response_from_request(&request.metadata);
        metadata.authoritative = true;

        let mut answers: Vec<Record> = Vec::new();

        if request.metadata.op_code != OpCode::Query
            || request.metadata.message_type != MessageType::Query
        {
            // Anything other than a query — an update, say — is not handled.
            metadata.response_code = ResponseCode::NotImp;
        } else if let Some(query) = request.queries.queries().first() {
            let name = query.name().to_string();

            if !self.config.serves(&name) {
                tracing::trace!("query outside our scope: {name}");
                metadata.response_code = ResponseCode::NXDomain;
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
        } else {
            // A query with no question in it. 0.24 read the question
            // through `request_info`, which had already rejected this; the
            // question section is reachable directly now, so the case is
            // ours to answer.
            tracing::trace!("a query with no question section");
            metadata.response_code = ResponseCode::FormErr;
        }

        let empty: [Record; 0] = [];
        let response = builder.build(metadata, answers.iter(), &empty, &empty, &empty);

        match response_handle.send_response(response).await {
            Ok(info) => info,
            Err(err) => {
                tracing::warn!("cannot send the DNS response: {err}");
                // The counts belong to a message that was never encoded, so
                // they are zero. `Metadata` is `Copy`, so handing it to the
                // builder above did not consume it.
                ResponseInfo::from(Header {
                    metadata,
                    counts: HeaderCounts::default(),
                })
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

    let mut server = Server::new(KobuneDns::new(config));

    for socket in udp {
        server.register_socket(socket);
    }
    for listener in tcp {
        server.register_listener(listener, TCP_TIMEOUT, TCP_RESPONSE_QUEUE);
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
