//! minatod — Minato の常駐プロセス。
//!
//! ポート台帳・リバースプロキシ・DNS・アイドル監視はいずれも常駐が要るため
//! daemon を置く。M0 の時点ではまだ Supervisor しかいないが、以降の
//! マイルストーンはここに機能を足していく形になる。

mod activation;
mod gateway;
mod resolve;
mod server;
mod spec;
mod supervisor;

use std::sync::Arc;

use clap::Parser;
use minato_core::Paths;
use tokio::sync::Notify;
use tracing_subscriber::EnvFilter;

use crate::gateway::{Gateway, GatewaySettings};
use crate::server::Server;
use crate::supervisor::Supervisor;

#[derive(Parser, Debug)]
#[command(name = "minatod", version, about = "Minato の常駐プロセス")]
struct Args {
    /// ログを標準エラーにも出す。手元でのデバッグ用。
    #[arg(long)]
    foreground: bool,

    /// ログの詳細度。`MINATO_LOG` でも指定できる。
    #[arg(long, default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let paths = Paths::resolve()?;
    paths.ensure()?;

    // bind してからだと `SUN_LEN` という原因の分からないエラーになる。
    // ログの初期化より前に弾いて、標準エラーにも必ず出す。
    if let Err(err) = paths.check_socket_length() {
        eprintln!("エラー: {err}");
        return Err(err.into());
    }

    init_logging(&args, &paths)?;

    tracing::info!(
        "minatod {} を起動します (protocol {})",
        env!("CARGO_PKG_VERSION"),
        minato_api::PROTOCOL_VERSION
    );

    let shutdown = Arc::new(Notify::new());

    // プロキシと DNS を先に立てる。bind に失敗しても daemon は動かす。
    // その場合 URL は発行されず、ポート直指定の endpoint だけになる。
    let settings = GatewaySettings::from_env();
    let gateway = Arc::new(Gateway::start(&paths, &settings, shutdown.clone()).await);

    if !gateway.is_serving() {
        tracing::warn!(
            "プロキシが待ち受けられていないため URL は発行されません。\
             `minato doctor` を確認してください"
        );
    }

    let supervisor = Arc::new(Supervisor::new(&paths, gateway, shutdown.clone()));
    let server = Server::new(paths.socket(), supervisor, shutdown.clone());

    // シグナルでも Request::Shutdown でも同じ経路で止める。
    spawn_signal_handler(shutdown.clone());

    let result = server.run().await;

    let _ = std::fs::remove_file(paths.pid_file());
    tracing::info!("minatod を終了します");

    result
}

fn init_logging(args: &Args, paths: &Paths) -> anyhow::Result<()> {
    let filter =
        EnvFilter::try_from_env("MINATO_LOG").unwrap_or_else(|_| EnvFilter::new(&args.log_level));

    // CLI から起動された daemon は標準出力が閉じられているため、
    // ログはファイルに残さないと何も分からなくなる。
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.daemon_log())?;

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false);

    if args.foreground {
        subscriber.with_writer(std::io::stderr).init();
    } else {
        subscriber.with_writer(log_file).init();
    }

    Ok(())
}

fn spawn_signal_handler(shutdown: Arc<Notify>) {
    tokio::spawn(async move {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(err) => {
                    tracing::warn!("SIGTERM を待ち受けられません: {err}");
                    return;
                }
            };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT を受信しました"),
            _ = terminate.recv() => tracing::info!("SIGTERM を受信しました"),
        }

        shutdown.notify_waiters();
    });
}
