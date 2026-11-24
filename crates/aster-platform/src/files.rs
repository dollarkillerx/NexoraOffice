//! Making, renaming and throwing away files and folders for the sidebar's
//! file tree: New Markdown File, New Folder, Rename and Move to Trash.
//!
//! Nothing here ever replaces a file that is already there. New files are
//! created exclusively, and a rename refuses a name that is taken, at the
//! moment of the rename where the system can say so:
//!
//! - macOS: `renamex_np` with `RENAME_EXCL`
//! - Linux: `renameat2` with `RENAME_NOREPLACE`
//! - Windows: `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` (the
//!   standard library's `fs::rename` replaces files there, so it is not
//!   used)
//!
//! Where none of those is available (other systems, or a volume that does
//! not support them) a file is renamed by making a second link and
//! removing the first, which also cannot replace anything. A folder, a
//! symbolic link, or a file on a volume without links is then renamed after
//! checking that the name is free: something made under that name between
//! the check and the rename would be replaced. That is the one race left.
//!
//! The error enums are for the shell to turn into translated messages;
//! their `Display` text is English, for logs.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The longest name allowed, in bytes of UTF-8: the limit of most file
/// systems.
pub const MAX_NAME_BYTES: usize = 255;

/// How many numbered names are tried before giving up.
const MAX_TRIES: u32 = 10_000;

/// Why a name cannot be used for a file or folder. The rules are the union
/// of what macOS, Windows and Linux refuse, so that a folder of documents
/// can be copied to any of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    /// Nothing, or only spaces.
    Empty,
    /// `.` or `..`, which name the folder itself and the one above it.
    Dots,
    /// Contains `/`, `\` or `:`, which separate folders on one system or
    /// another. The first such character found.
    Separator(char),
    /// Contains NUL or another control character.
    ControlChar,
    /// A name Windows keeps for devices, such as `CON` or `com1.txt`.
    Reserved,
    /// Ends with a dot or a space, which Windows drops.
    TrailingDotOrSpace,
    /// Longer than [`MAX_NAME_BYTES`] bytes.
    TooLong,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("the name is empty"),
            Self::Dots => f.write_str("the name cannot be . or .."),
            Self::Separator(c) => write!(f, "the name cannot contain {c:?}"),
            Self::ControlChar => f.write_str("the name contains a control character"),
            Self::Reserved => f.write_str("the name is reserved by Windows"),
            Self::TrailingDotOrSpace => f.write_str("the name cannot end with a dot or a space"),
            Self::TooLong => write!(f, "the name is longer than {MAX_NAME_BYTES} bytes"),
        }
    }
}

impl std::error::Error for NameError {}

/// Whether `name` can be used for a file or folder on every system. Leading
/// spaces are allowed; the caller decides whether to trim what was typed.
pub fn check_name(name: &str) -> Result<(), NameError> {
    if name.trim_matches(' ').is_empty() {
        return Err(NameError::Empty);
    }
    if name == "." || name == ".." {
        return Err(NameError::Dots);
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(NameError::TooLong);
    }
    if let Some(c) = name.chars().find(|c| matches!(c, '/' | '\\' | ':')) {
        return Err(NameError::Separator(c));
    }
    if name.chars().any(char::is_control) {
        return Err(NameError::ControlChar);
    }
    if name.ends_with(['.', ' ']) {
        return Err(NameError::TrailingDotOrSpace);
    }
    if reserved_on_windows(name) {
        return Err(NameError::Reserved);
    }
    Ok(())
}

