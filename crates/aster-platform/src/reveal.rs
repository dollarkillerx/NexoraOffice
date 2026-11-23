//! Showing a file in the system's file manager: Finder on macOS, Explorer on
//! Windows, and on Linux whichever file manager answers the freedesktop
//! `FileManager1` interface, or the folder opened with `xdg-open`.
//!
//! The path is passed as an argument, never through a shell. These functions
//! wait for the helper program, so call them from a worker thread.
//!
//! Only the macOS path has been tried.

use std::io;
use std::path::Path;
use std::process::Command;

/// Show `path` selected in the file manager. A path that does not exist, or is
/// not absolute, is an error and nothing is started.
pub fn show_in_file_manager(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not an absolute path",
        ));
    }
    std::fs::symlink_metadata(path)?;
    platform(path)
}

fn succeeded(mut command: Command) -> io::Result<()> {
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{command:?} failed: {status}")))
    }
}

#[cfg(target_os = "macos")]
fn platform(path: &Path) -> io::Result<()> {
    let mut command = Command::new("/usr/bin/open");
    command.arg("-R").arg(path);
    succeeded(command)
}

#[cfg(target_os = "windows")]
fn platform(path: &Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt;

    // Explorer reads its own command line: the path goes in quotes right
    // after the comma, which ordinary argument quoting would get wrong. A
    // path cannot contain a quote on Windows. Explorer's exit code is not
    // meaningful, so only failing to start it is an error.
    let mut command = Command::new("explorer.exe");
    command.raw_arg(format!("/select,\"{}\"", path.display()));
    command.spawn().map(drop)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform(path: &Path) -> io::Result<()> {
    let mut command = Command::new("dbus-send");
    command.args([
        "--session",
        "--print-reply",
        "--dest=org.freedesktop.FileManager1",
        "--type=method_call",
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1.ShowItems",
    ]);
    command.arg(format!("array:string:{}", file_uri(path)));
    command.arg("string:");
    if succeeded(command).is_ok() {
        return Ok(());
    }
    // No file manager answers: open the folder instead, without the selection.
    let folder = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let mut command = Command::new("xdg-open");
    command.arg(folder);
    command.spawn().map(drop)
}

/// `file://` and the path, with everything but unreserved characters and `/`
/// percent-encoded byte by byte.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
fn file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for byte in path.as_os_str().as_encoded_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(char::from(*byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_and_missing_paths_start_nothing() {
        assert_eq!(
            show_in_file_manager(Path::new("notes.md"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let dir = std::env::temp_dir().join("aster-no-such-folder-for-reveal");
        assert_eq!(
            show_in_file_manager(&dir.join("x.md")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn uris_encode_everything_unusual() {
        assert_eq!(
            file_uri(Path::new("/home/用户/a b#c%.md")),
            "file:///home/%E7%94%A8%E6%88%B7/a%20b%23c%25.md"
        );
    }
}
