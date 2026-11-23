//! Whole-file reads and atomic whole-file writes.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Read a whole file.
pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    fs::read(path)
}

/// Make a rename or a new file in `dir` durable. Directories cannot be synced on
/// Windows, where the rename itself is what the file system journals.
fn sync_dir(dir: &Path) -> io::Result<()> {
    if cfg!(unix) {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

/// Where a write to `path` really lands. A symbolic link is followed, so saving
/// through a link updates the file it points at instead of replacing the link
/// with a regular file.
fn destination(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
        }
        _ => path.to_owned(),
    }
}

/// Replace `path` with `bytes` so that a crash at any point leaves either the
/// previous file or the complete new one.
///
/// The bytes go to a temporary file in the same directory, which is flushed to
/// disk and then renamed over the target; the directory is flushed after the
/// rename. An existing file keeps its permissions. A new file gets the
/// permissions the system gives any newly created file.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let path = destination(path);
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let existing = fs::metadata(&path).ok().map(|meta| meta.permissions());

    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(bytes)?;
    match existing {
        // The temporary file is private to the owner; the document must not
        // become so just because it was saved.
        Some(permissions) => temp.as_file().set_permissions(permissions)?,
        None => widen_new_file_permissions(temp.as_file())?,
    }
    temp.as_file().sync_all()?;
    temp.persist(&path).map_err(|err| err.error)?;
    sync_dir(dir)
}

/// Give a file that did not exist before the mode an ordinary `create` would
/// have given it: read and write for everyone, minus the process umask.
#[cfg(unix)]
fn widen_new_file_permissions(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = default_create_mode()?;
    file.set_permissions(fs::Permissions::from_mode(mode))
}

/// The mode a plain `create` gives a new file here. There is no way to read the
/// umask without changing it, and changing it is not safe with other threads
/// running; creating a scratch file and reading its mode back asks the system
/// the same question.
#[cfg(unix)]
fn default_create_mode() -> io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir()?;
    let probe = dir.path().join("probe");
    File::create(&probe)?;
    Ok(fs::metadata(&probe)?.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn widen_new_file_permissions(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_replaces_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        write_atomically(&path, b"first version").unwrap();
        write_atomically(&path, b"second").unwrap();
        assert_eq!(read(&path).unwrap(), b"second");
    }

    #[test]
    fn no_temporary_file_is_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        write_atomically(&path, b"one").unwrap();
        write_atomically(&path, b"two").unwrap();
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["note.md"]);
    }

    #[test]
    fn empty_content_is_written_as_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        write_atomically(&path, b"").unwrap();
        assert_eq!(read(&path).unwrap(), b"");
    }

    #[test]
    fn a_missing_directory_is_an_error_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-such-folder").join("note.md");
        assert!(write_atomically(&path, b"text").is_err());
        assert!(!path.exists());
    }

    #[test]
    fn reading_a_missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = read(&dir.path().join("missing.md")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.md");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        write_atomically(&path, b"new").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }

    #[cfg(unix)]
    #[test]
    fn a_new_file_is_not_private_to_the_owner() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.md");
        write_atomically(&path, b"text").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, default_create_mode().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn saving_through_a_symlink_updates_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.md");
        let link = dir.path().join("link.md");
        fs::write(&target, b"old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_atomically(&link, b"new").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(read(&target).unwrap(), b"new");
    }
}
