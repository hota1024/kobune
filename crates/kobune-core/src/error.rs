use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no kobune.toml found: searched upwards from {0}")]
    ConfigNotFound(PathBuf),

    #[error("cannot read kobune.toml ({path}): {source}")]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid syntax in kobune.toml ({path}): {source}")]
    ConfigParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// Syntactically valid but semantically inconsistent configuration.
    #[error("invalid configuration: {0}")]
    ConfigInvalid(String),

    #[error("not a git repository: {0}")]
    NotAGitRepository(PathBuf),

    #[error("cannot run git: {0}")]
    GitSpawn(#[source] std::io::Error),

    #[error("git {args} failed (exit {code}): {stderr}")]
    GitFailed {
        args: String,
        code: i32,
        stderr: String,
    },

    #[error("no such workspace: {0}")]
    WorkspaceNotFound(String),

    #[error("workspace already exists: {0}")]
    WorkspaceExists(String),

    #[error("no such service: {0}")]
    ServiceNotFound(String),

    #[error("cannot access the state file ({path}): {source}")]
    StateIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("the state file is corrupt ({path}): {source}")]
    StateCorrupt {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("cannot determine the home directory")]
    NoHomeDirectory,

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
