//! Whether a path is on one of this computer's own disks, for code that
//! must not reach the network by reading a file: the Markdown preview's
//! pictures. NFS and SMB mounts, automount points and network drives all
//! count as not local.

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

/// Whether every folder on the way to `path` is on one of this computer's
/// own disks. Each folder is asked about before the next is entered, so a
/// network mount is noticed at its mount point, before anything inside it
/// is touched. `false` when it cannot be told.
pub fn on_local_disks(path: &Path) -> bool {
    #[cfg(windows)]
    {
        windows_local(path)
    }
    #[cfg(unix)]
    {
        let mut current = PathBuf::new();
        let mut parts = path.components().peekable();
        while let Some(part) = parts.next() {
            current.push(part);
            // The file itself is on the disk of its folder.
            if parts.peek().is_none() {
                return true;
            }
            if !unix_local(&current) {
                return false;
            }
        }
        true
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        false
    }
}

/// Whether the file system holding `dir` is a local one.
#[cfg(unix)]
fn unix_local(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(name) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `name` is a valid C string and `info` a properly sized
    // buffer that statfs fills in; neither outlives the call.
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(name.as_ptr(), &mut info) } != 0 {
        return false;
    }
    is_local_file_system(&info)
}

#[cfg(target_vendor = "apple")]
fn is_local_file_system(info: &libc::statfs) -> bool {
    (info.f_flags & libc::MNT_LOCAL as u32) != 0
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn is_local_file_system(info: &libc::statfs) -> bool {
    // File systems whose data lives elsewhere: NFS, SMB and CIFS, AFS,
    // Coda, NCP, 9P, Ceph, automount points and FUSE (sshfs and the like
    // cannot be told from local FUSE file systems, so all of FUSE counts).
    const REMOTE: [i64; 12] = [
        0x6969,
        0x517B,
        0xFF53_4D42,
        0xFE53_4D42,
        0x5346_414F,
        0x7375_7245,
        0x564C,
        0x0102_1997,
        0x00C3_6400,
        0x0187,
        0x6573_5546,
        0x0BD0_0BD0,
    ];
    !REMOTE.contains(&(info.f_type as i64))
}

#[cfg(all(
    unix,
    not(any(target_vendor = "apple", target_os = "linux", target_os = "android"))
))]
fn is_local_file_system(_: &libc::statfs) -> bool {
    false
}

#[cfg(windows)]
fn windows_local(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};
    /// GetDriveTypeW's answer for a network drive.
    const DRIVE_REMOTE: u32 = 4;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut root = [0u16; 512];
    // SAFETY: `wide` is NUL-terminated and `root` is as long as we say.
    if unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return false;
    }
    // SAFETY: GetVolumePathNameW wrote a NUL-terminated path into `root`.
    unsafe { GetDriveTypeW(root.as_ptr()) != DRIVE_REMOTE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_files_are_local_and_automounts_are_not() {
        let dir = std::env::temp_dir();
        assert!(on_local_disks(&dir.join("a.png")));
        // macOS mounts home folders on demand through autofs: not local,
        // and noticed before anything under it is looked at.
        #[cfg(target_os = "macos")]
        {
            let automount = Path::new("/System/Volumes/Data/home");
            if automount.exists() {
                assert!(!on_local_disks(&automount.join("someone/pic.png")));
            }
        }
    }
}