/// Windows' device names, which it refuses as a name with any extension,
/// in any case, and with spaces before the extension.
fn reserved_on_windows(name: &str) -> bool {
    const DEVICES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    if DEVICES
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
    {
        return true;
    }
    // COM1 to COM9 and LPT1 to LPT9, and the superscript digits ¹ ² ³
    // that Windows also counts.
    let (Some(prefix), Some(number)) = (stem.get(..3), stem.get(3..)) else {
        return false;
    };
    (prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT"))
        && matches!(
            number,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
}

/// The name a new item gets the `n`th time round: `base`, then `base 2`,
/// `base 3`, with `.ext` after it unless `ext` is empty.
fn numbered(base: &str, ext: &str, n: u32) -> String {
    let mut name = if n <= 1 {
        base.to_owned()
    } else {
        format!("{base} {n}")
    };
    if !ext.is_empty() {
        name.push('.');
        name.push_str(ext);
    }
    name
}

/// Whether nothing in `dir` has this name. A dangling link counts as
/// taken, and so does a name that cannot be looked at.
fn free(dir: &Path, name: &str) -> bool {
    matches!(
        fs::symlink_metadata(dir.join(name)),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    )
}

/// The first name in `dir` not yet taken: `未命名.md`, then `未命名 2.md`,
/// `未命名 3.md` and so on, for `base` "未命名" (translated by the caller)
/// and `ext` "md" (without the dot). Only a suggestion: something else may
/// take the name before it is used, so to make the file use
/// [`create_untitled_file`].
pub fn new_file_name(dir: &Path, base: &str, ext: &str) -> PathBuf {
    let n = (1..MAX_TRIES)
        .find(|&n| free(dir, &numbered(base, ext, n)))
        .unwrap_or(MAX_TRIES);
    dir.join(numbered(base, ext, n))
}

/// The first folder name in `dir` not yet taken: `base`, `base 2`, ...
pub fn new_folder_name(dir: &Path, base: &str) -> PathBuf {
    new_file_name(dir, base, "")
}

/// The error for a name [`check_name`] refuses; the [`NameError`] can be
/// had back with `io::Error::get_ref` and `downcast_ref`.
fn invalid_name(error: NameError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error)
}

/// Makes an empty file called `name` in `dir`, failing with
/// `AlreadyExists` rather than touching a file that is there, and with
/// `InvalidInput` holding a [`NameError`] for a name that cannot be used.
pub fn create_file(dir: &Path, name: &str) -> io::Result<PathBuf> {
    check_name(name).map_err(invalid_name)?;
    let path = dir.join(name);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    Ok(path)
}

/// Makes an empty folder called `name` in `dir`, failing as
/// [`create_file`] does.
pub fn create_folder(dir: &Path, name: &str) -> io::Result<PathBuf> {
    check_name(name).map_err(invalid_name)?;
    let path = dir.join(name);
    fs::create_dir(&path)?;
    Ok(path)
}

/// Makes a new empty file under the first free name [`new_file_name`]
/// would give, trying the next number when another program takes a name
/// first.
pub fn create_untitled_file(dir: &Path, base: &str, ext: &str) -> io::Result<PathBuf> {
    create_untitled(|n| create_file(dir, &numbered(base, ext, n)))
}

/// Makes a new empty folder under the first free name
/// [`new_folder_name`] would give.
pub fn create_untitled_folder(dir: &Path, base: &str) -> io::Result<PathBuf> {
    create_untitled(|n| create_folder(dir, &numbered(base, "", n)))
}

fn create_untitled(mut create: impl FnMut(u32) -> io::Result<PathBuf>) -> io::Result<PathBuf> {
    for n in 1..=MAX_TRIES {
        match create(n) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            result => return result,
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "every numbered name is taken",
    ))
}

/// Why a rename did not happen.
#[derive(Debug)]
pub enum RenameError {
    /// The new name cannot be used.
    Name(NameError),
    /// Something in the folder already has the new name.
    Exists,
    /// The file or folder to rename is no longer there.
    NotFound,
    /// The system refused, for example for lack of permission.
    Io(io::Error),
}

impl fmt::Display for RenameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(error) => error.fmt(f),
            Self::Exists => f.write_str("something with that name is already there"),
            Self::NotFound => f.write_str("the item to rename is not there"),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RenameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Name(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Exists | Self::NotFound => None,
        }
    }
}

