//! リバースプロキシと、そのためのローカル CA。
//!
//! プロキシは runtime の実装を知らない。[`Routes`] に入っている
//! `SocketAddr` へ転送するだけで、それがホストのフォワードポートか
//! コンテナ自身の IP かは区別しない。

pub mod ca;
pub mod proxy;
pub mod routes;
pub mod server;

pub use ca::{CA_CERT_FILE, CA_KEY_FILE, CaError, DynamicCertResolver, LocalCa, server_config};
pub use routes::{Route, Routes, normalize_host};
pub use server::{serve_http, serve_https};

/// 既定の HTTP 待ち受けポート。
pub const DEFAULT_HTTP_PORT: u16 = 80;

/// 既定の HTTPS 待ち受けポート。
pub const DEFAULT_HTTPS_PORT: u16 = 443;
