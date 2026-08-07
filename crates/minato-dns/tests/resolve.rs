//! 本物の DNS クエリを投げて応答を確かめる。
//!
//! `curl` が名前を引けるかどうかが M1 の成否を分けるので、
//! 実際のワイヤフォーマットで検証する。

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use hickory_proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::{DNSClass, Name, RData, RecordType};
use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};
use minato_dns::{DnsConfig, serve};
use tokio::net::UdpSocket;
use tokio::sync::Notify;

/// DNS サーバを起動して待ち受けアドレスを返す。
async fn spawn_dns(config: DnsConfig) -> SocketAddr {
    // ポート 0 で bind して空きを見つけ、その番号を使い直す。
    // serve() が UDP と TCP の両方を開くため、いったん解放する。
    let probe = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let addr = probe.local_addr().expect("addr");
    drop(probe);

    let shutdown = Arc::new(Notify::new());
    tokio::spawn(async move {
        let _ = serve(addr, config, shutdown).await;
    });

    // 待ち受けが始まるまで少し待つ。
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if UdpSocket::bind(addr).await.is_err() {
            // bind できない = サーバが掴んでいる。
            break;
        }
    }

    addr
}

/// 1 問い合わせを送って応答を受け取る。
async fn query(server: SocketAddr, name: &str, record_type: RecordType) -> Message {
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind");

    let mut message = Message::new();
    message
        .set_id(0x1234)
        .set_message_type(MessageType::Query)
        .set_op_code(OpCode::Query)
        .set_recursion_desired(true);

    let mut question = Query::new();
    question
        .set_name(Name::from_ascii(name).expect("名前として妥当"))
        .set_query_type(record_type)
        .set_query_class(DNSClass::IN);
    message.add_query(question);

    let bytes = message.to_bytes().expect("符号化できる");
    socket.send_to(&bytes, server).await.expect("送れる");

    let mut buffer = vec![0u8; 4096];
    let (len, _) = tokio::time::timeout(Duration::from_secs(3), socket.recv_from(&mut buffer))
        .await
        .expect("応答が返る")
        .expect("受け取れる");

    Message::from_bytes(&buffer[..len]).expect("復号できる")
}

#[tokio::test]
async fn resolves_nested_localhost_names_to_loopback() {
    let server = spawn_dns(DnsConfig::default()).await;

    // これが引けないと curl が使えず、エージェントが確認できない。
    let response = query(server, "web.feat-1.myapp.localhost.", RecordType::A).await;

    assert_eq!(response.response_code(), ResponseCode::NoError);
    assert_eq!(response.answers().len(), 1);

    match response.answers()[0].data() {
        Some(RData::A(address)) => assert_eq!(address.0, Ipv4Addr::LOCALHOST),
        other => panic!("A レコードが返るべき: {other:?}"),
    }
}

#[tokio::test]
async fn resolves_unknown_hosts_too() {
    // ルートが無くても解決する。プロキシまで届かせて 404 で案内する方が、
    // 名前解決の失敗より切り分けやすい。
    let server = spawn_dns(DnsConfig::default()).await;
    let response = query(server, "never-created.myapp.localhost.", RecordType::A).await;

    assert_eq!(response.response_code(), ResponseCode::NoError);
    assert_eq!(response.answers().len(), 1);
}

#[tokio::test]
async fn resolves_aaaa_to_ipv6_loopback() {
    // プロキシは ::1 でも待ち受ける。答えないとクライアントが A に
    // フォールバックするまで一往復ぶん遅れる。
    let server = spawn_dns(DnsConfig::default()).await;
    let response = query(server, "web.myapp.localhost.", RecordType::AAAA).await;

    assert_eq!(response.response_code(), ResponseCode::NoError);
    assert_eq!(response.answers().len(), 1);

    match response.answers()[0].data() {
        Some(RData::AAAA(address)) => {
            assert_eq!(address.0, std::net::Ipv6Addr::LOCALHOST)
        }
        other => panic!("AAAA レコードが返るべき: {other:?}"),
    }
}

#[tokio::test]
async fn refuses_names_outside_its_scope() {
    let server = spawn_dns(DnsConfig::default()).await;
    let response = query(server, "example.com.", RecordType::A).await;

    assert_eq!(response.response_code(), ResponseCode::NXDomain);
    assert!(response.answers().is_empty());
}

#[tokio::test]
async fn serves_configured_suffixes() {
    let server = spawn_dns(DnsConfig {
        suffixes: vec!["minato.test".into()],
        ttl: 5,
    })
    .await;

    let served = query(server, "web.myapp.minato.test.", RecordType::A).await;
    assert_eq!(served.response_code(), ResponseCode::NoError);
    assert_eq!(served.answers().len(), 1);

    // 既定の localhost は設定から外したので受け持たない。
    let not_served = query(server, "web.myapp.localhost.", RecordType::A).await;
    assert_eq!(not_served.response_code(), ResponseCode::NXDomain);
}

#[tokio::test]
async fn preserves_the_query_id() {
    // ID が一致しないと resolver は応答を捨てる。
    let server = spawn_dns(DnsConfig::default()).await;
    let response = query(server, "web.myapp.localhost.", RecordType::A).await;

    assert_eq!(response.id(), 0x1234);
    assert_eq!(response.message_type(), MessageType::Response);
}
