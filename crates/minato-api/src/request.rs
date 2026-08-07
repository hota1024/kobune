//! クライアントから daemon への要求。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 操作の対象を決める情報。
///
/// daemon は呼び出し元の作業ディレクトリを知らないため、クライアントが必ず渡す。
/// `cwd` から git リポジトリと `minato.toml` を解決し、`workspace` が
/// 省略されていれば `cwd` の属する worktree を対象にする。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub cwd: PathBuf,

    /// 明示指定する workspace ラベル。省略時は `cwd` から判定する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

impl Target {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            workspace: None,
        }
    }

    pub fn workspace(mut self, workspace: Option<String>) -> Self {
        self.workspace = workspace;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// 疎通確認とバージョン照合。
    Ping,

    /// daemon を終了する。
    Shutdown,

    /// 環境の診断。daemon 側で分かることを集めて返す。
    Doctor,

    /// workspace の一覧。
    Ls {
        target: Target,
        /// 現在のプロジェクトに限定せず、全プロジェクトを返す。
        #[serde(default)]
        all_projects: bool,
    },

    /// worktree を作り、環境を用意する。
    New {
        target: Target,
        /// チェックアウトするブランチ。存在しなければ `base` から作る。
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
        /// worktree を作るパス。省略時は規約に従って決める。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        /// 作成後にサービスを起動するか。
        #[serde(default = "yes")]
        start: bool,
    },

    /// worktree と環境を破棄する。
    Rm {
        target: Target,
        /// 未コミットの変更があっても削除する。
        #[serde(default)]
        force: bool,
    },

    /// サービスを起動する。
    Up {
        target: Target,
        /// 対象サービス。空なら全サービス。
        #[serde(default)]
        services: Vec<String>,
    },

    /// サービスを停止する。
    Down {
        target: Target,
        #[serde(default)]
        services: Vec<String>,
        /// プロジェクト内の全 workspace を停止する。
        #[serde(default)]
        all: bool,
    },

    /// workspace の現在の状態。
    Status { target: Target },

    /// ログを読む。出力は [`crate::Event::Output`] で流れる。
    Logs {
        target: Target,
        /// 対象サービス。省略時は全サービス。
        #[serde(default)]
        services: Vec<String>,
        /// 新しい行を待ち続ける。
        #[serde(default)]
        follow: bool,
        /// 末尾から何行取るか。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tail: Option<usize>,
    },

    /// コンテナ内でコマンドを実行する。
    Exec {
        target: Target,
        service: String,
        command: Vec<String>,
    },

    /// 環境変数の一覧。
    EnvList {
        target: Target,
        /// 値を伏せずに出す。既定では伏せる。
        #[serde(default)]
        reveal: bool,
    },

    /// 環境変数を設定する。
    EnvSet {
        target: Target,
        scope: minato_core::EnvScope,
        key: String,
        value: String,
    },

    /// 環境変数を削除する。
    EnvUnset {
        target: Target,
        scope: minato_core::EnvScope,
        key: String,
    },
}

fn yes() -> bool {
    true
}

impl Request {
    /// 進捗イベントを伴う長時間処理かどうか。
    ///
    /// クライアントは これが true のとき進捗表示を用意する。
    pub fn is_long_running(&self) -> bool {
        matches!(
            self,
            Self::New { .. }
                | Self::Up { .. }
                | Self::Down { .. }
                | Self::Rm { .. }
                | Self::Logs { .. }
                | Self::Exec { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_operations() {
        let json = serde_json::to_string(&Request::Ping).expect("serializes");
        assert_eq!(json, r#"{"op":"ping"}"#);
    }

    #[test]
    fn roundtrips_new_request() {
        let request = Request::New {
            target: Target::new(PathBuf::from("/repo")),
            branch: "feature/one".into(),
            base: None,
            path: None,
            start: true,
        };

        let json = serde_json::to_string(&request).expect("serializes");
        let back: Request = serde_json::from_str(&json).expect("deserializes");

        match back {
            Request::New { branch, start, .. } => {
                assert_eq!(branch, "feature/one");
                assert!(start);
            }
            other => panic!("想定外の variant: {other:?}"),
        }
    }

    #[test]
    fn start_defaults_to_true() {
        // `start` の既定が false だと `minato new` が環境を立ち上げなくなる。
        let request: Request =
            serde_json::from_str(r#"{"op":"new","target":{"cwd":"/repo"},"branch":"x"}"#)
                .expect("deserializes");

        match request {
            Request::New { start, .. } => assert!(start, "start は既定で true"),
            other => panic!("想定外の variant: {other:?}"),
        }
    }

    #[test]
    fn classifies_long_running_operations() {
        let target = Target::new(PathBuf::from("/repo"));
        assert!(!Request::Ping.is_long_running());
        assert!(
            !Request::Status {
                target: target.clone()
            }
            .is_long_running()
        );
        assert!(
            Request::Up {
                target,
                services: vec![]
            }
            .is_long_running()
        );
    }
}
