//! How errors travel on the wire.
//!
//! `kobune_core::Error` is not sent as-is; it is split into a code, a
//! message and a remedy. `hint` is what an agent uses to decide its next
//! move — it is not decoration.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,

    /// How to resolve it. Always shown when there is room.
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

    /// The caller gave up on this request.
    ///
    /// Work already done is not undone: the daemon stops where it is, and
    /// `up` or `rm` picks up from whatever state that left.
    pub fn cancelled() -> Self {
        Self::new(ErrorCode::Cancelled, "cancelled by the caller")
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
    /// No such workspace, service or project.
    NotFound,
    /// Already exists, so it cannot be created.
    AlreadyExists,
    /// No `kobune.toml` was found.
    ConfigNotFound,
    /// The contents of `kobune.toml` are invalid.
    InvalidConfig,
    /// Run outside a git repository.
    NotAGitRepository,
    /// The runtime cannot be reached.
    RuntimeUnavailable,
    /// A runtime operation failed.
    RuntimeFailed,
    /// Not implemented in this version.
    Unsupported,
    /// Cancelled by the caller.
    Cancelled,
    /// An unexpected internal error.
    Internal,
}

impl ErrorCode {
    /// The value the CLI returns as its exit code.
    ///
    /// Lets an agent tell failures apart without parsing output. 1 is the
    /// generic error and 2 is reserved for clap's usage error, so start at 4.
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

    /// Whether retrying the same operation might succeed.
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::RuntimeUnavailable | Self::RuntimeFailed)
    }
}

impl From<kobune_core::Error> for ApiError {
    fn from(err: kobune_core::Error) -> Self {
        use kobune_core::Error as E;

        let message = err.to_string();
        match err {
            E::ConfigNotFound(_) => Self::new(ErrorCode::ConfigNotFound, message)
                .with_hint("run `kobune init` at the project root to create kobune.toml"),
            E::ConfigParse { .. } | E::ConfigInvalid(_) => {
                Self::new(ErrorCode::InvalidConfig, message)
            }
            // The message already names the files it merged. The hint
            // says how to see which of them set what, because a merged
            // document is not something anybody can open and read.
            E::ConfigMerged { .. } => Self::new(ErrorCode::InvalidConfig, message)
                .with_hint("run `kobune config show` to see which layer sets what"),
            E::ConfigRead { .. } => Self::new(ErrorCode::InvalidConfig, message),
            E::NotAGitRepository(_) => Self::new(ErrorCode::NotAGitRepository, message)
                .with_hint("run this inside a git repository"),
            E::WorkspaceNotFound(_) => Self::new(ErrorCode::NotFound, message)
                .with_hint("run `kobune ls` to see the available workspaces"),
            E::ServiceNotFound(_) => Self::new(ErrorCode::NotFound, message)
                .with_hint("use a name defined under [services] in kobune.toml"),
            E::WorkspaceExists(_) => Self::new(ErrorCode::AlreadyExists, message),
            E::GitSpawn(_) => Self::new(ErrorCode::Internal, message)
                .with_hint("check that git is installed and on PATH"),
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
            assert!(seen.insert(exit), "duplicate exit code {exit}: {code:?}");
            assert_ne!(exit, 0, "an error must not look like success: {code:?}");
            assert_ne!(exit, 2, "2 is reserved for clap usage errors: {code:?}");
        }
    }

    #[test]
    fn missing_config_carries_actionable_hint() {
        let err: ApiError = kobune_core::Error::ConfigNotFound(PathBuf::from("/repo")).into();
        assert_eq!(err.code, ErrorCode::ConfigNotFound);
        assert!(
            err.hint.expect("a hint is present").contains("kobune init"),
            "it must say what to do next"
        );
    }

    #[test]
    fn maps_core_errors_to_codes() {
        let cases: Vec<(kobune_core::Error, ErrorCode)> = vec![
            (
                kobune_core::Error::NotAGitRepository(PathBuf::from("/tmp")),
                ErrorCode::NotAGitRepository,
            ),
            (
                kobune_core::Error::WorkspaceNotFound("feat-1".into()),
                ErrorCode::NotFound,
            ),
            (
                kobune_core::Error::ServiceNotFound("web".into()),
                ErrorCode::NotFound,
            ),
            (
                kobune_core::Error::ConfigInvalid("bad".into()),
                ErrorCode::InvalidConfig,
            ),
            (
                kobune_core::Error::WorkspaceExists("feat-1".into()),
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
