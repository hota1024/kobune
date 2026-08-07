//! tokio スレッドと UI の橋渡し。
//!
//! `minato-client` は tokio の上でしか動かないが、GPUI は独自の
//! executor を持つ。両者を混ぜず、tokio は専用スレッドで回して
//! 結果だけを渡す。
//!
//! **UI フレームワークに依存しない。** 通知は [`Notifier`] という
//! 単なるチャネルで行い、それを描画に繋ぐのは UI 側の仕事にする。

use std::path::{Path, PathBuf};
use std::time::Duration;

use minato_api::{Event, OutputStream, Request, Response, Target};
use minato_client::Client;

use crate::state::{Connection, LogLine, SharedState};

/// 状態が変わったことを UI に伝える。
///
/// 何が変わったかは伝えない。UI は [`SharedState`] を読み直せばよく、
/// 差分を運ぶ設計にすると両者が密になる。
#[derive(Clone)]
pub struct Notifier(tokio::sync::mpsc::UnboundedSender<()>);

impl Notifier {
    pub fn channel() -> (Self, tokio::sync::mpsc::UnboundedReceiver<()>) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (Self(sender), receiver)
    }

    /// 通知する。受け手が居なくても失敗にしない。
    pub fn notify(&self) {
        let _ = self.0.send(());
    }
}

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
    /// サービスを起動する。
    Up { workspace: String },
    /// サービスを停止する。
    Down { workspace: String },
}

/// tokio 側を起動し、依頼を送るためのハンドルを返す。
pub fn spawn(
    state: SharedState,
    cwd: PathBuf,
    notifier: Notifier,
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
                        state.connection = Some(Connection::Failed(format!(
                            "cannot create the runtime: {err}"
                        )));
                    });
                    notifier.notify();
                    return;
                }
            };

            runtime.block_on(run(state, cwd, notifier, receiver));
        })
        .expect("スレッドを作れる");

    sender
}

async fn run(
    state: SharedState,
    cwd: PathBuf,
    notifier: Notifier,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
) {
    let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
    let mut log_task: Option<tokio::task::JoinHandle<()>> = None;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                refresh(&state, &cwd, &notifier).await;
            }
            command = commands.recv() => {
                match command {
                    Some(Command::Refresh) => refresh(&state, &cwd, &notifier).await,
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
                            notifier.clone(),
                            workspace,
                        )));
                    }
                    Some(Command::Up { workspace }) => {
                        operate(&state, &cwd, &notifier, workspace, true).await;
                    }
                    Some(Command::Down { workspace }) => {
                        operate(&state, &cwd, &notifier, workspace, false).await;
                    }
                    Some(Command::StopLogs) => {
                        if let Some(task) = log_task.take() {
                            task.abort();
                        }
                        state.write(|state| state.log_target = None);
                        notifier.notify();
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
async fn refresh(state: &SharedState, cwd: &Path, notifier: &Notifier) {
    let client = match Client::from_env() {
        Ok(client) => client,
        Err(err) => {
            set_failed(
                state,
                notifier,
                format!("cannot resolve the configuration: {err}"),
            );
            return;
        }
    };

    // GUI から daemon を起動しない。daemon の面倒を見るのは launchd の
    // 仕事で、GUI が二重に管理すると責務が重なる（`docs/DESIGN.md` §15）。
    let mut connection = match client.connect().await {
        Ok(connection) => connection,
        Err(err) => {
            set_failed(state, notifier, err.to_string());
            tokio::time::sleep(RETRY_INTERVAL).await;
            return;
        }
    };

    match connection.handshake().await {
        Ok(pong) => state.write(|state| {
            state.connection = Some(Connection::Connected(Box::new(pong)));
        }),
        Err(err) => {
            set_failed(state, notifier, err.to_string());
            return;
        }
    }

    let request = Request::Ls {
        target: Target::new(cwd.to_path_buf()),
        all_projects: false,
    };

    match connection.request(request).await {
        Ok(Response::Workspaces { workspaces }) => {
            tracing::debug!("fetched {} workspaces", workspaces.len());
            state.write(|state| {
                state.workspaces = workspaces;
                state.error = None;
            });
        }
        Ok(_) => state.write(|state| {
            state.error = Some("unexpected response from the daemon".to_string());
        }),
        Err(err) => state.write(|state| {
            // 接続はできているので、これは一覧固有の失敗。
            // minato.toml が無いディレクトリで起動した場合など。
            state.error = Some(err.to_string());
            state.workspaces.clear();
        }),
    }

    notifier.notify();
}

/// サービスを起動／停止する。
///
/// 完了まで待ってから一覧を取り直す。押した直後に見た目が変わらないと
/// 壊れて見えるので、処理中であることを状態に残す。
async fn operate(
    state: &SharedState,
    cwd: &Path,
    notifier: &Notifier,
    workspace: String,
    start: bool,
) {
    state.write(|state| {
        state.busy.insert(workspace.clone());
    });
    notifier.notify();

    let outcome = run_operation(cwd, &workspace, start).await;

    state.write(|state| {
        state.busy.remove(&workspace);

        if let Err(err) = &outcome {
            state.error = Some(err.clone());
        }
    });

    refresh(state, cwd, notifier).await;
}

async fn run_operation(cwd: &Path, workspace: &str, start: bool) -> Result<(), String> {
    let client = Client::from_env().map_err(|err| err.to_string())?;
    let mut connection = client.connect().await.map_err(|err| err.to_string())?;

    let target = Target::new(cwd.to_path_buf()).workspace(Some(workspace.to_string()));
    let request = if start {
        Request::Up {
            target,
            services: Vec::new(),
        }
    } else {
        Request::Down {
            target,
            services: Vec::new(),
            all: false,
        }
    };

    connection
        .request(request)
        .await
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// ログを追い続け、届いた行を状態に積む。
async fn follow_logs(state: SharedState, cwd: PathBuf, notifier: Notifier, workspace: String) {
    let Ok(client) = Client::from_env() else {
        return;
    };

    let Ok(mut connection) = client.connect().await else {
        state.write(|state| {
            state.push_log(LogLine {
                service: "minato".into(),
                line: "cannot connect to the daemon".into(),
                is_error: true,
            });
        });
        notifier.notify();
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

                notifier.notify();
            }
        })
        .await;

    if let Err(err) = outcome {
        state.write(|state| {
            state.push_log(LogLine {
                service: "minato".into(),
                line: format!("log stream ended: {err}"),
                is_error: true,
            });
        });
        notifier.notify();
    }
}

fn set_failed(state: &SharedState, notifier: &Notifier, reason: String) {
    // 画面に出すだけだと、GUI が繋がらないときログに何も残らない。
    tracing::warn!("cannot connect to the daemon: {reason}");

    state.write(|state| {
        state.connection = Some(Connection::Failed(reason));
        state.workspaces.clear();
    });
    notifier.notify();
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
