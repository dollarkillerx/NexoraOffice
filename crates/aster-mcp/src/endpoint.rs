//! Where the server listens and how the bridge finds it. Both sides use
//! this module so that they always agree on the socket's path.
//!
//! The socket lives in a directory only the user can enter (0700, owned by
//! the user, not a symbolic link), and the socket file itself is 0600:
//! together with the peer check on every connection, no other account on
//! the machine can reach the server.
//!
//! - macOS: `~/Library/Application Support/Aster Office/mcp.sock`
//! - Linux and other Unix systems: `$XDG_RUNTIME_DIR/aster-office/mcp.sock`,
//!   or `/tmp/aster-office-{uid}/mcp.sock` without a runtime directory
//! - Windows: not supported yet

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

/// The environment variable that points the bridge at another socket. Only
/// the bridge reads it; the server takes an override from its configuration.
pub const SOCKET_ENV: &str = "ASTER_MCP_SOCKET";

/// The socket's file name inside its directory.
pub const SOCKET_NAME: &str = "mcp.sock";

/// The longest socket path the system can bind or connect to, in bytes:
/// `sun_path` less its closing NUL.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const MAX_PATH_BYTES: usize = 107;
/// The longest socket path the system can bind or connect to, in bytes:
/// `sun_path` less its closing NUL.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const MAX_PATH_BYTES: usize = 103;

/// Why there is no usable socket path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointError {
    /// Local sockets are not implemented on this system yet (Windows).
    Unsupported,
    /// The home folder is unknown, so the default path cannot be made.
    NoHome,
    /// The path does not fit in a socket address.
    TooLong { path: PathBuf, max: usize },
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EndpointError::Unsupported => {
                f.write_str("the MCP server is not supported on this system yet")
            }
            EndpointError::NoHome => f.write_str(
                "the home folder is unknown (HOME is not set), so there is no socket path",
            ),
            EndpointError::TooLong { path, max } => write!(
                f,
                "the socket path `{}` is {} bytes long; this system allows at most {max}",
                path.display(),
                path.as_os_str().len(),
            ),
        }
    }
}

impl std::error::Error for EndpointError {}

/// The socket path for this user, checked for length.
pub fn default_socket_path() -> Result<PathBuf, EndpointError> {
    let path = default_path_for(
        std::env::var_os("HOME"),
        std::env::var_os("XDG_RUNTIME_DIR"),
        current_uid(),
    )?;
    check_length(&path)?;
    Ok(path)
}

/// The default path given the environment, kept apart from reading it so
/// that the rules can be tested on any system.
fn default_path_for(
    home: Option<OsString>,
    runtime_dir: Option<OsString>,
    uid: Option<u32>,
) -> Result<PathBuf, EndpointError> {
    if cfg!(windows) {
        return Err(EndpointError::Unsupported);
    }
    if cfg!(target_os = "macos") {
        let home = home
            .filter(|h| !h.is_empty())
            .ok_or(EndpointError::NoHome)?;
        return Ok(PathBuf::from(home)
            .join("Library/Application Support/Aster Office")
            .join(SOCKET_NAME));
    }
    // A runtime directory that is not absolute is not one: the XDG rules
    // say to ignore it.
    if let Some(dir) = runtime_dir.map(PathBuf::from)
        && dir.is_absolute()
    {
        return Ok(dir.join("aster-office").join(SOCKET_NAME));
    }
    let uid = uid.ok_or(EndpointError::Unsupported)?;
    Ok(PathBuf::from(format!("/tmp/aster-office-{uid}")).join(SOCKET_NAME))
}

/// Refuse a path that would be cut short in `sun_path`: binding it would
/// fail, or worse, bind a different, shorter path.
pub fn check_length(path: &Path) -> Result<(), EndpointError> {
    if path.as_os_str().len() > MAX_PATH_BYTES {
        return Err(EndpointError::TooLong {
            path: path.to_owned(),
            max: MAX_PATH_BYTES,
        });
    }
    Ok(())
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    Some(own_uid())
}

#[cfg(not(unix))]
fn current_uid() -> Option<u32> {
    None
}

/// The effective user id of this process.
#[cfg(unix)]
pub(crate) fn own_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Connect to the server's socket. The bridge's only way in.
#[cfg(unix)]
pub fn connect(path: &Path) -> std::io::Result<std::os::unix::net::UnixStream> {
    check_length(path).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    std::os::unix::net::UnixStream::connect(path)
}

/// Make the socket's directory if needed, and make sure it is one only this
/// user can use: a real directory (not a link someone planted), owned by
/// us, with no access for group or others.
#[cfg(unix)]
pub(crate) fn prepare_directory(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    if let Err(error) = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
    {
        return Err(format!("cannot create it: {error}"));
    }
    let metadata = std::fs::symlink_metadata(dir).map_err(|e| format!("cannot read it: {e}"))?;
    if metadata.file_type().is_symlink() {
        return Err("it is a symbolic link".to_owned());
    }
    if !metadata.is_dir() {
        return Err("it is not a directory".to_owned());
    }
    if metadata.uid() != own_uid() {
        return Err("it belongs to another user".to_owned());
    }
    // `create` leaves an existing directory's mode alone, and the mode it
    // gives a new one is cut by the umask; either way, set it now.
    if metadata.mode() & 0o777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot make it private: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_uses_application_support() {
        let path = default_path_for(Some("/Users/a".into()), Some("/run/x".into()), Some(501));
        assert_eq!(
            path.unwrap(),
            Path::new("/Users/a/Library/Application Support/Aster Office/mcp.sock")
        );
        assert_eq!(
            default_path_for(None, None, Some(501)),
            Err(EndpointError::NoHome)
        );
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn other_unix_systems_use_the_runtime_directory_or_tmp() {
        assert_eq!(
            default_path_for(None, Some("/run/user/1000".into()), Some(1000)).unwrap(),
            Path::new("/run/user/1000/aster-office/mcp.sock")
        );
        // A relative runtime directory is ignored.
        assert_eq!(
            default_path_for(None, Some("run".into()), Some(1000)).unwrap(),
            Path::new("/tmp/aster-office-1000/mcp.sock")
        );
        assert_eq!(
            default_path_for(None, None, Some(7)).unwrap(),
            Path::new("/tmp/aster-office-7/mcp.sock")
        );
    }

    #[test]
    fn paths_too_long_for_a_socket_are_refused() {
        let fits = PathBuf::from(format!("/{}", "a".repeat(MAX_PATH_BYTES - 1)));
        assert_eq!(check_length(&fits), Ok(()));
        let long = PathBuf::from(format!("/{}", "a".repeat(MAX_PATH_BYTES)));
        let error = check_length(&long).unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("at most {MAX_PATH_BYTES}"))
        );
    }

    #[test]
    #[cfg(unix)]
    fn the_directory_is_made_private_and_links_are_refused() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("a/b");
        prepare_directory(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        // An existing directory that is too open is closed.
        let open = temp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        prepare_directory(&open).unwrap();
        let mode = std::fs::metadata(&open).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        // A link to a directory is not accepted, even one of ours.
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(
            prepare_directory(&link)
                .unwrap_err()
                .contains("symbolic link")
        );
        let file = temp.path().join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(prepare_directory(&file).is_err());
    }
}
