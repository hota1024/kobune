use minato_api::{ApiError, ErrorCode};

pub type Result<T, E = RuntimeError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// runtime そのものに接続できない。Docker Desktop が起動していないなど。
    #[error("{runtime} に接続できません: {message}")]
    Unavailable { runtime: String, message: String },

    #[error("イメージ `{image}` を取得できません: {message}")]
    ImageUnavailable { image: String, message: String },

    #[error("{operation} に失敗しました: {message}")]
    Failed { operation: String, message: String },

    /// この runtime 実装では未対応の指定。
    #[error("{0}")]
    Unsupported(String),

    /// 仕様として成立していない入力。設定の書き方が悪い場合。
    #[error("{0}")]
    InvalidSpec(String),
}

impl RuntimeError {
    pub fn failed(operation: impl Into<String>, message: impl std::fmt::Display) -> Self {
        Self::Failed {
            operation: operation.into(),
            message: message.to_string(),
        }
    }
}

impl From<RuntimeError> for ApiError {
    fn from(err: RuntimeError) -> Self {
        let message = err.to_string();
        match err {
            RuntimeError::Unavailable { .. } => {
                ApiError::new(ErrorCode::RuntimeUnavailable, message).with_hint(
                    "コンテナランタイムが起動しているか確認してください\
                     （Docker Desktop / OrbStack / colima など）",
                )
            }
            RuntimeError::ImageUnavailable { .. } => {
                ApiError::new(ErrorCode::RuntimeFailed, message)
                    .with_hint("イメージ名が正しいか、ネットワークに接続できるか確認してください")
            }
            RuntimeError::Failed { .. } => ApiError::new(ErrorCode::RuntimeFailed, message),
            RuntimeError::Unsupported(_) => ApiError::new(ErrorCode::Unsupported, message),
            RuntimeError::InvalidSpec(_) => ApiError::new(ErrorCode::InvalidConfig, message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_suggests_starting_the_runtime() {
        let err = RuntimeError::Unavailable {
            runtime: "docker".into(),
            message: "connection refused".into(),
        };
        let api: ApiError = err.into();

        assert_eq!(api.code, ErrorCode::RuntimeUnavailable);
        assert!(api.code.is_retryable(), "起動すれば直るので再試行可能");
        assert!(api.hint.expect("hint がある").contains("Docker"));
    }

    #[test]
    fn invalid_spec_is_a_config_problem_not_a_runtime_one() {
        let api: ApiError = RuntimeError::InvalidSpec("volumes の書式が不正".into()).into();
        assert_eq!(api.code, ErrorCode::InvalidConfig);
        assert!(
            !api.code.is_retryable(),
            "設定を直さない限り再試行しても無駄"
        );
    }
}
