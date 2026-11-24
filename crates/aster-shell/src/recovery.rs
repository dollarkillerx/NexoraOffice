//! Recovery records of Markdown documents, workbooks and presentations, as
//! the shell sees them. A workbook's or presentation's record has no journal
//! of edits: its checkpoint is the whole file as saving would write it.
//!
//! A record's journal holds the text; its named values say what the text was:
//! the tab's title, the file it belongs to, how that file is written, and what
//! the file held when it was last read or saved, so that a restored document
//! can say whether another program has changed the file since.
//!
//! Loading a record reads files and replays a journal, so it belongs on a
//! worker thread, like everything else here that touches the disk.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use aster_markdown::{Encoding, LineEnding, Rope, TextFormat};
use aster_storage::{FileState, Fingerprint, FoundRecord, RecoveryRoot};

const KIND: &str = "markdown";
const KIND_WORKBOOK: &str = "workbook";
const KIND_PRESENTATION: &str = "presentation";
const KEY_KIND: &str = "kind";
const KEY_TITLE: &str = "title";
const KEY_PATH: &str = "path";
const KEY_FORMAT: &str = "format";
const KEY_DISK: &str = "disk";

/// What a record says about its document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordInfo {
    pub title: String,
    /// The file the document belongs to; `None` if it was never saved.
    pub path: Option<PathBuf>,
    pub format: TextFormat,
    /// What the file held when the document last read or wrote it.
    pub disk: Option<Fingerprint>,
    /// A workbook rather than a Markdown document.
    pub workbook: bool,
    /// A presentation rather than a Markdown document.
    pub presentation: bool,
}

fn encode_format(format: &TextFormat) -> String {
    let mut out = format.line_ending.label().to_ascii_lowercase();
    if format.bom {
        out.push_str(" bom");
    }
    if format.mixed_line_endings {
        out.push_str(" mixed");
    }
    if format.encoding != Encoding::Utf8 {
        out.push_str(" enc=");
        out.push_str(format.encoding.key());
    }
    out
}

fn decode_format(text: &str) -> Option<TextFormat> {
    let mut words = text.split(' ');
    let line_ending = match words.next()? {
        "lf" => LineEnding::Lf,
        "crlf" => LineEnding::CrLf,
        "cr" => LineEnding::Cr,
        _ => return None,
    };
    let mut format = TextFormat {
        line_ending,
        ..TextFormat::default()
    };
    for word in words {
        match word {
            "bom" => format.bom = true,
            "mixed" => format.mixed_line_endings = true,
            word => format.encoding = Encoding::from_key(word.strip_prefix("enc=")?)?,
        }
    }
    Some(format)
}

/// A path as bytes, without loss on the systems that allow names that are not
/// valid Unicode.
#[cfg(unix)]
fn encode_path(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    Some(path.as_os_str().as_bytes().to_vec())
}

