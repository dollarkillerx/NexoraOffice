//! Whether a path is on one of this computer's own disks, for code that
//! must not reach the network by reading a file: the Markdown preview's
//! pictures. NFS and SMB mounts, automount points and network drives all
//! count as not local.

use std::path::Path;

/// How many links a path may pass through before it is given up on.
const MAX_LINKS: u32 = 8;

/// Whether every folder on the way to `path` is on one of this computer's
/// own disks. Each folder is checked before anything in it is looked at,
/// so a network mount is noticed at its mount point; each step is looked at
/// without following links, and a link's target is checked the same way
/// before it is followed, so a link cannot lead the check into an
/// automount either. `false` when it cannot be told, for a relative path,
/// or after more than eight links.
pub fn on_local_disks(path: &Path) -> bool {
    check(path, 0)
}

fn check(path: &Path, links: u32) -> bool {
    if links > MAX_LINKS || !path.is_absolute() {
        return false;
    }
    #[cfg(windows)]
    if !windows_drive_local(path) {
        return false;
    }
    let parts: Vec<std::path::Component<'_>> = path.components().collect();
    let mut current = std::path::PathBuf::new();
    for (index, part) in parts.iter().enumerate() {
        current.push(part);
        if !matches!(part, std::path::Component::Normal(_)) {
            // The root (and on Windows the drive): checked above or below.
            #[cfg(unix)]
            if matches!(part, std::path::Component::RootDir) && !unix_local(&current) {
                return false;
            }
            continue;
        }
        // `current` is in a folder already found local: looking at it
        // without following it reaches nothing else.
        let Ok(meta) = std::fs::symlink_metadata(&current) else {
            return false;
        };
        if meta.file_type().is_symlink() {
            let Ok(target) = std::fs::read_link(&current) else {
                return false;
            };
            let target = match current.parent() {
                Some(folder) if target.is_relative() => folder.join(target),
                _ => target,
            };
            let mut rest = target;
            for part in &parts[index + 1..] {
                rest.push(part);
            }
            return check(&rest, links + 1);
        }
        // The file itself is on the disk of its folder; a folder on the
        // way must be local before anything in it is looked at.
        if index + 1 < parts.len() {
            #[cfg(unix)]
            if !unix_local(&current) {
                return false;
            }
        }
    }
    true
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

/// Whether a path's drive is a local one, by its drive letter alone, so
/// that nothing on a network drive is asked about. Paths without a drive
/// letter (network shares, device paths) are not local.
#[cfg(windows)]
fn windows_drive_local(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Prefix};
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    // GetDriveTypeW's answers for local drives: removable, fixed, CD-ROM
    // and RAM disks. Network (4), unknown (0) and no such root (1) are not.
    const LOCAL: [u32; 4] = [2, 3, 5, 6];
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    let letter = match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
        _ => return false,
    };
    let root: Vec<u16> = std::ffi::OsStr::new(&format!("{}:\\", letter as char))
        .encode_wide()
        .chain([0])
        .collect();
    // SAFETY: `root` is a NUL-terminated wide string that outlives the call.
    LOCAL.contains(&unsafe { GetDriveTypeW(root.as_ptr()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_files_are_local_and_automounts_are_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"").unwrap();
        assert!(on_local_disks(&dir.path().join("a.png")));
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

    #[cfg(unix)]
    #[test]
    fn links_are_checked_before_they_are_followed() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("a.png"), b"").unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
        assert!(on_local_disks(&dir.path().join("link/a.png")));
        // A link into an automount is refused without entering it.
        #[cfg(target_os = "macos")]
        {
            let automount = Path::new("/System/Volumes/Data/home");
            if automount.exists() {
                std::os::unix::fs::symlink(automount, dir.path().join("away")).unwrap();
                assert!(!on_local_disks(&dir.path().join("away/someone/a.png")));
            }
        }
        // A loop of links ends.
        std::os::unix::fs::symlink(dir.path().join("b"), dir.path().join("a")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("a"), dir.path().join("b")).unwrap();
        assert!(!on_local_disks(&dir.path().join("a/x.png")));
        // A path that is not there cannot be told.
        assert!(!on_local_disks(&dir.path().join("missing/x.png")));
        assert!(!on_local_disks(Path::new("relative/x.png")));
    }
}
