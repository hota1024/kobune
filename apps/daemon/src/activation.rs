//! Taking already-listening sockets from launchd.
//!
//! 80, 443 and 53 are all below 1024, so no unprivileged process can bind
//! them. launchd binds them as root and hands over nothing but the file
//! descriptors, which keeps **the daemon itself unprivileged**.
//!
//! macOS does not use systemd's `LISTEN_FDS` convention. Instead
//! `launch_activate_socket` looks a descriptor up by the name written in
//! the plist's `Sockets`.

use std::os::fd::{FromRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};

/// The keys written under `Sockets` in the plist.
pub const HTTP_SOCKET: &str = "http";
pub const HTTPS_SOCKET: &str = "https";
pub const DNS_TCP_SOCKET: &str = "dns-tcp";
pub const DNS_UDP_SOCKET: &str = "dns-udp";

/// Set once launchd has handed anything over.
///
/// **launchd answers for a socket name once.** Every call after the first
/// gets `EALREADY` and no descriptors, which is indistinguishable from
/// never having been launchd's at all. Startup spends that one answer, so
/// whatever asks later has to read what startup found rather than ask
/// again.
static ACTIVATED: AtomicBool = AtomicBool::new(false);

/// The TCP listeners launchd handed over.
///
/// Empty when the process was not started by launchd, or when there is no
/// socket by that name.
pub fn tcp_listeners(name: &str) -> Vec<std::net::TcpListener> {
    raw_fds(name)
        .into_iter()
        .filter_map(|fd| {
            // SAFETY: a descriptor from launchd is an already-listening
            // socket this process owns. The same one never arrives twice.
            let listener = unsafe { std::net::TcpListener::from_raw_fd(fd) };

            match listener.set_nonblocking(true) {
                Ok(()) => Some(listener),
                Err(err) => {
                    tracing::warn!("cannot set {name}'s fd {fd} non-blocking: {err}");
                    None
                }
            }
        })
        .collect()
}

/// The UDP sockets launchd handed over.
pub fn udp_sockets(name: &str) -> Vec<std::net::UdpSocket> {
    raw_fds(name)
        .into_iter()
        .filter_map(|fd| {
            // SAFETY: as in tcp_listeners.
            let socket = unsafe { std::net::UdpSocket::from_raw_fd(fd) };

            match socket.set_nonblocking(true) {
                Ok(()) => Some(socket),
                Err(err) => {
                    tracing::warn!("cannot set {name}'s fd {fd} non-blocking: {err}");
                    None
                }
            }
        })
        .collect()
}

/// Whether launchd handed this process any of its sockets.
///
/// Reports what the descriptors already taken say, so it stays true for
/// the life of the daemon. Asking launchd here instead would answer
/// `EALREADY` and read as a plain start — the state that sends people
/// after a socket nothing else holds.
pub fn is_active() -> bool {
    ACTIVATED.load(Ordering::Relaxed)
}

#[cfg(target_os = "macos")]
fn raw_fds(name: &str) -> Vec<RawFd> {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int};

    unsafe extern "C" {
        /// From `<launch.h>`. Looks descriptors up by the name in the
        /// plist's `Sockets`. Returns 0 on success, with a malloc'd array
        /// in `fds`.
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

    // SAFETY: key is a valid C string; fds and count are writable.
    let result = unsafe { launch_activate_socket(key.as_ptr(), &mut fds, &mut count) };

    if result != 0 {
        match result {
            // Normal for a standalone start: nothing here is launchd's.
            libc::ESRCH => tracing::debug!("not started by launchd (no socket `{name}`)"),
            // Asked twice. The descriptors went to the first caller and
            // are not repeated, so this says nothing about whether the
            // process is launchd's — read ACTIVATED for that.
            libc::EALREADY => tracing::debug!("socket `{name}` was already taken from launchd"),
            errno => tracing::debug!("cannot get socket `{name}` (errno {errno})"),
        }
        return Vec::new();
    }

    if fds.is_null() || count == 0 {
        return Vec::new();
    }

    // SAFETY: launchd malloc'd an array of exactly count elements.
    let slice = unsafe { std::slice::from_raw_parts(fds, count) };
    let collected: Vec<RawFd> = slice.iter().map(|fd| *fd as RawFd).collect();

    // SAFETY: freeing what launch_activate_socket malloc'd.
    unsafe { libc::free(fds as *mut libc::c_void) };

    // The one place descriptors ever arrive, so the one place that can
    // record it before the answer is spent.
    ACTIVATED.store(true, Ordering::Relaxed);

    tracing::info!(
        "took {} descriptor(s) for socket `{name}` from launchd",
        collected.len()
    );
    collected
}

/// Socket activation is macOS-only.
///
/// There is room to support systemd's `LISTEN_FDS` on Linux; for now
/// everything falls back to an ordinary bind.
#[cfg(not(target_os = "macos"))]
fn raw_fds(_name: &str) -> Vec<RawFd> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_nothing_when_not_launched_by_launchd() {
        // Tests are not started by launchd, so this is always empty.
        // Holding a descriptor here would end up listening twice, once
        // through the ordinary bind.
        assert!(tcp_listeners(HTTP_SOCKET).is_empty());
        assert!(udp_sockets(DNS_UDP_SOCKET).is_empty());
    }

    // Owns ACTIVATED for the whole suite: it is process-wide, so no other
    // test may assert on `is_active` alongside this one.
    #[test]
    fn activation_outlives_launchd_answering() {
        assert!(!is_active(), "nothing here was handed over");

        // A daemon that did get descriptors spent launchd's one answer
        // getting them, so from then on asking comes back empty. Reading
        // that as a plain start is what sent `doctor` after a socket
        // nothing else was holding.
        ACTIVATED.store(true, Ordering::Relaxed);
        assert!(tcp_listeners(HTTP_SOCKET).is_empty());
        assert!(is_active());

        ACTIVATED.store(false, Ordering::Relaxed);
    }

    #[test]
    fn unknown_socket_names_are_harmless() {
        assert!(tcp_listeners("no-such-socket").is_empty());
    }

    #[test]
    fn rejects_names_with_interior_nul() {
        // Input that CString cannot take must not panic.
        assert!(tcp_listeners("bad\0name").is_empty());
    }
}
