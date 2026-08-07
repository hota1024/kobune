use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("minato.toml が見つかりません: {0} 以下を探索しました")]
    ConfigNotFound(PathBuf),

    #[error("minato.toml の読み込みに失敗しました ({path}): {source}")]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("minato.toml の構文エラー ({path}): {source}")]
    ConfigParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// 構文としては正しいが、意味的に矛盾している設定。
    #[error("設定が不正です: {0}")]
    ConfigInvalid(String),

    #[error("git リポジトリではありません: {0}")]
    NotAGitRepository(PathBuf),

    #[error("git コマンドの実行に失敗しました: {0}")]
    GitSpawn(#[source] std::io::Error),

    #[error("git {args} が失敗しました (exit {code}): {stderr}")]
    GitFailed {
        args: String,
        code: i32,
        stderr: String,
    },

    #[error("workspace が見つかりません: {0}")]
    WorkspaceNotFound(String),

    #[error("workspace は既に存在します: {0}")]
    WorkspaceExists(String),

    #[error("サービスが見つかりません: {0}")]
    ServiceNotFound(String),

    #[error("状態ファイルの操作に失敗しました ({path}): {source}")]
    StateIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("状態ファイルが壊れています ({path}): {source}")]
    StateCorrupt {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("ホームディレクトリを特定できませんでした")]
    NoHomeDirectory,

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
