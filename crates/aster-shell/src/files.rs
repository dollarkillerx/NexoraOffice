//! Loading and saving the files behind Markdown tabs.
//!
//! These functions block on the disk. The shell calls them from a worker
//! thread, never from the thread that draws the window.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use aster_markdown::{DecodeError, Decoded, EncodeError, Encoding, Rope, TextFormat};
use aster_storage::Fingerprint;
use aster_workspace::FileKind;

/// Why a file could not be opened.
#[derive(Debug)]
pub enum LoadError {
    /// The system could not read it.
    Io(io::Error),
    /// It was read, but it is not text this version can edit.
    Decode(DecodeError),
    /// It is not UTF-8, but reads as text in these encodings, most likely
    /// first. Which one is right is the user's to say.
    OtherEncoding(Vec<Reading>),
}

/// How the start of a file reads in an encoding, for the user to judge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    pub encoding: Encoding,
    /// The first few lines.
    pub preview: String,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io(err) => err.fmt(f),
            LoadError::Decode(err) => err.fmt(f),
            LoadError::OtherEncoding(_) => f.write_str("the file is not UTF-8"),
        }
    }
}

impl std::error::Error for LoadError {}

/// A file as read from disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub decoded: Decoded,
    /// What the file held, to tell later whether another program changed it.
    pub fingerprint: Fingerprint,
}

/// Read a text file. Nothing is written, whatever the outcome. A file that
/// is not UTF-8 is not guessed at: the encodings it could be in come back
/// as [`LoadError::OtherEncoding`].
pub fn load(path: &Path) -> Result<Loaded, LoadError> {
    let bytes = aster_storage::read(path).map_err(LoadError::Io)?;
    let decoded = match aster_markdown::decode(&bytes) {
        Ok(decoded) => decoded,
        Err(err) => {
            let readings: Vec<Reading> = aster_markdown::guess_encodings(&bytes)
                .into_iter()
                .filter_map(|encoding| reading(&bytes, encoding))
                .collect();
            return Err(if readings.is_empty() {
                LoadError::Decode(err)
            } else {
                LoadError::OtherEncoding(readings)
            });
        }
    };
    Ok(Loaded {
        decoded,
        fingerprint: Fingerprint::of(path, &bytes),
    })
}

/// Read a text file in an encoding the user chose.
pub fn load_as(path: &Path, encoding: Encoding) -> Result<Loaded, LoadError> {
    let bytes = aster_storage::read(path).map_err(LoadError::Io)?;
    let decoded = if encoding == Encoding::Utf8 {
        aster_markdown::decode(&bytes)
    } else {
        aster_markdown::decode_as(&bytes, encoding)
    }
    .map_err(LoadError::Decode)?;
    Ok(Loaded {
        decoded,
        fingerprint: Fingerprint::of(path, &bytes),
    })
}

/// Every encoding a file reads as, for opening it again in another one:
/// UTF-8 first if it is valid UTF-8, then the likely others.
pub fn readings_of(path: &Path) -> io::Result<Vec<Reading>> {
    let bytes = aster_storage::read(path)?;
    let mut encodings = Vec::new();
    if aster_markdown::decode(&bytes).is_ok() {
        encodings.push(Encoding::Utf8);
    }
    encodings.extend(aster_markdown::guess_encodings(&bytes));
    Ok(encodings
        .into_iter()
        .filter_map(|encoding| reading(&bytes, encoding))
        .collect())
}

/// How many bytes of a file the preview of a reading decodes.
const PREVIEW_BYTES: usize = 4096;
const PREVIEW_LINES: usize = 6;

/// The start of `bytes` read in `encoding`, or `None` if that start is not
/// valid in it. A character cut off at the end of the sample is dropped.
fn reading(bytes: &[u8], encoding: Encoding) -> Option<Reading> {
    let decode = |sample: &[u8]| match encoding {
        Encoding::Utf8 => aster_markdown::decode(sample),
        _ => aster_markdown::decode_as(sample, encoding),
    };
    let sample = &bytes[..bytes.len().min(PREVIEW_BYTES)];
    let decoded = match decode(sample) {
        Ok(decoded) => decoded,
        // Cut in the middle of a character: try without it.
        Err(DecodeError::NotUtf8 { offset } | DecodeError::Malformed { offset, .. })
            if sample.len() < bytes.len() && offset + 4 >= sample.len() && offset > 0 =>
        {
            decode(&sample[..offset]).ok()?
        }
        Err(_) => return None,
    };
    let preview = decoded
        .text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(PREVIEW_LINES)
        .map(|line| line.chars().take(80).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    Some(Reading { encoding, preview })
}

/// Why a document could not be saved.
#[derive(Debug)]
pub enum SaveError {
    Io(io::Error),
    /// The file's encoding has no bytes for a character of the text, on
    /// `line` (from 0). Nothing was written.
    Encode {
        error: EncodeError,
        line: usize,
    },
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::Io(err) => err.fmt(f),
            SaveError::Encode { error, .. } => error.fmt(f),
        }
    }
}

