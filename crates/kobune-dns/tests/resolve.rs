//! Sends real DNS queries and checks the answers.
//!
//! Whether `curl` can resolve a name decides whether any of this works, so
//! verify against the actual wire format.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use hickory_proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::{DNSClass, Name, RData, RecordType};
use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};
use kobune_dns::{DnsConfig, serve};
use tokio::net::UdpSocket;
use tokio::sync::Notify;

/// Starts the server and returns the address it listens on.
async fn spawn_dns(config: DnsConfig) -> SocketAddr {
    // Bind port 0 to find a free port, then reuse the number. serve()
    // opens both UDP and TCP, so release it first.
    let probe = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let addr = probe.local_addr().expect("addr");
    drop(probe);

    let shutdown = Arc::new(Notify::new());
    tokio::spawn(async move {
        let _ = serve(addr, config, shutdown).await;
    });

    // Give the server a moment to start listening.
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if UdpSocket::bind(addr).await.is_err() {
            // Cannot bind means the server holds it.
            break;
        }
    }

    addr
}

/// Sends one query and reads the answer.
async fn query(server: SocketAddr, name: &str, record_type: RecordType) -> Message {
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind");

    // 0.26 takes the header contents at construction rather than through
    // setters, and the flags are fields on the metadata.
    let mut message = Message::new(0x1234, MessageType::Query, OpCode::Query);
    message.metadata.recursion_desired = true;

    let mut question = Query::new();
    question
        .set_name(Name::from_ascii(name).expect("a valid name"))
        .set_query_type(record_type)
        .set_query_class(DNSClass::IN);
    message.add_query(question);

    let bytes = message.to_bytes().expect("encodes");
    socket.send_to(&bytes, server).await.expect("sends");

    let mut buffer = vec![0u8; 4096];
    let (len, _) = tokio::time::timeout(Duration::from_secs(3), socket.recv_from(&mut buffer))
        .await
        .expect("an answer arrives")
        .expect("receives");

    Message::from_bytes(&buffer[..len]).expect("decodes")
}

#[tokio::test]
async fn resolves_nested_localhost_names_to_loopback() {
    let server = spawn_dns(DnsConfig::default()).await;

    // Without this curl is useless and an agent cannot verify anything.
    let response = query(server, "web.feat-1.myapp.localhost.", RecordType::A).await;

    assert_eq!(response.metadata.response_code, ResponseCode::NoError);
    assert_eq!(response.answers.len(), 1);

    match &response.answers[0].data {
        RData::A(address) => assert_eq!(address.0, Ipv4Addr::LOCALHOST),
        other => panic!("expected an A record: {other:?}"),
    }
}

#[tokio::test]
async fn resolves_unknown_hosts_too() {
    // Resolves even without a route. Reaching the proxy for a 404 is far
    // easier to diagnose than a name that does not resolve.
    let server = spawn_dns(DnsConfig::default()).await;
    let response = query(server, "never-created.myapp.localhost.", RecordType::A).await;

    assert_eq!(response.metadata.response_code, ResponseCode::NoError);
    assert_eq!(response.answers.len(), 1);
}

#[tokio::test]
async fn resolves_aaaa_to_ipv6_loopback() {
    // The proxy also listens on ::1. Staying silent costs a round trip
    // while the client falls back to A.
    let server = spawn_dns(DnsConfig::default()).await;
    let response = query(server, "web.myapp.localhost.", RecordType::AAAA).await;

    assert_eq!(response.metadata.response_code, ResponseCode::NoError);
    assert_eq!(response.answers.len(), 1);

    match &response.answers[0].data {
        RData::AAAA(address) => {
            assert_eq!(address.0, std::net::Ipv6Addr::LOCALHOST)
        }
        other => panic!("expected an AAAA record: {other:?}"),
    }
}

#[tokio::test]
async fn refuses_names_outside_its_scope() {
    let server = spawn_dns(DnsConfig::default()).await;
    let response = query(server, "example.com.", RecordType::A).await;

    assert_eq!(response.metadata.response_code, ResponseCode::NXDomain);
    assert!(response.answers.is_empty());
}

#[tokio::test]
async fn serves_configured_suffixes() {
    let server = spawn_dns(DnsConfig {
        suffixes: vec!["kobune.test".into()],
        ttl: 5,
    })
    .await;

    let served = query(server, "web.myapp.kobune.test.", RecordType::A).await;
    assert_eq!(served.metadata.response_code, ResponseCode::NoError);
    assert_eq!(served.answers.len(), 1);

    // localhost was left out of the configuration, so it is not served.
    let not_served = query(server, "web.myapp.localhost.", RecordType::A).await;
    assert_eq!(not_served.metadata.response_code, ResponseCode::NXDomain);
}

#[tokio::test]
async fn preserves_the_query_id() {
    // A resolver discards answers whose ID does not match.
    let server = spawn_dns(DnsConfig::default()).await;
    let response = query(server, "web.myapp.localhost.", RecordType::A).await;

    assert_eq!(response.metadata.id, 0x1234);
    assert_eq!(response.metadata.message_type, MessageType::Response);
}