impl From<NameError> for RenameError {
    fn from(error: NameError) -> Self {
        Self::Name(error)
    }
}

impl From<io::Error> for RenameError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::AlreadyExists => Self::Exists,
            io::ErrorKind::NotFound => Self::NotFound,
            _ => Self::Io(error),
        }
    }
}

/// Gives the file or folder at `from` the name `new_name` in the same
/// folder, and returns its new path. Refuses a name that is taken; a
/// change of case alone (`a.md` to `A.md`) works on file systems that
/// ignore case, by way of a temporary name. A link is renamed itself, not
/// what it points to.
pub fn rename(from: &Path, new_name: &str) -> Result<PathBuf, RenameError> {
    check_name(new_name)?;
    let (Some(folder), Some(old_name)) = (from.parent(), from.file_name()) else {
        return Err(RenameError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a file or folder in a folder",
        )));
    };
    let source = fs::symlink_metadata(from)?;
    let to = folder.join(new_name);
    if old_name == OsStr::new(new_name) {
        return Ok(to);
    }
    match fs::symlink_metadata(&to) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => rename_exclusive(from, &to)?,
        Err(error) => return Err(RenameError::Io(error)),
        // The new name finds the item itself: only the case (or on macOS
        // the Unicode form) differs, on a file system that ignores that.
        Ok(existing) if same_item(&source, &existing, old_name, new_name) => {
            rename_through_temporary(from, &to, folder)?
        }
        Ok(_) => return Err(RenameError::Exists),
    }
    Ok(to)
}

/// Whether `a` and `b`, looked up under different names, are the same
/// item.
#[cfg(unix)]
fn same_item(a: &fs::Metadata, b: &fs::Metadata, _: &OsStr, _: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Windows has no stable way to compare file identities in the standard
/// library; on a file system that ignores case, two names that differ only
/// in case find the same item. If they are in fact two items (a folder
/// set to tell case apart), the second step of the rename refuses and the
/// first is undone.
#[cfg(not(unix))]
fn same_item(_: &fs::Metadata, _: &fs::Metadata, old_name: &OsStr, new_name: &str) -> bool {
    old_name
        .to_str()
        .is_some_and(|old| old.to_lowercase() == new_name.to_lowercase())
}

/// Renames `from` to `to` in two exclusive steps through a temporary name
/// in `folder`, so that a file system that ignores case sees two different
/// names each time. If the second step fails the item gets its old name
/// back.
fn rename_through_temporary(from: &Path, to: &Path, folder: &Path) -> io::Result<()> {
    let mut temporary = None;
    for n in 0..16 {
        let candidate = folder.join(format!(".aster-rename-{}-{n}", std::process::id()));
        match rename_exclusive(from, &candidate) {
            Ok(()) => {
                temporary = Some(candidate);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let Some(temporary) = temporary else {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no free temporary name",
        ));
    };
    match rename_exclusive(&temporary, to) {
        Ok(()) => Ok(()),
        Err(error) => match rename_exclusive(&temporary, from) {
            Ok(()) => Err(error),
            Err(_) => Err(io::Error::new(
                error.kind(),
                format!("{error}; the item was left as {}", temporary.display()),
            )),
        },
    }
}

#[cfg(unix)]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path contains NUL"))
}

/// Renames `from` to `to`, failing with `AlreadyExists` if `to` is there.
#[cfg(target_vendor = "apple")]
fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    let (old, new) = (c_path(from)?, c_path(to)?);
    // SAFETY: both are valid C strings that outlive the call.
    if unsafe { libc::renamex_np(old.as_ptr(), new.as_ptr(), libc::RENAME_EXCL) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // The volume does not support RENAME_EXCL.
        Some(libc::ENOTSUP | libc::EOPNOTSUPP | libc::EINVAL) => rename_fallback(from, to),
        _ => Err(error),
    }
}