#[cfg(unix)]
fn decode_path(bytes: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

/// Elsewhere a path is stored as UTF-8. One that is not valid Unicode is not
/// stored, and the restored document asks where to save.
#[cfg(not(unix))]
fn encode_path(path: &std::path::Path) -> Option<Vec<u8>> {
    path.to_str().map(|text| text.as_bytes().to_vec())
}

#[cfg(not(unix))]
fn decode_path(bytes: &[u8]) -> Option<PathBuf> {
    std::str::from_utf8(bytes).ok().map(PathBuf::from)
}

impl RecordInfo {
    /// The named values to store with a record.
    pub fn to_meta(&self) -> Vec<(String, Vec<u8>)> {
        let mut meta = vec![
            (
                KEY_KIND.to_owned(),
                if self.workbook {
                    KIND_WORKBOOK
                } else if self.presentation {
                    KIND_PRESENTATION
                } else {
                    KIND
                }
                .as_bytes()
                .to_vec(),
            ),
            (KEY_TITLE.to_owned(), self.title.as_bytes().to_vec()),
            (
                KEY_FORMAT.to_owned(),
                encode_format(&self.format).into_bytes(),
            ),
        ];
        if let Some(path) = self.path.as_deref().and_then(encode_path) {
            meta.push((KEY_PATH.to_owned(), path));
        }
        if let Some(disk) = &self.disk {
            meta.push((KEY_DISK.to_owned(), disk.encode().into_bytes()));
        }
        meta
    }

    /// The keys `to_meta` may leave out, which an update must remove.
    pub fn optional_keys() -> Vec<String> {
        vec![KEY_PATH.to_owned(), KEY_DISK.to_owned()]
    }

    pub fn from_meta(meta: &BTreeMap<String, Vec<u8>>) -> Result<Self, String> {
        let text = |key: &str| -> Result<Option<&str>, String> {
            meta.get(key)
                .map(|bytes| std::str::from_utf8(bytes).map_err(|_| format!("{key} is not UTF-8")))
                .transpose()
        };
        let (workbook, presentation) = match text(KEY_KIND)? {
            Some(KIND) => (false, false),
            Some(KIND_WORKBOOK) => (true, false),
            Some(KIND_PRESENTATION) => (false, true),
            Some(other) => return Err(format!("a record of an unknown kind ({other})")),
            None => return Err("the record does not say what it holds".to_owned()),
        };
        let title = text(KEY_TITLE)?.unwrap_or_default().to_owned();
        let format = match text(KEY_FORMAT)? {
            Some(format) => {
                decode_format(format).ok_or_else(|| format!("unknown format {format:?}"))?
            }
            None => TextFormat::default(),
        };
        let path = meta.get(KEY_PATH).and_then(|bytes| decode_path(bytes));
        let disk = text(KEY_DISK)?.and_then(Fingerprint::decode);
        Ok(Self {
            title,
            path,
            format,
            disk,
            workbook,
            presentation,
        })
    }
}

/// A record found at start-up, loaded as far as it could be.
#[derive(Debug)]
pub struct Candidate {
    /// Held until the user decides; restoring adopts it, discarding deletes it.
    pub record: FoundRecord,
    pub info: Option<RecordInfo>,
    /// The text the record replays to, or why it could not be read. Empty
    /// for a workbook.
    pub text: Result<Rope, String>,
    /// A workbook's or presentation's record: the file as saving would
    /// write it.
    pub workbook: Option<Vec<u8>>,
    /// Whatever the shell computed from the text in advance, on the thread
    /// that found the record, so restoring a large document is quick.
    pub prepared: Option<Box<dyn std::any::Any + Send>>,
    /// How the document's file compares with what it held; `None` for a
    /// document that has no file.
    pub disk: Option<FileState>,
    pub last_written: Option<SystemTime>,
}

impl Candidate {
    pub fn is_readable(&self) -> bool {
        self.info.is_some() && self.text.is_ok()
    }
}

/// Load what a record holds. Never fails: what goes wrong is kept in the
/// result, so the user can still see and discard a damaged record.
pub fn load(record: FoundRecord) -> Candidate {
    let last_written = record.last_written();
    let read = || -> Result<(RecordInfo, Rope, Option<Vec<u8>>), String> {
        let journal = record.open_journal().map_err(|err| err.to_string())?;
        journal.integrity().map_err(|err| err.to_string())?;
        let info = RecordInfo::from_meta(&journal.meta().map_err(|err| err.to_string())?)?;
        let replay = journal.replay().map_err(|err| err.to_string())?;
        if info.workbook || info.presentation {
            if replay.checkpoint.is_empty() {
                return Err("the record holds no file".to_owned());
            }
            return Ok((info, Rope::default(), Some(replay.checkpoint)));
        }
        let text = aster_markdown::journal::rebuild(&replay.checkpoint, &replay.commands)?;
        Ok((info, text, None))
    };
    let (info, text, workbook) = match read() {
        Ok((info, text, workbook)) => (Some(info), Ok(text), workbook),
        Err(why) => (None, Err(why), None),
    };
    let disk = info.as_ref().and_then(|info| {
        let path = info.path.as_deref()?;
        Some(match &info.disk {
            Some(earlier) => aster_storage::compare(path, earlier),
            // A path without a fingerprint: all that can be said is whether
            // the file is there.
            None if path.exists() => FileState::Changed,
            None => FileState::Missing,
        })
    });
    Candidate {
        record,
        info,
        text,
        workbook,
        prepared: None,
        disk,
        last_written,
    }
}

/// Every record left behind by a run that did not end normally, loaded, the
/// most recently written first.
pub fn find(root: &RecoveryRoot) -> io::Result<Vec<Candidate>> {
    let mut candidates: Vec<Candidate> = root.scan()?.into_iter().map(load).collect();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.last_written));
    Ok(candidates)
}

