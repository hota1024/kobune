//! Minato がホームディレクトリ以下に置くファイルのレイアウト。

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// ルートディレクトリを上書きする環境変数。テストと複数インスタンスの分離に使う。
pub const HOME_ENV: &str = "MINATO_HOME";

/// Unix socket のパス長の上限。
///
/// `sockaddr_un.sun_path` は macOS で 104 バイト、Linux で 108 バイト。
/// 終端も含めて収まるよう、短い方から余裕を取る。
pub const MAX_SOCKET_PATH_LEN: usize = 100;

#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// `$MINATO_HOME`、なければ `~/.minato` を使う。
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

    /// workspace レジストリ。
    pub fn state_file(&self) -> PathBuf {
        self.root.join("state.json")
    }

    /// daemon が待ち受ける Unix socket。
    ///
    /// Unix socket のパスには長さ制限（macOS で 104 バイト）があるため、
    /// ルート直下に置いて階層を深くしない。
    pub fn socket(&self) -> PathBuf {
        self.root.join("minatod.sock")
    }

    /// socket のパスが Unix socket の長さ制限に収まるかを確かめる。
    ///
    /// 超えていると bind が `SUN_LEN` のエラーで落ちる。原因が分かりにくい
    /// エラーなので、`MINATO_HOME` を深い場所に設定したときに気づけるよう
    /// ここで説明付きのエラーにする。
    pub fn check_socket_length(&self) -> Result<()> {
        let socket = self.socket();
        let length = socket.as_os_str().as_encoded_bytes().len();

        if length > MAX_SOCKET_PATH_LEN {
            return Err(Error::ConfigInvalid(format!(
                "Unix socket のパスが長すぎます（{length} バイト、上限 {MAX_SOCKET_PATH_LEN} バイト）: {}\n\
                 {HOME_ENV} をより短いパスに設定してください",
                socket.display()
            )));
        }

        Ok(())
    }

    /// daemon の PID ファイル。多重起動の検出に使う。
    pub fn pid_file(&self) -> PathBuf {
        self.root.join("minatod.pid")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn daemon_log(&self) -> PathBuf {
        self.log_dir().join("minatod.log")
    }

    /// ローカル CA の鍵と証明書。
    pub fn ca_dir(&self) -> PathBuf {
        self.root.join("ca")
    }

    /// 必要なディレクトリを作る。
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
            .expect("既定のレイアウトは上限に収まる");
    }

    #[test]
    fn rejects_socket_path_over_the_limit() {
        // 深いディレクトリを MINATO_HOME にすると bind が SUN_LEN で失敗する。
        // 原因が分かりにくいエラーなので、事前に説明付きで弾く。
        let deep = PathBuf::from("/tmp").join("x".repeat(MAX_SOCKET_PATH_LEN));
        let err = Paths::with_root(deep).check_socket_length().unwrap_err();

        let message = err.to_string();
        assert!(message.contains("長すぎます"), "got: {message}");
        assert!(
            message.contains(HOME_ENV),
            "どう直せばよいかを示す: {message}"
        );
    }

    #[test]
    fn accepts_socket_path_at_the_limit() {
        // 境界ちょうどは通す。
        let socket_name_len = "/minatod.sock".len();
        let root =
            PathBuf::from("/".to_string() + &"x".repeat(MAX_SOCKET_PATH_LEN - socket_name_len - 1));

        let paths = Paths::with_root(root);
        assert_eq!(
            paths.socket().as_os_str().as_encoded_bytes().len(),
            MAX_SOCKET_PATH_LEN
        );
        paths.check_socket_length().expect("上限ちょうどは許容する");
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