/// Renames `from` to `to`, failing with `AlreadyExists` if `to` is there.
/// Called through `syscall` rather than glibc's wrapper, which older
/// versions lack.
#[cfg(all(target_os = "linux", any(target_env = "gnu", target_env = "musl")))]
fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    let (old, new) = (c_path(from)?, c_path(to)?);
    // SAFETY: both are valid C strings that outlive the call, and the
    // arguments are those renameat2 takes.
    let done = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            old.as_ptr(),
            libc::AT_FDCWD,
            new.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if done == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // A file system without RENAME_NOREPLACE, a kernel before 3.15, or
        // a sandbox that does not allow the call.
        Some(libc::EINVAL | libc::ENOSYS | libc::EPERM) => rename_fallback(from, to),
        _ => Err(error),
    }
}

/// Renames `from` to `to`, failing with `AlreadyExists` if `to` is there.
#[cfg(windows)]
fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let wide = |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain([0]).collect() };
    let (old, new) = (wide(from), wide(to));
    // No flags: never replace, never copy to another volume.
    // SAFETY: both are NUL-terminated wide strings that outlive the call.
    if unsafe { MoveFileExW(old.as_ptr(), new.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Renames `from` to `to` without a system call that refuses a taken name.
#[cfg(not(any(
    windows,
    target_vendor = "apple",
    all(target_os = "linux", any(target_env = "gnu", target_env = "musl"))
)))]
fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    rename_fallback(from, to)
}

/// A file gets a second link under the new name, which fails if the name
/// is taken, and then loses the old one. Anything else, or a file where
/// links cannot be made, is renamed after a check that the name is free.
#[cfg(not(windows))]
fn rename_fallback(from: &Path, to: &Path) -> io::Result<()> {
    if fs::symlink_metadata(from)?.is_file() {
        match fs::hard_link(from, to) {
            Ok(()) => {
                if let Err(error) = fs::remove_file(from) {
                    let _ = fs::remove_file(to);
                    return Err(error);
                }
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Err(error),
            // No links on this volume (FAT, exFAT and the like).
            Err(_) => {}
        }
    }
    // Racy: something made at `to` after this check would be replaced.
    match fs::symlink_metadata(to) {
        Ok(_) => Err(io::Error::from(io::ErrorKind::AlreadyExists)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::rename(from, to),
        Err(error) => Err(error),
    }
}

/// Why something was not moved to the Trash. Nothing is deleted
/// permanently in any of these cases.
#[derive(Debug)]
pub enum TrashError {
    /// The file or folder is no longer there.
    NotFound,
    /// It is on a network share or a disk whose kind cannot be told (or the
    /// path is relative). Network shares have no Trash, and deleting there
    /// would be for good, so it is left alone.
    NotLocal,
    /// It is a disk, or the top folder of one.
    Root,
    /// It could not be looked at.
    Io(io::Error),
    /// The system's Trash refused.
    Failed(trash::Error),
}

impl fmt::Display for TrashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("the item is not there"),
            Self::NotLocal => f.write_str("the item is not on one of this computer's own disks"),
            Self::Root => f.write_str("a disk cannot be moved to the Trash"),
            Self::Io(error) => error.fmt(f),
            Self::Failed(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for TrashError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Failed(error) => Some(error),
            Self::NotFound | Self::NotLocal | Self::Root => None,
        }
    }
}

/// Moves the file or folder at `path` (a link itself, not what it points
/// to) to the system's Trash or Recycle Bin. Refuses anything not on one of
/// this computer's own disks rather than delete it for good. On Windows a
/// local disk without a Recycle Bin makes the system refuse too.
pub fn move_to_trash(path: &Path) -> Result<(), TrashError> {
    check_trashable(path)?;
    #[allow(unused_mut)]
    let mut context = trash::TrashContext::new();
    // Asking Finder would need permission to control it; the file manager
    // call needs none. The price is that Finder may not offer Put Back.
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        context.set_delete_method(DeleteMethod::NsFileManager);
    }
    context.delete(path).map_err(|error| match error {
        trash::Error::TargetedRoot => TrashError::Root,
        error => TrashError::Failed(error),
    })
}