/// What became of a file since it was last read or written.
#[derive(Debug)]
pub enum OnDisk {
    Unchanged,
    /// Its content is different: what it holds now, or why that could not
    /// be read.
    Changed(Result<Loaded, LoadError>),
    Missing,
}

/// Look at `path` again, against what it held `earlier`. A file with the
/// same length and modification time counts as unchanged without being
/// read, so that looking costs one `stat` while nothing happens; otherwise
/// it is read and its content compared.
pub fn look_again(path: &Path, earlier: &Fingerprint) -> OnDisk {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return OnDisk::Missing,
        Err(err) => return OnDisk::Changed(Err(LoadError::Io(err))),
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_nanos());
    if meta.len() == earlier.len && modified.is_some() && modified == earlier.modified_nanos {
        return OnDisk::Unchanged;
    }
    match load(path) {
        Ok(loaded) if loaded.fingerprint.same_content(earlier) => OnDisk::Unchanged,
        Err(LoadError::Io(err)) if err.kind() == io::ErrorKind::NotFound => OnDisk::Missing,
        result => OnDisk::Changed(result),
    }
}

/// Write a document's text to its file, in the format the file had. The file
/// is replaced atomically: a failure leaves the previous contents in place.
/// Returns the fingerprint of what was written.
pub fn save(path: &Path, text: &Rope, format: &TextFormat) -> Result<Fingerprint, SaveError> {
    let bytes = aster_markdown::encode_rope(text, format).map_err(|error| SaveError::Encode {
        error,
        line: text.byte_to_line(error.offset.min(text.len_bytes())),
    })?;
    aster_storage::write_atomically(path, &bytes).map_err(SaveError::Io)?;
    Ok(Fingerprint::of(path, &bytes))
}

/// The name a tab shows for a file.
pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// The file name to suggest when a document is saved for the first time:
/// its title, with `.md` added unless the title already has an extension.
pub fn suggested_file_name(title: &str) -> PathBuf {
    // Characters no file system we target accepts in a name.
    let cleaned: String = title
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '\0') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let mut name = PathBuf::from(cleaned.trim());
    if name.as_os_str().is_empty() {
        name = PathBuf::from("untitled");
    }
    if name.extension().is_none() {
        name.set_extension("md");
    }
    name
}

/// Whether two paths name the same file, as far as can be told. Paths that do
/// not exist are compared as written.
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// What to do with something dragged onto the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// Show it in the sidebar.
    Folder,
    /// Open it in a tab: Markdown and text to edit, Word, Excel and PowerPoint
    /// to look at, as "Open..." does.
    File,
    /// Say this version cannot open it.
    Unsupported,
}

/// Whether a file is a picture a Markdown document can show or link to.
pub fn is_picture(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp"
        )
    })
}

/// The Markdown for a picture dropped into a document: `![name](address)`,
/// where the address is relative to the document's folder when the
/// picture is under it (or a few folders up), and absolute otherwise or for
/// a document that was never saved. Paths use `/`; one with spaces or
/// parentheses is written between `<` and `>`.
pub fn picture_reference(document: Option<&Path>, picture: &Path) -> String {
    let name = picture
        .file_stem()
        .map(|s| s.to_string_lossy().replace(['[', ']'], ""))
        .unwrap_or_default();
    let address = document
        .and_then(Path::parent)
        .and_then(|folder| relative(folder, picture))
        .unwrap_or_else(|| picture.to_string_lossy().replace('\\', "/"));
    let address = if address.contains(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        format!("<{address}>")
    } else {
        address
    };
    format!("![{name}]({address})")
}

/// The folder pasted pictures go in, next to the document.
pub const PASTED_FOLDER: &str = "images";

/// Save a picture pasted into the document at `document`: into the
/// `images` folder beside it (made if missing), named after `stamp` (such
/// as `20261124-153012`), with a number added if that name is taken. Never
/// overwrites a file. Returns where it went.
pub fn save_pasted_picture(
    document: &Path,
    stamp: &str,
    extension: &str,
    bytes: &[u8],
) -> io::Result<PathBuf> {
    use std::io::Write;
    let folder = document
        .parent()
        .ok_or_else(|| io::Error::other("the document has no folder"))?
        .join(PASTED_FOLDER);
    std::fs::create_dir_all(&folder)?;
    for n in 1..1000 {
        let name = if n == 1 {
            format!("pasted-{stamp}.{extension}")
        } else {
            format!("pasted-{stamp}-{n}.{extension}")
        };
        let path = folder.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(bytes)?;
                file.sync_all()?;
                return Ok(path);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many pictures with the same name",
    ))
}

