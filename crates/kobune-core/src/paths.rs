//! The layout of the files Minato keeps under its home directory.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Overrides the root directory. Used by tests and to isolate instances.
pub const HOME_ENV: &str = "MINATO_HOME";

/// The upper bound on the length of a Unix socket path.
///
/// `sockaddr_un.sun_path` holds 104 bytes on macOS and 108 on Linux.
/// Leave headroom below the smaller of the two, terminator included.
pub const MAX_SOCKET_PATH_LEN: usize = 100;

#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// Uses `$MINATO_HOME`, falling back to `~/.minato`.
    pub fn resolve() -> Result<Self> {
        if let Some(value) = std::env::var_os(HOME_ENV) {
            let path = PathBuf::from(value);
            if !path.as_os_str().is_empty() {
                return Ok(Self::with_root(path));
            }
        }

        let home = dirs::home_dir().ok_or(Error::NoHomeDirectory)?;
        Ok(Self::with_root(home.join(".minato")))
    }

    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The workspace registry.
    pub fn state_file(&self) -> PathBuf {
        self.root.join("state.json")
    }

    /// The Unix socket the daemon listens on.
    ///
    /// Socket paths are length-limited (104 bytes on macOS), so keep this
    /// directly under the root rather than nesting it.
    pub fn socket(&self) -> PathBuf {
        self.root.join("minatod.sock")
    }

    /// Checks that the socket path fits within the platform limit.
    ///
    /// Exceeding it makes `bind` fail with `SUN_LEN`, which says nothing
    /// about the cause. Fail here with an explanation instead, so a deep
    /// `MINATO_HOME` is caught before it turns into a puzzle.
    pub fn check_socket_length(&self) -> Result<()> {
        let socket = self.socket();
        let length = socket.as_os_str().as_encoded_bytes().len();

        if length > MAX_SOCKET_PATH_LEN {
            return Err(Error::ConfigInvalid(format!(
                "the Unix socket path is too long ({length} bytes, limit {MAX_SOCKET_PATH_LEN}): {}\n\
                 set {HOME_ENV} to a shorter path",
                socket.display()
            )));
        }

        Ok(())
    }

    pub fn log_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn daemon_log(&self) -> PathBuf {
        self.log_dir().join("minatod.log")
    }

    /// The local CA's key and certificate.
    pub fn ca_dir(&self) -> PathBuf {
        self.root.join("ca")
    }

    /// The generated cloudflared configuration.
    ///
    /// Not `~/.cloudflared`: that belongs to cloudflared itself and holds
    /// the login certificate, which Minato only ever reads.
    pub fn tunnel_dir(&self) -> PathBuf {
        self.root.join("tunnel")
    }

    /// Creates the directories Minato needs.
    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.log_dir())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_flat_enough_for_unix_socket() {
        let paths = Paths::with_root(PathBuf::from("/Users/someone/.minato"));
        paths
            .check_socket_length()
            .expect("the default layout fits within the limit");
    }

    #[test]
    fn rejects_socket_path_over_the_limit() {
        // A deep MINATO_HOME makes bind fail with SUN_LEN, which explains
        // nothing. Reject it up front with a message that does.
        let deep = PathBuf::from("/tmp").join("x".repeat(MAX_SOCKET_PATH_LEN));
        let err = Paths::with_root(deep).check_socket_length().unwrap_err();

        let message = err.to_string();
        assert!(message.contains("too long"), "got: {message}");
        assert!(message.contains(HOME_ENV), "say how to fix it: {message}");
    }

    #[test]
    fn accepts_socket_path_at_the_limit() {
        // Exactly at the limit is allowed.
        let socket_name_len = "/minatod.sock".len();
        let root =
            PathBuf::from("/".to_string() + &"x".repeat(MAX_SOCKET_PATH_LEN - socket_name_len - 1));

        let paths = Paths::with_root(root);
        assert_eq!(
            paths.socket().as_os_str().as_encoded_bytes().len(),
            MAX_SOCKET_PATH_LEN
        );
        paths
            .check_socket_length()
            .expect("exactly at the limit is fine");
    }

    #[test]
    fn derives_paths_from_root() {
        let paths = Paths::with_root(PathBuf::from("/tmp/minato-test"));
        assert_eq!(paths.state_file(), Path::new("/tmp/minato-test/state.json"));
        assert_eq!(paths.socket(), Path::new("/tmp/minato-test/minatod.sock"));
        assert_eq!(
            paths.daemon_log(),
            Path::new("/tmp/minato-test/logs/minatod.log")
        );
    }
}
