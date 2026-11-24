//! Loading and saving the files behind Markdown tabs.
//!
//! These functions block on the disk. The shell calls them from a worker
//! thread, never from the thread that draws the window.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use aster_markdown::{DecodeError, Decoded, Rope, TextFormat};
use aster_storage::Fingerprint;
use aster_workspace::FileKind;

/// Why a file could not be opened.
#[derive(Debug)]
pub enum LoadError {
    /// The system could not read it.
    Io(io::Error),
    /// It was read, but it is not text this version can edit.
    Decode(DecodeError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io(err) => err.fmt(f),
            LoadError::Decode(err) => err.fmt(f),
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

/// Read a text file. Nothing is written, whatever the outcome.
pub fn load(path: &Path) -> Result<Loaded, LoadError> {
    let bytes = aster_storage::read(path).map_err(LoadError::Io)?;
    let decoded = aster_markdown::decode(&bytes).map_err(LoadError::Decode)?;
    Ok(Loaded {
        decoded,
        fingerprint: Fingerprint::of(path, &bytes),
    })
}

/// Write a document's text to its file, in the format the file had. The file
/// is replaced atomically: a failure leaves the previous contents in place.
/// Returns the fingerprint of what was written.
pub fn save(path: &Path, text: &Rope, format: &TextFormat) -> io::Result<Fingerprint> {
    let bytes = aster_markdown::encode_rope(text, format);
    aster_storage::write_atomically(path, &bytes)?;
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
    /// Open it in a tab: Markdown and text to edit, Excel and PowerPoint to
    /// look at, as "Open..." does.
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
    fn a_file_that_is_not_utf8_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        // "中文" in GBK.
        let bytes = b"\xD6\xD0\xCE\xC4\r\n";
        let path = write(&dir, "gbk.txt", bytes);
        let err = load(&path).unwrap_err();
        assert!(matches!(
            err,
            LoadError::Decode(DecodeError::NotUtf8 { offset: 0 })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
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
        ] {
            assert_eq!(dropped(&write(&dir, name, b"x")), Dropped::File, "{name}");
        }
        for name in ["报告.docx", "photo.png", "no-extension"] {
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
