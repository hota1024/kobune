//! tokio スレッドと描画スレッドの橋渡し。
//!
//! egui の描画ループは同期なので、daemon との通信は別スレッドの
//! tokio ランタイムで回す。状態は [`SharedState`] に書き、
//! `request_repaint` で描画側に知らせる。
//!
//! **イベントを受けたときだけ再描画を要求する。** 常時再描画すると
//! 何もしていない間も CPU を回し続けることになる。

use std::path::{Path, PathBuf};
use std::time::Duration;

use minato_api::{Event, OutputStream, Request, Response, Target};
use minato_client::Client;

use crate::state::{Connection, LogLine, SharedState};

/// 一覧を取り直す間隔。
///
/// scale-to-zero でサービスが止まるのを画面に反映するため、
/// 触っていなくても定期的に見に行く。
const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

/// 接続に失敗したあと、次に試すまでの間隔。
const RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// 描画側から tokio 側への依頼。
#[derive(Debug, Clone)]
pub enum Command {
    /// 一覧を今すぐ取り直す。
    Refresh,
    /// この workspace のログを追う。
    FollowLogs { workspace: String },
    /// ログの購読をやめる。
    StopLogs,
}

/// tokio 側を起動し、依頼を送るためのハンドルを返す。
pub fn spawn(
    state: SharedState,
    cwd: PathBuf,
    ctx: egui::Context,
) -> tokio::sync::mpsc::UnboundedSender<Command> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();

    std::thread::Builder::new()
        .name("minato-bridge".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    state.write(|state| {
                        state.connection =
                            Some(Connection::Failed(format!("実行環境を作れません: {err}")));
                    });
                    ctx.request_repaint();
                    return;
                }
            };

            runtime.block_on(run(state, cwd, ctx, receiver));
        })
        .expect("スレッドを作れる");

    sender
}

async fn run(
    state: SharedState,
    cwd: PathBuf,
    ctx: egui::Context,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
) {
    let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
    let mut log_task: Option<tokio::task::JoinHandle<()>> = None;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                refresh(&state, &cwd, &ctx).await;
            }
            command = commands.recv() => {
                match command {
                    Some(Command::Refresh) => refresh(&state, &cwd, &ctx).await,
                    Some(Command::FollowLogs { workspace }) => {
                        // 前の購読を止めてから始める。放置すると
                        // 複数の workspace のログが混ざる。
                        if let Some(task) = log_task.take() {
                            task.abort();
                        }

                        state.write(|state| {
                            state.clear_logs();
                            state.log_target = Some(workspace.clone());
                        });

                        log_task = Some(tokio::spawn(follow_logs(
                            state.clone(),
                            cwd.clone(),
                            ctx.clone(),
                            workspace,
                        )));
                    }
                    Some(Command::StopLogs) => {
                        if let Some(task) = log_task.take() {
                            task.abort();
                        }
                        state.write(|state| state.log_target = None);
                        ctx.request_repaint();
                    }
                    // 描画側が終了した。
                    None => {
                        if let Some(task) = log_task.take() {
                            task.abort();
                        }
                        return;
                    }
                }
            }
        }
    }
}

/// workspace の一覧を取り直す。
async fn refresh(state: &SharedState, cwd: &Path, ctx: &egui::Context) {
    let client = match Client::from_env() {
        Ok(client) => client,
        Err(err) => {
            set_failed(state, ctx, format!("設定を解決できません: {err}"));
            return;
        }
    };

    // GUI から daemon を起動しない。daemon の面倒を見るのは launchd の
    // 仕事で、GUI が二重に管理すると責務が重なる（`docs/DESIGN.md` §15）。
    let mut connection = match client.connect().await {
        Ok(connection) => connection,
        Err(err) => {
            set_failed(state, ctx, err.to_string());
            tokio::time::sleep(RETRY_INTERVAL).await;
            return;
        }
    };

    match connection.handshake().await {
        Ok(pong) => state.write(|state| {
            state.connection = Some(Connection::Connected(Box::new(pong)));
        }),
        Err(err) => {
            set_failed(state, ctx, err.to_string());
            return;
        }
    }

    let request = Request::Ls {
        target: Target::new(cwd.to_path_buf()),
        all_projects: false,
    };

    match connection.request(request).await {
        Ok(Response::Workspaces { workspaces }) => {
            tracing::debug!("workspace を {} 件取得しました", workspaces.len());
            state.write(|state| {
                state.workspaces = workspaces;
                state.error = None;
            });
        }
        Ok(_) => state.write(|state| {
            state.error = Some("想定外の応答を受け取りました".to_string());
        }),
        Err(err) => state.write(|state| {
            // 接続はできているので、これは一覧固有の失敗。
            // minato.toml が無いディレクトリで起動した場合など。
            state.error = Some(err.to_string());
            state.workspaces.clear();
        }),
    }

    ctx.request_repaint();
}

/// ログを追い続け、届いた行を状態に積む。
async fn follow_logs(state: SharedState, cwd: PathBuf, ctx: egui::Context, workspace: String) {
    let Ok(client) = Client::from_env() else {
        return;
    };

    let Ok(mut connection) = client.connect().await else {
        state.write(|state| {
            state.push_log(LogLine {
                service: "minato".into(),
                line: "daemon に接続できません".into(),
                is_error: true,
            });
        });
        ctx.request_repaint();
        return;
    };

    let request = Request::Logs {
        target: Target::new(cwd).workspace(Some(workspace)),
        services: Vec::new(),
        follow: true,
        tail: Some(200),
    };

    let outcome = connection
        .call(request, |event| {
            if let Event::Output {
                service,
                stream,
                line,
            } = event
            {
                state.write(|state| {
                    state.push_log(LogLine {
                        service: service.unwrap_or_else(|| "-".to_string()),
                        line,
                        is_error: stream == OutputStream::Stderr,
                    });
                });

                ctx.request_repaint();
            }
        })
        .await;

    if let Err(err) = outcome {
        state.write(|state| {
            state.push_log(LogLine {
                service: "minato".into(),
                line: format!("ログの購読が終了しました: {err}"),
                is_error: true,
            });
        });
        ctx.request_repaint();
    }
}

fn set_failed(state: &SharedState, ctx: &egui::Context, reason: String) {
    // 画面に出すだけだと、GUI が繋がらないときログに何も残らない。
    tracing::warn!("daemon に接続できません: {reason}");

    state.write(|state| {
        state.connection = Some(Connection::Failed(reason));
        state.workspaces.clear();
    });
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_interval_follows_scale_to_zero() {
        // アイドル停止が画面に反映されないと、止まっているのに
        // 動いているように見える。
        assert!(REFRESH_INTERVAL <= Duration::from_secs(5));
    }

    #[test]
    fn retry_is_slower_than_refresh() {
        // 繋がらない相手に同じ頻度で試し続けない。
        assert!(RETRY_INTERVAL > REFRESH_INTERVAL);
    }
}