/// `target` relative to `folder`, going up at most three folders.
fn relative(folder: &Path, target: &Path) -> Option<String> {
    let folder: Vec<_> = folder.components().collect();
    let target: Vec<_> = target.components().collect();
    let common = folder
        .iter()
        .zip(&target)
        .take_while(|(a, b)| a == b)
        .count();
    // Sharing only the root (or a drive) is no folder in common.
    let shared = folder[..common]
        .iter()
        .filter(|part| matches!(part, std::path::Component::Normal(_)))
        .count();
    if shared == 0 {
        return None;
    }
    let up = folder.len() - common;
    if up > 3 {
        return None;
    }
    let mut parts: Vec<String> = vec!["..".to_owned(); up];
    parts.extend(
        target[common..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().into_owned()),
    );
    Some(parts.join("/"))
}

/// Decide what a dropped path is. Reads whether it is a folder.
pub fn dropped(path: &Path) -> Dropped {
    if path.is_dir() {
        return Dropped::Folder;
    }
    match FileKind::of(path) {
        Some(kind) if kind.opens() => Dropped::File,
        _ => Dropped::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_pictures_go_beside_the_document_and_never_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let document = dir.path().join("笔记.md");
        let first = save_pasted_picture(&document, "20261124-120000", "png", b"one").unwrap();
        assert_eq!(first, dir.path().join("images/pasted-20261124-120000.png"));
        let second = save_pasted_picture(&document, "20261124-120000", "png", b"two").unwrap();
        assert_eq!(
            second,
            dir.path().join("images/pasted-20261124-120000-2.png")
        );
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
        assert_eq!(
            picture_reference(Some(&document), &second),
            "![pasted-20261124-120000-2](images/pasted-20261124-120000-2.png)"
        );
    }

    #[test]
    fn a_file_in_another_encoding_is_offered_in_the_ones_it_reads_as() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gbk.md");
        // "中文标题\n正文" in GBK.
        let gbk: &[u8] = b"\xd6\xd0\xce\xc4\xb1\xea\xcc\xe2\n\xd5\xfd\xce\xc4";
        std::fs::write(&path, gbk).unwrap();
        let Err(LoadError::OtherEncoding(readings)) = load(&path) else {
            panic!("not offered");
        };
        let gb = readings
            .iter()
            .find(|reading| reading.encoding == Encoding::Gb18030)
            .expect("GB18030 offered");
        assert_eq!(gb.preview, "中文标题\n正文");
        let loaded = load_as(&path, Encoding::Gb18030).unwrap();
        assert_eq!(loaded.decoded.text, "中文标题\n正文");
        assert_eq!(loaded.decoded.format.encoding, Encoding::Gb18030);
        // Saved back in the same bytes.
        let text = Rope::from_str(&loaded.decoded.text);
        save(&path, &text, &loaded.decoded.format).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), gbk);
        // A character the encoding has no bytes for is refused, and nothing
        // written. (GB18030 holds all of Unicode; Shift_JIS has no emoji.)
        let text = Rope::from_str("中文\n😀");
        let japanese = TextFormat {
            encoding: Encoding::ShiftJis,
            ..loaded.decoded.format
        };
        match save(&path, &text, &japanese) {
            Err(SaveError::Encode { error, line }) => assert_eq!((error.ch, line), ('😀', 1)),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read(&path).unwrap(), gbk);
        // UTF-8 is offered first when the file is valid UTF-8 too.
        std::fs::write(&path, "plain").unwrap();
        assert_eq!(readings_of(&path).unwrap()[0].encoding, Encoding::Utf8);
    }

    #[test]
    fn a_file_is_looked_at_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        std::fs::write(&path, "one\n").unwrap();
        let first = load(&path).unwrap().fingerprint;
        assert!(matches!(look_again(&path, &first), OnDisk::Unchanged));
        // The same bytes written again: not a change.
        std::fs::write(&path, "one\n").unwrap();
        assert!(matches!(look_again(&path, &first), OnDisk::Unchanged));
        std::fs::write(&path, "two\n").unwrap();
        match look_again(&path, &first) {
            OnDisk::Changed(Ok(loaded)) => assert_eq!(loaded.decoded.text, "two\n"),
            other => panic!("{other:?}"),
        }
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(look_again(&path, &first), OnDisk::Missing));
    }
    use aster_markdown::LineEnding;

    fn write(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn an_edited_crlf_file_is_saved_as_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "notes.md", "# 标题\r\n\r\n第一行\r\n".as_bytes());
        let loaded = load(&path).unwrap().decoded;
        assert_eq!(loaded.text, "# 标题\n\n第一行\n");
        assert_eq!(loaded.format.line_ending, LineEnding::CrLf);

        let edited = format!("{}日本語の行\n", loaded.text);
        save(&path, &Rope::from_str(&edited), &loaded.format).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            "# 标题\r\n\r\n第一行\r\n日本語の行\r\n".as_bytes()
        );
    }

    #[test]
    fn saving_without_editing_leaves_the_bytes_as_they_were() {
        let dir = tempfile::tempdir().unwrap();
        let samples: [&[u8]; 4] = [
            b"lf\nfile\n",
            b"crlf\r\nfile\r\n",
            b"\xEF\xBB\xBFbom\r\n",
            b"no final break",
        ];
        for (index, sample) in samples.into_iter().enumerate() {
            let path = write(&dir, &format!("{index}.txt"), sample);
            let loaded = load(&path).unwrap();
            let text = Rope::from_str(&loaded.decoded.text);
            let written = save(&path, &text, &loaded.decoded.format).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), sample);
            // Same bytes, same fingerprint.
            assert!(written.same_content(&loaded.fingerprint));
        }
    }

    #[test]
    fn a_file_that_is_not_utf8_is_not_guessed_at_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        // "中文" in GBK.
        let bytes = b"\xD6\xD0\xCE\xC4\r\n";
        let path = write(&dir, "gbk.txt", bytes);
        let Err(LoadError::OtherEncoding(readings)) = load(&path) else {
            panic!("not offered");
        };
        assert_eq!(readings[0].encoding, Encoding::Gb18030);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        // Bytes no encoding reads are refused as before.
        let path = write(&dir, "binary.bin", b"\x81\x00\xff\xfe\xfd\x80");
        assert!(matches!(
            load(&path),
            Err(LoadError::Decode(DecodeError::NotUtf8 { .. }) | LoadError::OtherEncoding(_))
        ));
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = load(&dir.path().join("missing.md")).unwrap_err();
        assert!(matches!(err, LoadError::Io(_)));
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn a_failed_save_keeps_the_old_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-folder").join("a.md");
        assert!(save(&path, &Rope::from_str("text"), &TextFormat::default()).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn names_for_tabs_and_for_first_saves() {
        assert_eq!(display_name(Path::new("/tmp/笔记/会议.md")), "会议.md");
        assert_eq!(
            suggested_file_name("未命名笔记 1"),
            PathBuf::from("未命名笔记 1.md")
        );
        assert_eq!(suggested_file_name("notes.txt"), PathBuf::from("notes.txt"));
        assert_eq!(suggested_file_name("a/b:c"), PathBuf::from("a-b-c.md"));
        assert_eq!(suggested_file_name("  "), PathBuf::from("untitled.md"));
    }

    #[test]
    fn the_same_file_is_recognised_through_different_spellings() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "a.md", b"x");
        let other = dir.path().join(".").join("a.md");
        assert!(same_file(&path, &other));
        assert!(!same_file(&path, &dir.path().join("b.md")));
        // Neither exists: compared as written.
        assert!(same_file(Path::new("/no/such"), Path::new("/no/such")));
    }

    #[test]
    fn dropped_paths_are_folders_files_or_not_supported() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(dropped(dir.path()), Dropped::Folder);
        // A folder whose name looks like a file is still a folder.
        let odd = dir.path().join("notes.md");
        std::fs::create_dir(&odd).unwrap();
        assert_eq!(dropped(&odd), Dropped::Folder);
        for name in [
            "说明.md",
            "README.MARKDOWN",
            "todo.txt",
            "预算.xlsx",
            "发布会.PPTX",
            "报告.docx",
            "宏.DOCM",
        ] {
            assert_eq!(dropped(&write(&dir, name, b"x")), Dropped::File, "{name}");
        }
        for name in ["旧报告.doc", "photo.png", "no-extension"] {
            assert_eq!(
                dropped(&write(&dir, name, b"x")),
                Dropped::Unsupported,
                "{name}"
            );
        }
    }

    #[test]
    fn dropped_pictures_are_written_relative_to_the_document() {
        let doc = Path::new("/notes/2026/会议.md");
        assert_eq!(
            picture_reference(Some(doc), Path::new("/notes/2026/图/白板.png")),
            "![白板](图/白板.png)"
        );
        assert_eq!(
            picture_reference(Some(doc), Path::new("/notes/附件/a b.jpg")),
            "![a b](<../附件/a b.jpg>)"
        );
        // Far away or a document never saved: the whole path.
        assert_eq!(
            picture_reference(Some(doc), Path::new("/Users/x/Desktop/a.png")),
            "![a](/Users/x/Desktop/a.png)"
        );
        assert_eq!(
            picture_reference(None, Path::new("/tmp/p.gif")),
            "![p](/tmp/p.gif)"
        );
        assert!(is_picture(Path::new("/a/B.PNG")));
        assert!(!is_picture(Path::new("/a/b.md")));
    }
}