/// Everything [`move_to_trash`] checks before it asks the system.
fn check_trashable(path: &Path) -> Result<(), TrashError> {
    if !path.is_absolute() {
        return Err(TrashError::NotLocal);
    }
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(TrashError::NotFound),
        Err(error) => return Err(TrashError::Io(error)),
    };
    let Some(folder) = path.parent() else {
        return Err(TrashError::Root);
    };
    let local = if meta.file_type().is_symlink() {
        // `on_local_disks` would follow the link; what is moved is the
        // link in its folder. That folder's own disk is only looked at
        // through the folders above it, so it must not be a mount.
        crate::volumes::on_local_disks(folder) && !separate_mount(folder)
    } else {
        // Every folder on the way is checked, down to the one holding it.
        crate::volumes::on_local_disks(path)
    };
    if !local {
        return Err(TrashError::NotLocal);
    }
    if meta.is_dir() && separate_mount(path) {
        return Err(TrashError::Root);
    }
    Ok(())
}

/// Whether the folder `dir` is the top of a disk other than its parent's,
/// or cannot be told. The root of the file system is not.
#[cfg(unix)]
fn separate_mount(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(dir), fs::metadata(dir.join(".."))) {
        (Ok(inner), Ok(outer)) => inner.dev() != outer.dev(),
        _ => true,
    }
}

