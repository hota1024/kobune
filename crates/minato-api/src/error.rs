//! ワイヤ上のエラー表現。
//!
//! `minato_core::Error` をそのまま送らず、コード・メッセージ・対処方法の
//! 3 つに分解する。`hint` はエージェントが次の行動を決めるための情報であり、
//! 単なる装飾ではない。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,

    /// どうすれば解消できるか。表示できるなら必ず表示する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unsupported, message)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ApiError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// workspace / サービス / プロジェクトが見つからない。
    NotFound,
    /// 既に存在するため作成できない。
    AlreadyExists,
    /// `minato.toml` が見つからない。
    ConfigNotFound,
    /// `minato.toml` の内容が不正。
    InvalidConfig,
    /// git リポジトリの外で実行された。
    NotAGitRepository,
    /// runtime（Docker など）に接続できない。
    RuntimeUnavailable,
    /// runtime の操作が失敗した。
    RuntimeFailed,
    /// この版では未実装の機能。
    Unsupported,
    /// 呼び出し側によって中断された。
    Cancelled,
    /// 想定外の内部エラー。
    Internal,
}

impl ErrorCode {
    /// CLI がプロセス終了コードとして返す値。
    ///
    /// エージェントが出力をパースせずに失敗の種類を判別できるようにする。
    /// 1 は汎用エラー、2 は clap の usage エラーに予約されているため 4 から始める。
    pub fn exit_code(self) -> i32 {
        match self {
            Self::NotFound => 4,
            Self::AlreadyExists => 5,
            Self::ConfigNotFound => 6,
            Self::InvalidConfig => 7,
            Self::NotAGitRepository => 8,
            Self::RuntimeUnavailable => 9,
            Self::RuntimeFailed => 10,
            Self::Unsupported => 11,
            Self::Cancelled => 130,
            Self::Internal => 70,
        }
    }

    /// 同じ操作を再試行して解決する見込みがあるか。
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::RuntimeUnavailable | Self::RuntimeFailed)
    }
}

impl From<minato_core::Error> for ApiError {
    fn from(err: minato_core::Error) -> Self {
        use minato_core::Error as E;

        let message = err.to_string();
        match err {
            E::ConfigNotFound(_) => Self::new(ErrorCode::ConfigNotFound, message).with_hint(
                "プロジェクトのルートで `minato init` を実行して minato.toml を作成してください",
            ),
            E::ConfigParse { .. } | E::ConfigInvalid(_) => {
                Self::new(ErrorCode::InvalidConfig, message)
            }
            E::ConfigRead { .. } => Self::new(ErrorCode::InvalidConfig, message),
            E::NotAGitRepository(_) => Self::new(ErrorCode::NotAGitRepository, message)
                .with_hint("git 管理下のディレクトリで実行してください"),
            E::WorkspaceNotFound(_) => Self::new(ErrorCode::NotFound, message)
                .with_hint("`minato ls` で利用できる workspace を確認してください"),
            E::ServiceNotFound(_) => Self::new(ErrorCode::NotFound, message)
                .with_hint("minato.toml の [services] に定義されている名前を指定してください"),
            E::WorkspaceExists(_) => Self::new(ErrorCode::AlreadyExists, message),
            E::GitSpawn(_) => Self::new(ErrorCode::Internal, message)
                .with_hint("git がインストールされ、PATH に含まれているか確認してください"),
            E::GitFailed { .. } => Self::new(ErrorCode::Internal, message),
            E::StateIo { .. } | E::StateCorrupt { .. } | E::NoHomeDirectory | E::Io(_) => {
                Self::new(ErrorCode::Internal, message)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn exit_codes_are_distinct() {
        let codes = [
            ErrorCode::NotFound,
            ErrorCode::AlreadyExists,
            ErrorCode::ConfigNotFound,
            ErrorCode::InvalidConfig,
            ErrorCode::NotAGitRepository,
            ErrorCode::RuntimeUnavailable,
            ErrorCode::RuntimeFailed,
            ErrorCode::Unsupported,
            ErrorCode::Internal,
        ];

        let mut seen = std::collections::BTreeSet::new();
        for code in codes {
            let exit = code.exit_code();
            assert!(
                seen.insert(exit),
                "終了コード {exit} が重複している: {code:?}"
            );
            assert_ne!(exit, 0, "エラーが成功扱いになっている: {code:?}");
            assert_ne!(exit, 2, "2 は clap の usage エラーに予約: {code:?}");
        }
    }

    #[test]
    fn missing_config_carries_actionable_hint() {
        let err: ApiError = minato_core::Error::ConfigNotFound(PathBuf::from("/repo")).into();
        assert_eq!(err.code, ErrorCode::ConfigNotFound);
        assert!(
            err.hint.expect("hint がある").contains("minato init"),
            "次に何をすべきかを示す必要がある"
        );
    }

    #[test]
    fn maps_core_errors_to_codes() {
        let cases: Vec<(minato_core::Error, ErrorCode)> = vec![
            (
                minato_core::Error::NotAGitRepository(PathBuf::from("/tmp")),
                ErrorCode::NotAGitRepository,
            ),
            (
                minato_core::Error::WorkspaceNotFound("feat-1".into()),
                ErrorCode::NotFound,
            ),
            (
                minato_core::Error::ServiceNotFound("web".into()),
                ErrorCode::NotFound,
            ),
            (
                minato_core::Error::ConfigInvalid("bad".into()),
                ErrorCode::InvalidConfig,
            ),
            (
                minato_core::Error::WorkspaceExists("feat-1".into()),
                ErrorCode::AlreadyExists,
            ),
        ];

        for (err, expected) in cases {
            let api: ApiError = err.into();
            assert_eq!(api.code, expected);
            assert!(!api.message.is_empty());
        }
    }

    #[test]
    fn runtime_errors_are_retryable() {
        assert!(ErrorCode::RuntimeUnavailable.is_retryable());
        assert!(!ErrorCode::InvalidConfig.is_retryable());
    }
}