/// How long ago something happened, in the unit the dialog shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Age {
    JustNow,
    Minutes(u64),
    Hours(u64),
    Days(u64),
}

pub fn age(elapsed: Duration) -> Age {
    let minutes = elapsed.as_secs() / 60;
    match minutes {
        0 => Age::JustNow,
        1..60 => Age::Minutes(minutes),
        60..1440 => Age::Hours(minutes / 60),
        _ => Age::Days(minutes / 1440),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aster_storage::RecordWriter;

    fn info(path: Option<PathBuf>) -> RecordInfo {
        RecordInfo {
            title: "会议纪要.md".to_owned(),
            path,
            format: TextFormat {
                line_ending: LineEnding::CrLf,
                encoding: Encoding::ShiftJis,
                bom: true,
                mixed_line_endings: false,
            },
            disk: None,
            workbook: false,
            presentation: false,
        }
    }

    fn meta_map(meta: Vec<(String, Vec<u8>)>) -> BTreeMap<String, Vec<u8>> {
        meta.into_iter().collect()
    }

    #[test]
    fn record_info_round_trips() {
        let mut with_file = info(Some(PathBuf::from("/文档/会议纪要.md")));
        with_file.disk = Some(Fingerprint {
            len: 12,
            checksum: 0xabc,
            modified_nanos: Some(1),
        });
        let untitled = RecordInfo {
            title: "未命名笔记 1".to_owned(),
            path: None,
            format: TextFormat::default(),
            disk: None,
            workbook: false,
            presentation: false,
        };
        // A workbook's record says it is one.
        let workbook = RecordInfo {
            title: "帳簿.xlsx".to_owned(),
            path: Some(PathBuf::from("/文档/帳簿.xlsx")),
            format: TextFormat::default(),
            disk: None,
            workbook: true,
            presentation: false,
        };
        for original in [with_file, untitled, workbook] {
            let meta = meta_map(original.to_meta());
            assert_eq!(RecordInfo::from_meta(&meta), Ok(original));
        }
    }

    #[test]
    fn every_format_round_trips() {
        for line_ending in [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr] {
            for bom in [false, true] {
                for mixed_line_endings in [false, true] {
                    for encoding in Encoding::ALL.iter().copied() {
                        let format = TextFormat {
                            line_ending,
                            encoding,
                            bom,
                            mixed_line_endings,
                        };
                        assert_eq!(decode_format(&encode_format(&format)), Some(format));
                    }
                }
            }
        }
        assert_eq!(decode_format("utf16"), None);
        assert_eq!(decode_format("lf extra"), None);
    }

    #[test]
    fn records_of_other_kinds_or_without_a_kind_are_refused() {
        let mut meta = meta_map(info(None).to_meta());
        meta.insert(KEY_KIND.to_owned(), b"sheets".to_vec());
        assert!(RecordInfo::from_meta(&meta).unwrap_err().contains("sheets"));
        meta.remove(KEY_KIND);
        assert!(RecordInfo::from_meta(&meta).is_err());
    }

    fn write_record(root: &RecoveryRoot, info: &RecordInfo, text: &str, edits: &[(usize, &str)]) {
        let writer = RecordWriter::spawn(root.clone(), |_| {});
        writer.open(1, info.to_meta(), text.as_bytes().to_vec());
        let mut current = text.to_owned();
        for (at, insert) in edits {
            let change = aster_markdown::Change {
                range: *at..*at,
                inserted: (*insert).to_owned(),
                removed: String::new(),
                selection: aster_markdown::Selection::caret(at + insert.len()),
            };
            current.insert_str(*at, insert);
            writer.append(1, aster_markdown::journal::encode(&change));
        }
        // Dropped without discarding: what a crash leaves behind.
    }

    #[test]
    fn a_record_left_behind_is_found_with_its_text_and_file_state() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("会议纪要.md");
        std::fs::write(&file, "# 会议\r\n").unwrap();
        let mut with_file = info(Some(file.clone()));
        with_file.disk = Some(Fingerprint::read(&file).unwrap());
        let root = RecoveryRoot::new(dir.path().join("recovery"));
        // Byte offsets: "# 会议\n" is 9 bytes, "第一项" another 9.
        write_record(
            &root,
            &with_file,
            "# 会议\n",
            &[(9, "第一项"), (18, "\n日本語")],
        );

        let candidates = find(&root).unwrap();
        assert_eq!(candidates.len(), 1);
        let candidate = &candidates[0];
        assert!(candidate.is_readable());
        assert_eq!(candidate.info.as_ref(), Some(&with_file));
        assert_eq!(
            candidate.text.as_ref().map(Rope::to_string).as_deref(),
            Ok("# 会议\n第一项\n日本語")
        );
        assert_eq!(candidate.disk, Some(FileState::Unchanged));
        assert!(candidate.last_written.is_some());
        drop(candidates);

        // Another program changes the file; the next start says so.
        std::fs::write(&file, "# 别人改过\r\n").unwrap();
        let candidates = find(&root).unwrap();
        assert_eq!(candidates[0].disk, Some(FileState::Changed));
        drop(candidates);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(find(&root).unwrap()[0].disk, Some(FileState::Missing));
    }

    #[test]
    fn an_untitled_document_has_no_file_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryRoot::new(dir.path());
        write_record(&root, &info(None), "", &[(0, "草稿")]);
        let candidates = find(&root).unwrap();
        assert_eq!(
            candidates[0].text.as_ref().map(Rope::to_string).as_deref(),
            Ok("草稿")
        );
        assert_eq!(candidates[0].disk, None);
    }

    #[test]
    fn a_damaged_record_is_listed_as_unreadable_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryRoot::new(dir.path());
        write_record(&root, &info(None), "text", &[]);
        let found = root.scan().unwrap();
        let record_dir = found[0].dir().to_owned();
        drop(found);
        // Damage the checkpoint file.
        let checkpoint = std::fs::read_dir(&record_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "snap"))
            .unwrap();
        let mut bytes = std::fs::read(&checkpoint).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&checkpoint, bytes).unwrap();

        let candidates = find(&root).unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(!candidates[0].is_readable());
        assert!(candidates[0].text.is_err());
        drop(candidates);
        assert!(record_dir.exists(), "a damaged record must not be deleted");
    }

    #[test]
    fn ages_pick_a_sensible_unit() {
        let minutes = |m: u64| Duration::from_secs(m * 60);
        assert_eq!(age(Duration::from_secs(20)), Age::JustNow);
        assert_eq!(age(minutes(5)), Age::Minutes(5));
        assert_eq!(age(minutes(59)), Age::Minutes(59));
        assert_eq!(age(minutes(60)), Age::Hours(1));
        assert_eq!(age(minutes(1439)), Age::Hours(23));
        assert_eq!(age(minutes(3 * 1440)), Age::Days(3));
    }
}
