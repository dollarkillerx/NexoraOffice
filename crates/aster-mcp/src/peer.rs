//! Who is on the other end of a local socket. The socket's directory and
//! file modes already keep other users out; asking the kernel for the
//! peer's user id on every connection is the second lock, and the one that
//! does not depend on file permissions being right.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

/// The peer's effective user id, or `None` if the system cannot tell.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn peer_uid(stream: &UnixStream) -> Option<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let mut length = size;
    // SAFETY: the pointer and length describe `credentials`, which lives
    // for the whole call, and the descriptor belongs to `stream`.
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &mut length,
        )
    };
    (status == 0 && length == size).then_some(credentials.uid)
}

/// The peer's effective user id, or `None` if the system cannot tell.
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub(crate) fn peer_uid(stream: &UnixStream) -> Option<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: both pointers are to locals that outlive the call, and the
    // descriptor belongs to `stream`.
    let status = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    (status == 0).then_some(uid)
}

/// Systems where we do not know how to ask: nobody gets in.
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
pub(crate) fn peer_uid(_: &UnixStream) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_socket_pair_is_our_own_user() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(&a), Some(crate::endpoint::own_uid()));
    }
}
