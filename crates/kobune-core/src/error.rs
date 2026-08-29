use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no kobune.toml found: searched upwards from {0}")]
    ConfigNotFound(PathBuf),

    /// One of the configuration files could not be read.
    ///
    /// **Names no file in the sentence**, because three of them can land
    /// here: `config.toml` under `$KOBUNE_HOME` and `kobune.local.toml`
    /// as readily as `kobune.toml`. Saying "kobune.toml" and then giving
    /// another path in brackets sends the reader to open the file that is
    /// fine.
    #[error("cannot read the configuration ({path}): {source}")]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// One of the configuration files is not valid TOML. Same three.
    #[error("invalid syntax in {path}: {source}")]
    ConfigParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// A merge of valid TOML files that is not a valid configuration.
    ///
    /// Names the files rather than the one line at fault: the merged
    /// document exists nowhere on disk, so a message about `runtime.defalut`
    /// would otherwise send someone searching a file that does not have it.
    #[error(
        "invalid configuration in {}: {source}",
        crate::config::describe_files(files)
    )]
    ConfigMerged {
        files: Vec<PathBuf>,
        /// Boxed to keep this variant off the widest one.
        ///
        /// `toml::de::Error` is 96 bytes, and a second variant carrying
        /// one takes the whole enum past the size at which every
        /// `Result<_, Error>` in the workspace starts being complained
        /// about. Every caller of this is on a path that has already
        /// failed, so the indirection costs nothing that matters.
        #[source]
        source: Box<toml::de::Error>,
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

    /// A loosely-typed name that fits more than one workspace.
    ///
    /// **The candidates are in the message**, because the next thing
    /// anyone does is choose between them, and a name is easier to pick
    /// out of a list than out of a second command.
    #[error("`{query}` could mean {}", .candidates.join(" or "))]
    WorkspaceAmbiguous {
        query: String,
        candidates: Vec<String>,
    },

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
