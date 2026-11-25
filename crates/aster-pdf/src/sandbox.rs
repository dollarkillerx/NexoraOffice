//! Restricting the PDF child process.
//!
//! The threat: a crafted PDF finds a memory-safety bug in the parser or the
//! renderer and runs code inside the child. Two things limit what that code
//! can do:
//!
//! * [`close_inherited_descriptors`]: the parent application may hold
//!   descriptors without close-on-exec (GUI frameworks, sockets, files).
//!   The child must not be able to use them.
//! * [`restrict`]: on macOS a Seatbelt sandbox: no network, no writes, no
//!   process creation, and reads only from the font directories.
//!
//! Limits: the sandbox is entered after the file is read and the pipes are
//! open, so it only protects the rest of the child's life. Seatbelt is not a
//! boundary against kernel bugs, and `sandbox_init` is deprecated by Apple
//! (but is what Chromium, Firefox and others still use for their helpers;
//! the supported route is an App Sandbox entitlement on the signed bundle,
//! which cannot be set per child process). The profile still allows reading
//! system metadata and the font directories, and using Mach services the
//! frameworks may need.

use std::path::{Path, PathBuf};

/// Close every descriptor except 0, 1 and 2. Call it first thing in the
/// child, before anything opens a file of its own.
#[cfg(unix)]
pub fn close_inherited_descriptors() {
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
    {
        #[cfg(target_os = "linux")]
        let listing = "/proc/self/fd";
        #[cfg(not(target_os = "linux"))]
        let listing = "/dev/fd";
        if let Ok(entries) = std::fs::read_dir(listing) {
            let numbers: Vec<libc::c_int> = entries
                .flatten()
                .filter_map(|e| e.file_name().to_str()?.parse().ok())
                .collect();
            for fd in numbers.into_iter().filter(|fd| *fd > 2) {
                // SAFETY: closing a number we found open (or the directory
                // handle itself, already gone: EBADF, ignored).
                unsafe { libc::close(fd) };
            }
            return;
        }
    }
    // No listing available: close everything up to the limit.
    // SAFETY: getrlimit writes into the struct we pass.
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let known = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0;
    // The type of rlim_cur differs between systems.
    #[allow(clippy::unnecessary_cast)]
    let top = if known { limit.rlim_cur as u64 } else { 1024 };
    for fd in 3..top.min(1 << 20) {
        // SAFETY: closing a descriptor that may not be open: EBADF, ignored.
        unsafe { libc::close(fd as libc::c_int) };
    }
}

/// Windows: handles are not inherited unless asked for; nothing to do.
#[cfg(not(unix))]
pub fn close_inherited_descriptors() {}

/// The directories `fonts.rs` may read system fonts from on this system.
pub fn font_dirs() -> Vec<PathBuf> {
    let dirs: &[&str] = if cfg!(target_os = "macos") {
        &["/System/Library/Fonts", "/Library/Fonts"]
    } else if cfg!(windows) {
        &["C:\\Windows\\Fonts"]
    } else {
        &["/usr/share/fonts"]
    };
    dirs.iter().map(PathBuf::from).collect()
}

/// Only for the sandbox self-test: open the file and render page 0 at scale
/// 1, as the child does. Returns the pixel size and
/// the number of pixels that are not white.
pub fn render_first_page_for_selftest(bytes: Vec<u8>) -> Result<(u32, u32, usize), String> {
    let document =
        crate::Document::open(bytes, "", crate::Limits::default()).map_err(|e| format!("{e:?}"))?;
    let cache = hayro::RenderCache::new();
    match document.render(&cache, 0, 1.0) {
        Ok(Some(pixmap)) => {
            let ink = pixmap
                .data_as_u8_slice()
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[..3] != [255, 255, 255])
                .count();
            Ok((u32::from(pixmap.width()), u32::from(pixmap.height()), ink))
        }
        Ok(None) => Err("nothing to draw".into()),
        Err(()) => Err("renderer panicked".into()),
    }
}

/// Quote a path as an SBPL string literal. `None` if it has characters we
/// would rather not put into a profile.
#[cfg(any(target_os = "macos", test))]
fn quote(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    if !path.is_absolute() || text.chars().any(|c| c.is_control()) {
        return None;
    }
    let mut out = String::from("\"");
    for c in text.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    Some(out)
}

/// The Seatbelt profile: deny by default, then allow what a running thread
/// needs, plus reading below `read_dirs`.
#[cfg(any(target_os = "macos", test))]
fn profile(read_dirs: &[&Path]) -> Result<String, String> {
    let mut text = String::from(PROFILE_BASE);
    for dir in read_dirs {
        let quoted = quote(dir).ok_or_else(|| format!("unusable path {}", dir.display()))?;
        text.push_str(&format!("(allow file-read* (subpath {quoted}))\n"));
    }
    Ok(text)
}

#[cfg(any(target_os = "macos", test))]
const PROFILE_BASE: &str = "\
(version 1)
(deny default)
(allow sysctl-read)
(allow file-read-metadata)
";

/// Enter the sandbox. Irreversible; call it after the PDF is read and the
/// pipes are open. Afterwards the process can only read below `read_dirs`
/// (and what the OS needs), and cannot write files, use the network or start
/// processes. Already-open descriptors keep working.
#[cfg(target_os = "macos")]
pub fn restrict(read_dirs: &[&Path]) -> Result<(), String> {
    use std::ffi::{CStr, CString};
    use std::os::raw::{c_char, c_int};

    unsafe extern "C" {
        // From libSystem. Deprecated, but still present and working.
        fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
        fn sandbox_free_error(errorbuf: *mut c_char);
    }

    let text = profile(read_dirs)?;
    let text = CString::new(text).map_err(|_| "profile has a NUL byte".to_string())?;
    let mut error: *mut c_char = std::ptr::null_mut();
    // SAFETY: `text` is a valid NUL-terminated string; flags 0 means the
    // argument is a profile string; `error` is a valid out-pointer.
    let result = unsafe { sandbox_init(text.as_ptr(), 0, &mut error) };
    if result == 0 {
        return Ok(());
    }
    let message = if error.is_null() {
        "sandbox_init failed".to_string()
    } else {
        // SAFETY: on failure `error` points to a NUL-terminated string that
        // we own until we pass it to sandbox_free_error.
        let message = unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: `error` came from sandbox_init and is freed once.
        unsafe { sandbox_free_error(error) };
        message
    };
    Err(message)
}

/// No sandbox on this system.
#[cfg(not(target_os = "macos"))]
pub fn restrict(_read_dirs: &[&Path]) -> Result<(), String> {
    Err("not available on this system".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_quoted() {
        assert_eq!(quote(Path::new("/a b")).unwrap(), "\"/a b\"");
        assert_eq!(quote(Path::new("/a\"b\\")).unwrap(), "\"/a\\\"b\\\\\"");
        assert!(quote(Path::new("relative")).is_none());
        assert!(quote(Path::new("/a\nb")).is_none());
    }

    #[test]
    fn profile_lists_the_directories() {
        let text = profile(&[Path::new("/fonts")]).unwrap();
        assert!(text.contains("(allow file-read* (subpath \"/fonts\"))"));
    }
}
