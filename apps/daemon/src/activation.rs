//! launchd から待ち受け済みソケットを受け取る。
//!
//! 80 / 443 / 53 は 1024 未満なので、非 root プロセスでは bind できない。
//! launchd（root）に bind させ、ファイルディスクリプタだけを渡してもらう。
//! これにより **daemon 本体は非 root のまま**でよい。
//!
//! macOS は systemd の `LISTEN_FDS` 規約を使わない。`launch_activate_socket`
//! を呼んで、plist の `Sockets` に書いた名前で fd を引く。

use std::os::fd::{FromRawFd, RawFd};

/// plist の `Sockets` に書くキー。
pub const HTTP_SOCKET: &str = "http";
pub const HTTPS_SOCKET: &str = "https";
pub const DNS_TCP_SOCKET: &str = "dns-tcp";
pub const DNS_UDP_SOCKET: &str = "dns-udp";

/// launchd から渡された TCP リスナー。
///
/// launchd 経由で起動していない場合や、その名前の socket が無い場合は空。
pub fn tcp_listeners(name: &str) -> Vec<std::net::TcpListener> {
    raw_fds(name)
        .into_iter()
        .filter_map(|fd| {
            // SAFETY: launchd から受け取った fd は待ち受け済みの socket で、
            // このプロセスが所有権を持つ。同じ fd を二重に受け取ることはない。
            let listener = unsafe { std::net::TcpListener::from_raw_fd(fd) };

            match listener.set_nonblocking(true) {
                Ok(()) => Some(listener),
                Err(err) => {
                    tracing::warn!("{name} の fd {fd} を非ブロッキングにできません: {err}");
                    None
                }
            }
        })
        .collect()
}

/// launchd から渡された UDP ソケット。
pub fn udp_sockets(name: &str) -> Vec<std::net::UdpSocket> {
    raw_fds(name)
        .into_iter()
        .filter_map(|fd| {
            // SAFETY: tcp_listeners と同じ。
            let socket = unsafe { std::net::UdpSocket::from_raw_fd(fd) };

            match socket.set_nonblocking(true) {
                Ok(()) => Some(socket),
                Err(err) => {
                    tracing::warn!("{name} の fd {fd} を非ブロッキングにできません: {err}");
                    None
                }
            }
        })
        .collect()
}

/// launchd 経由で起動しているか。
pub fn is_active() -> bool {
    !raw_fds(HTTP_SOCKET).is_empty() || !raw_fds(HTTPS_SOCKET).is_empty()
}

#[cfg(target_os = "macos")]
fn raw_fds(name: &str) -> Vec<RawFd> {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int};

    unsafe extern "C" {
        /// `<launch.h>`。plist の `Sockets` に書いた名前で fd を取り出す。
        /// 成功時は 0 を返し、`fds` に malloc された配列が入る。
        fn launch_activate_socket(
            name: *const c_char,
            fds: *mut *mut c_int,
            count: *mut usize,
        ) -> c_int;
    }

    let Ok(key) = CString::new(name) else {
        return Vec::new();
    };

    let mut fds: *mut c_int = std::ptr::null_mut();
    let mut count: usize = 0;

    // SAFETY: key は有効な C 文字列、fds と count は書き込み可能。
    let result = unsafe { launch_activate_socket(key.as_ptr(), &mut fds, &mut count) };

    if result != 0 {
        // ESRCH (3) は「launchd 管理下でない」。単体起動では普通に起きる。
        if result == 3 {
            tracing::debug!("launchd 経由の起動ではありません（socket `{name}` なし）");
        } else {
            tracing::debug!("socket `{name}` を取得できません (errno {result})");
        }
        return Vec::new();
    }

    if fds.is_null() || count == 0 {
        return Vec::new();
    }

    // SAFETY: launchd が count 個の要素を持つ配列を malloc して返している。
    let slice = unsafe { std::slice::from_raw_parts(fds, count) };
    let collected: Vec<RawFd> = slice.iter().map(|fd| *fd as RawFd).collect();

    // SAFETY: launch_activate_socket が malloc したものを解放する。
    unsafe { libc::free(fds as *mut libc::c_void) };

    tracing::info!(
        "launchd から socket `{name}` を {} 個受け取りました",
        collected.len()
    );
    collected
}

/// macOS 以外では socket activation を使わない。
///
/// Linux では systemd の `LISTEN_FDS` に対応する余地があるが、
/// 現状は通常の bind にフォールバックする。
#[cfg(not(target_os = "macos"))]
fn raw_fds(_name: &str) -> Vec<RawFd> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_nothing_when_not_launched_by_launchd() {
        // テストは launchd 経由ではないので、必ず空になる。
        // ここで fd を握ってしまうと通常の bind と二重に待ち受けてしまう。
        assert!(tcp_listeners(HTTP_SOCKET).is_empty());
        assert!(udp_sockets(DNS_UDP_SOCKET).is_empty());
        assert!(!is_active());
    }

    #[test]
    fn unknown_socket_names_are_harmless() {
        assert!(tcp_listeners("no-such-socket").is_empty());
    }

    #[test]
    fn rejects_names_with_interior_nul() {
        // CString の生成に失敗する入力でも落とさない。
        assert!(tcp_listeners("bad\0name").is_empty());
    }
}