/// Windows drives are told apart by letter, which `on_local_disks` has
/// already looked at, and the Recycle Bin refuses a drive's root itself.
#[cfg(not(unix))]
fn separate_mount(_: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names in `dir`, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Whether `dir` is on a file system that ignores case.
    fn ignores_case(dir: &Path) -> bool {
        let probe = dir.join(".case-probe");
        fs::write(&probe, b"").unwrap();
        let ignores = dir.join(".CASE-PROBE").exists();
        fs::remove_file(probe).unwrap();
        ignores
    }

    #[test]
    fn names_that_work_everywhere_pass() {
        for name in [
            "a.md",
            "未命名.md",
            "第2章 草稿.md",
            ".hidden",
            " leading space.md",
            "conference.md",
            "CONSOLE.md",
            "COM10.txt",
            "LPT0",
            "what?.md",
            "会議：議事録.md",
        ] {
            assert_eq!(check_name(name), Ok(()), "{name:?}");
        }
        assert_eq!(check_name(&"あ".repeat(85)), Ok(()));
    }

    #[test]
    fn names_that_fail_somewhere_are_refused() {
        assert_eq!(check_name(""), Err(NameError::Empty));
        assert_eq!(check_name("   "), Err(NameError::Empty));
        assert_eq!(check_name("."), Err(NameError::Dots));
        assert_eq!(check_name(".."), Err(NameError::Dots));
        assert_eq!(check_name("a/b"), Err(NameError::Separator('/')));
        assert_eq!(check_name("a\\b"), Err(NameError::Separator('\\')));
        assert_eq!(check_name("a:b"), Err(NameError::Separator(':')));
        assert_eq!(check_name("../x"), Err(NameError::Separator('/')));
        assert_eq!(check_name("a\0b"), Err(NameError::ControlChar));
        assert_eq!(check_name("a\nb"), Err(NameError::ControlChar));
        assert_eq!(check_name("tab\t"), Err(NameError::ControlChar));
        assert_eq!(check_name("\u{7f}"), Err(NameError::ControlChar));
        assert_eq!(check_name("a."), Err(NameError::TrailingDotOrSpace));
        assert_eq!(check_name("a.md "), Err(NameError::TrailingDotOrSpace));
        assert_eq!(check_name("..."), Err(NameError::TrailingDotOrSpace));
        assert_eq!(check_name(&"a".repeat(256)), Err(NameError::TooLong));
        assert_eq!(check_name(&"あ".repeat(86)), Err(NameError::TooLong));
        assert_eq!(check_name(&"a".repeat(255)), Ok(()));
    }

    #[test]
    fn windows_device_names_are_refused_in_any_form() {
        for name in [
            "CON",
            "con",
            "Prn",
            "aux.md",
            "nul.tar.gz",
            "COM1",
            "com9.txt",
            "LPT1",
            "lpt5.md",
            "COM¹",
            "lpt³.txt",
            "CON .txt",
            "conin$",
            "CONOUT$.log",
        ] {
            assert_eq!(check_name(name), Err(NameError::Reserved), "{name:?}");
        }
    }

    #[test]
    fn new_names_count_up_from_the_bare_one() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        assert_eq!(new_file_name(dir, "未命名", "md"), dir.join("未命名.md"));
        fs::write(dir.join("未命名.md"), b"").unwrap();
        assert_eq!(new_file_name(dir, "未命名", "md"), dir.join("未命名 2.md"));
        fs::write(dir.join("未命名 2.md"), b"").unwrap();
        // A folder takes a file's name too.
        fs::create_dir(dir.join("未命名 3.md")).unwrap();
        assert_eq!(new_file_name(dir, "未命名", "md"), dir.join("未命名 4.md"));
        // Another extension is another name.
        assert_eq!(new_file_name(dir, "未命名", "txt"), dir.join("未命名.txt"));

        assert_eq!(new_folder_name(dir, "新文件夹"), dir.join("新文件夹"));
        fs::create_dir(dir.join("新文件夹")).unwrap();
        assert_eq!(new_folder_name(dir, "新文件夹"), dir.join("新文件夹 2"));
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_link_takes_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        std::os::unix::fs::symlink(dir.join("gone"), dir.join("Untitled.md")).unwrap();
        assert_eq!(
            new_file_name(dir, "Untitled", "md"),
            dir.join("Untitled 2.md")
        );
        let made = create_untitled_file(dir, "Untitled", "md").unwrap();
        assert_eq!(made, dir.join("Untitled 2.md"));
        assert!(!dir.join("gone").exists());
    }

    #[test]
    fn creating_never_touches_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        let file = create_file(dir, "a.md").unwrap();
        assert_eq!(file, dir.join("a.md"));
        fs::write(&file, b"keep").unwrap();
        let error = create_file(dir, "a.md").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&file).unwrap(), b"keep");

        let folder = create_folder(dir, "notes").unwrap();
        assert!(folder.is_dir());
        let error = create_folder(dir, "notes").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            create_folder(dir, "a.md").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            create_file(dir, "notes").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );

        // A bad name comes back as InvalidInput carrying the reason.
        let error = create_file(dir, "../escape.md").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let reason = error.get_ref().unwrap().downcast_ref::<NameError>();
        assert_eq!(reason, Some(&NameError::Separator('/')));
        assert!(create_folder(dir, "CON").is_err());
        assert_eq!(names(dir), ["a.md", "notes"]);
    }

    #[test]
    fn untitled_items_take_the_next_free_number() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        let first = create_untitled_file(dir, "Untitled", "md").unwrap();
        let second = create_untitled_file(dir, "Untitled", "md").unwrap();
        assert_eq!(first, dir.join("Untitled.md"));
        assert_eq!(second, dir.join("Untitled 2.md"));
        let folder = create_untitled_folder(dir, "New Folder").unwrap();
        let again = create_untitled_folder(dir, "New Folder").unwrap();
        assert_eq!(folder, dir.join("New Folder"));
        assert_eq!(again, dir.join("New Folder 2"));
        assert!(again.is_dir());
        // A base that cannot be a name is refused, not retried.
        let error = create_untitled_file(dir, "a/b", "md").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn renaming_moves_within_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        fs::write(dir.join("a.md"), b"text").unwrap();
        let renamed = rename(&dir.join("a.md"), "b.md").unwrap();
        assert_eq!(renamed, dir.join("b.md"));
        assert_eq!(fs::read(&renamed).unwrap(), b"text");
        assert_eq!(names(dir), ["b.md"]);

        fs::create_dir(dir.join("folder")).unwrap();
        fs::write(dir.join("folder/inner.md"), b"").unwrap();
        let renamed = rename(&dir.join("folder"), "章节").unwrap();
        assert!(renamed.join("inner.md").is_file());
        assert_eq!(names(dir), ["b.md", "章节"]);

        // The same name is nothing to do.
        assert_eq!(rename(&dir.join("b.md"), "b.md").unwrap(), dir.join("b.md"));
    }

    #[test]
    fn renaming_never_replaces_anything() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        fs::write(dir.join("a.md"), b"a").unwrap();
        fs::write(dir.join("b.md"), b"b").unwrap();
        fs::create_dir(dir.join("empty")).unwrap();
        fs::create_dir(dir.join("other")).unwrap();
        assert!(matches!(
            rename(&dir.join("a.md"), "b.md"),
            Err(RenameError::Exists)
        ));
        assert!(matches!(
            rename(&dir.join("a.md"), "empty"),
            Err(RenameError::Exists)
        ));
        // A plain rename would replace an empty folder with another.
        assert!(matches!(
            rename(&dir.join("other"), "empty"),
            Err(RenameError::Exists)
        ));
        assert!(matches!(
            rename(&dir.join("other"), "a.md"),
            Err(RenameError::Exists)
        ));
        assert_eq!(fs::read(dir.join("a.md")).unwrap(), b"a");
        assert_eq!(fs::read(dir.join("b.md")).unwrap(), b"b");
        assert_eq!(names(dir), ["a.md", "b.md", "empty", "other"]);
    }

    #[test]
    fn renaming_checks_the_name_and_the_item() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        fs::write(dir.join("a.md"), b"").unwrap();
        assert!(matches!(
            rename(&dir.join("a.md"), "../a.md"),
            Err(RenameError::Name(NameError::Separator('/')))
        ));
        assert!(matches!(
            rename(&dir.join("a.md"), ".."),
            Err(RenameError::Name(NameError::Dots))
        ));
        assert!(matches!(
            rename(&dir.join("a.md"), "nul.md"),
            Err(RenameError::Name(NameError::Reserved))
        ));
        assert!(matches!(
            rename(&dir.join("missing.md"), "b.md"),
            Err(RenameError::NotFound)
        ));
        assert!(matches!(
            rename(Path::new("/"), "x"),
            Err(RenameError::Io(_))
        ));
        assert_eq!(names(dir), ["a.md"]);
    }

    #[test]
    fn a_change_of_case_alone_works() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        fs::write(dir.join("readme.md"), b"text").unwrap();
        let renamed = rename(&dir.join("readme.md"), "README.md").unwrap();
        assert_eq!(renamed, dir.join("README.md"));
        assert_eq!(names(dir), ["README.md"]);
        assert_eq!(fs::read(&renamed).unwrap(), b"text");

        fs::create_dir(dir.join("notes")).unwrap();
        fs::write(dir.join("notes/x.md"), b"").unwrap();
        rename(&dir.join("notes"), "Notes").unwrap();
        assert_eq!(names(dir), ["Notes", "README.md"]);
        assert!(dir.join("Notes/x.md").is_file());

        // Where case counts, a second item spelt the other way is kept.
        if !ignores_case(dir) {
            fs::write(dir.join("a.md"), b"a").unwrap();
            fs::write(dir.join("A.md"), b"A").unwrap();
            assert!(matches!(
                rename(&dir.join("a.md"), "A.md"),
                Err(RenameError::Exists)
            ));
            assert_eq!(fs::read(dir.join("A.md")).unwrap(), b"A");
        }
    }

    #[test]
    fn a_failed_second_step_puts_the_old_name_back() {
        // What happens when the new name finds a different item, as on a
        // file system that tells case apart.
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        fs::write(dir.join("a.md"), b"a").unwrap();
        fs::write(dir.join("b.md"), b"b").unwrap();
        let error =
            rename_through_temporary(&dir.join("a.md"), &dir.join("b.md"), dir).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(names(dir), ["a.md", "b.md"]);
        assert_eq!(fs::read(dir.join("a.md")).unwrap(), b"a");
        assert_eq!(fs::read(dir.join("b.md")).unwrap(), b"b");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_renamed_itself() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        fs::write(dir.join("target.md"), b"t").unwrap();
        std::os::unix::fs::symlink("target.md", dir.join("link.md")).unwrap();
        rename(&dir.join("link.md"), "renamed.md").unwrap();
        assert_eq!(names(dir), ["renamed.md", "target.md"]);
        assert!(
            fs::symlink_metadata(dir.join("renamed.md"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_fallback_never_replaces_either() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        fs::write(dir.join("a.md"), b"a").unwrap();
        fs::write(dir.join("b.md"), b"b").unwrap();
        fs::create_dir(dir.join("x")).unwrap();
        fs::create_dir(dir.join("y")).unwrap();
        let kind = |result: io::Result<()>| result.unwrap_err().kind();
        assert_eq!(
            kind(rename_fallback(&dir.join("a.md"), &dir.join("b.md"))),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            kind(rename_fallback(&dir.join("x"), &dir.join("y"))),
            io::ErrorKind::AlreadyExists
        );
        rename_fallback(&dir.join("a.md"), &dir.join("c.md")).unwrap();
        rename_fallback(&dir.join("x"), &dir.join("z")).unwrap();
        assert_eq!(names(dir), ["b.md", "c.md", "y", "z"]);
        assert_eq!(fs::read(dir.join("c.md")).unwrap(), b"a");
    }

    #[test]
    fn only_local_items_that_are_there_may_go_to_the_trash() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        fs::write(dir.join("a.md"), b"").unwrap();
        fs::create_dir(dir.join("folder")).unwrap();
        assert!(check_trashable(&dir.join("a.md")).is_ok());
        assert!(check_trashable(&dir.join("folder")).is_ok());
        assert!(matches!(
            check_trashable(&dir.join("missing.md")),
            Err(TrashError::NotFound)
        ));
        assert!(matches!(
            check_trashable(Path::new("relative.md")),
            Err(TrashError::NotLocal)
        ));
        assert!(matches!(
            move_to_trash(&dir.join("missing.md")),
            Err(TrashError::NotFound)
        ));
        #[cfg(unix)]
        {
            assert!(matches!(
                check_trashable(Path::new("/")),
                Err(TrashError::Root)
            ));
            // A link is judged by its own folder, even when it points
            // nowhere.
            std::os::unix::fs::symlink(dir.join("gone"), dir.join("dangling")).unwrap();
            assert!(check_trashable(&dir.join("dangling")).is_ok());
        }
        // macOS mounts home folders through autofs: not local.
        #[cfg(target_os = "macos")]
        {
            let automount = Path::new("/System/Volumes/Data/home");
            if automount.exists() {
                assert!(check_trashable(automount).is_err());
            }
        }
        assert_eq!(names(dir).len(), if cfg!(unix) { 3 } else { 2 });
    }

    /// Puts a file into the real Trash of whoever runs the tests, so it
    /// only runs when asked for: `cargo test -- --ignored`.
    #[test]
    #[ignore = "puts a file into the real Trash"]
    fn moving_to_the_trash_takes_the_item_away() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("aster-trash-test.md");
        fs::write(&file, b"").unwrap();
        move_to_trash(&file).unwrap();
        assert!(!file.exists());
        let folder = dir.path().join("aster-trash-test folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("inner.md"), b"").unwrap();
        move_to_trash(&folder).unwrap();
        assert!(!folder.exists());
    }
}
