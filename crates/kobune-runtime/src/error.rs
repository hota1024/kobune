use kobune_api::{ApiError, ErrorCode};

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

    /// [`RuntimeError::failed`], keeping what is behind the error as well
    /// as the error. See [`with_causes`].
    pub fn caused_by(
        operation: impl Into<String>,
        err: &(dyn std::error::Error + 'static),
    ) -> Self {
        Self::Failed {
            operation: operation.into(),
            message: with_causes(err),
        }
    }
}

/// An error, and everything behind it.
///
/// **`Display` is only ever the top of the chain**, and the libraries under
/// this one keep the answer at the bottom of it. `hyper`'s client prints
/// `client error (SendRequest)` — literally `write!(f, "client error
/// ({:?})", self.kind)` — and hangs the `Invalid argument (os error 22)`
/// that caused it off `source()`. `bollard` wraps that in `Error in the
/// hyper legacy client: {err}`, passing the useless half along and dropping
/// the rest. A build failing on a context too large to write reported those
/// seven words and nothing that led anywhere, and no amount of retrying was
/// going to add to them.
///
/// A cause that only repeats what wrapped it is left out, so the common
/// `#[error("{0}")]` wrapper does not say everything twice.
pub fn with_causes(err: &(dyn std::error::Error + 'static)) -> String {
    let mut message = err.to_string();
    let mut cause = err.source();

    while let Some(next) = cause {
        let text = next.to_string();
        if !text.is_empty() && !message.ends_with(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        cause = next.source();
    }

    message
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

    /// A stand-in for the two-deep wrapping `bollard` does over `hyper`.
    #[derive(Debug)]
    struct Layer(&'static str, Option<Box<Layer>>);

    impl std::fmt::Display for Layer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for Layer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1.as_deref().map(|next| next as _)
        }
    }

    #[test]
    fn an_error_carries_what_is_behind_it() {
        let err = Layer(
            "Error in the hyper legacy client",
            Some(Box::new(Layer(
                "client error (SendRequest)",
                Some(Box::new(Layer("Invalid argument (os error 22)", None))),
            ))),
        );

        assert_eq!(
            with_causes(&err),
            "Error in the hyper legacy client: client error (SendRequest): \
             Invalid argument (os error 22)"
        );
    }

    #[test]
    fn a_cause_that_only_repeats_its_wrapper_is_left_out() {
        // What `#[error("{0}")]` produces, and saying it twice helps
        // nobody.
        let err = Layer(
            "cannot reach docker",
            Some(Box::new(Layer("cannot reach docker", None))),
        );

        assert_eq!(with_causes(&err), "cannot reach docker");
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
