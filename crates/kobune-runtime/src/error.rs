use minato_api::{ApiError, ErrorCode};

pub type Result<T, E = RuntimeError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// The runtime itself is unreachable — Docker Desktop is not running,
    /// for instance.
    #[error("cannot reach {runtime}: {message}")]
    Unavailable { runtime: String, message: String },

    #[error("cannot pull the image `{image}`: {message}")]
    ImageUnavailable { image: String, message: String },

    #[error("{operation} failed: {message}")]
    Failed { operation: String, message: String },

    /// Something this runtime implementation does not support.
    #[error("{0}")]
    Unsupported(String),

    /// An input that is not a coherent spec — the configuration is wrong.
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
                    "check that the container runtime is running \
                     (Docker Desktop, OrbStack, colima, …)",
                )
            }
            RuntimeError::ImageUnavailable { .. } => {
                ApiError::new(ErrorCode::RuntimeFailed, message)
                    .with_hint("check that the image name is right and the network is up")
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
        assert!(
            api.code.is_retryable(),
            "starting it fixes this, so retrying is worth it"
        );
        assert!(api.hint.expect("has a hint").contains("Docker"));
    }

    #[test]
    fn invalid_spec_is_a_config_problem_not_a_runtime_one() {
        let api: ApiError = RuntimeError::InvalidSpec("volumes is malformed".into()).into();
        assert_eq!(api.code, ErrorCode::InvalidConfig);
        assert!(
            !api.code.is_retryable(),
            "retrying gets nowhere until the config is fixed"
        );
    }
}
