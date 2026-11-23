//! What the sidebar and the start page say about files: the recent list and
//! the folder tree as lines of text in the interface language.
//!
//! The lists themselves live in `aster-workspace`; this module only words
//! them. File and folder names are content, not interface text, and are shown
//! as they are.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aster_workspace::{FileKind, Filter, Note, ReadError, RecentFiles, Row, RowKind};
use chrono::{DateTime, Datelike, Local, Timelike};

use crate::files::display_name;
use crate::i18n::{Strings, fill};
use crate::stats::group_digits;
use crate::tabs::DocKind;

/// How many recent files the sidebar lists while not filtering, and how many
/// rows its recent section is always tall. The start page lists them all.
pub const SIDEBAR_RECENT: usize = 5;

/// The editor a kind of file belongs to, for its icon.
pub fn doc_kind(kind: FileKind) -> DocKind {
    match kind {
        FileKind::Markdown | FileKind::Text => DocKind::Markdown,
        FileKind::Word => DocKind::Writer,
        FileKind::Spreadsheet => DocKind::Sheets,
        FileKind::Presentation => DocKind::Slides,
        FileKind::Pdf => DocKind::Pdf,
    }
}

/// One recent file as a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentRow {
    pub path: PathBuf,
    /// The file name.
    pub title: String,
    pub kind: DocKind,
    /// The folder it is in.
    pub location: String,
    /// When it was last opened.
    pub opened: String,
    pub missing: bool,
}

/// The recent files to show, newest first, those whose names match `filter`.
pub fn recent_rows(
    strings: &Strings,
    recent: &RecentFiles,
    filter: Option<&Filter>,
    now: DateTime<Local>,
    home: Option<&Path>,
) -> Vec<RecentRow> {
    recent
        .files()
        .iter()
        .filter_map(|file| {
            let title = display_name(&file.path);
            if filter.is_some_and(|filter| !filter.matches(&title)) {
                return None;
            }
            Some(RecentRow {
                kind: FileKind::of(&file.path).map_or(DocKind::Markdown, doc_kind),
                location: location_label(&file.path, home),
                opened: opened_label(strings, file.opened, now),
                missing: file.missing,
                path: file.path.clone(),
                title,
            })
        })
        .collect()
}

/// When a file was opened, as briefly as is clear: the time today and
/// yesterday, the date this year, the full date before that.
pub fn opened_label(strings: &Strings, opened: SystemTime, now: DateTime<Local>) -> String {
    let at: DateTime<Local> = opened.into();
    let days = now
        .date_naive()
        .signed_duration_since(at.date_naive())
        .num_days();
    let time = format!("{:02}:{:02}", at.hour(), at.minute());
    let date = |template: &str| {
        let text = fill(template, "year", &at.year().to_string());
        let text = fill(&text, "month", &at.month().to_string());
        fill(&text, "day", &at.day().to_string())
    };
    // A time in the future (the clock was changed) counts as today.
    if days <= 0 {
        fill(strings.opened_today, "time", &time)
    } else if days == 1 {
        fill(strings.opened_yesterday, "time", &time)
    } else if at.year() == now.year() {
        date(strings.opened_this_year)
    } else {
        date(strings.opened_other_year)
    }
}

/// The folder a file is in, with the home folder written as `~`.
pub fn location_label(path: &Path, home: Option<&Path>) -> String {
    let Some(folder) = path.parent() else {
        return String::new();
    };
    if let Some(home) = home
        && let Ok(rest) = folder.strip_prefix(home)
    {
        let mut label = String::from("~");
        for part in rest.components() {
            label.push('/');
            label.push_str(&part.as_os_str().to_string_lossy());
        }
        return label;
    }
    folder.to_string_lossy().into_owned()
}

/// The user's home folder, for [`location_label`].
pub fn home_folder() -> Option<PathBuf> {
    let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// How a line of the tree looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Folder {
        expanded: bool,
    },
    /// A link to a folder above it.
    Loop,
    File {
        kind: DocKind,
        opens: bool,
    },
    /// Not an entry: a message about the folder above it.
    Note {
        problem: bool,
    },
}

/// One line of the tree in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeLine {
    pub depth: usize,
    pub label: String,
    pub look: Look,
    pub path: PathBuf,
}

pub fn tree_lines(strings: &Strings, rows: &[Row]) -> Vec<TreeLine> {
    rows.iter()
        .map(|row| {
            let (label, look) = match &row.kind {
                RowKind::Folder { expanded } => (
                    row.name.clone(),
                    Look::Folder {
                        expanded: *expanded,
                    },
                ),
                RowKind::Loop => (row.name.clone(), Look::Loop),
                RowKind::File(kind) => (
                    row.name.clone(),
                    Look::File {
                        kind: doc_kind(*kind),
                        opens: kind.opens(),
                    },
                ),
                RowKind::Note(note) => (
                    note_text(strings, note),
                    Look::Note {
                        problem: matches!(note, Note::Failed(_)),
                    },
                ),
            };
            TreeLine {
                depth: row.depth,
                label,
                look,
                path: row.path.clone(),
            }
        })
        .collect()
}

pub fn note_text(strings: &Strings, note: &Note) -> String {
    match note {
        Note::Reading => strings.tree_reading.to_owned(),
        Note::Empty => strings.tree_empty.to_owned(),
        Note::Omitted { count, at_least } => fill(
            if *at_least {
                strings.tree_omitted_at_least
            } else {
                strings.tree_omitted
            },
            "count",
            &group_digits(*count),
        ),
        Note::Failed(err) => read_error_text(strings, err),
    }
}

pub fn read_error_text(strings: &Strings, err: &ReadError) -> String {
    match err {
        ReadError::Missing => strings.tree_missing.to_owned(),
        ReadError::Denied => strings.tree_denied.to_owned(),
        ReadError::NotFolder => strings.tree_not_folder.to_owned(),
        ReadError::Other(reason) => fill(strings.tree_failed, "reason", reason),
    }
}

/// What "show in the file manager" is called here.
pub fn reveal_label(strings: &Strings) -> &'static str {
    if cfg!(target_os = "macos") {
        strings.reveal_finder
    } else {
        strings.reveal_other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{EN, Language, ZH_HANS};
    use chrono::TimeZone;

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).single().unwrap()
    }

    #[test]
    fn times_are_as_short_as_is_clear() {
        let now = local(2026, 11, 23, 15, 0);
        let label = |at: DateTime<Local>| opened_label(&ZH_HANS, at.into(), now);
        assert_eq!(label(local(2026, 11, 23, 9, 5)), "今天 09:05");
        assert_eq!(label(local(2026, 11, 22, 23, 59)), "昨天 23:59");
        assert_eq!(label(local(2026, 11, 19, 14, 20)), "11 月 19 日");
        assert_eq!(label(local(2025, 12, 31, 8, 0)), "2025 年 12 月 31 日");
        // Clock set back: still today.
        assert_eq!(label(local(2026, 11, 24, 8, 0)), "今天 08:00");
        let english = opened_label(&EN, local(2025, 3, 4, 0, 0).into(), now);
        assert_eq!(english, "3/4/2025");
        // Every language fills every marker.
        for language in Language::ALL {
            for at in [
                local(2026, 11, 23, 9, 5),
                local(2026, 11, 22, 9, 5),
                local(2026, 1, 2, 9, 5),
                local(2020, 1, 2, 9, 5),
            ] {
                let text = opened_label(language.strings(), at.into(), now);
                assert!(!text.contains('{'), "{language:?}: {text}");
            }
        }
    }

    #[test]
    fn locations_use_a_tilde_for_home() {
        let home = Path::new("/Users/山田");
        assert_eq!(
            location_label(Path::new("/Users/山田/書類/2026/報告.md"), Some(home)),
            "~/書類/2026"
        );
        assert_eq!(
            location_label(Path::new("/Users/山田/a.md"), Some(home)),
            "~"
        );
        assert_eq!(
            location_label(Path::new("/Volumes/共享/a.md"), Some(home)),
            "/Volumes/共享"
        );
        assert_eq!(location_label(Path::new("/tmp/a.md"), None), "/tmp");
    }

    #[test]
    fn recent_rows_filter_by_name_and_keep_order() {
        let mut recent = RecentFiles::default();
        let dir = std::env::temp_dir();
        let now = local(2026, 11, 23, 15, 0);
        recent.add(&dir.join("会议纪要.md"), local(2026, 11, 20, 10, 0).into());
        recent.add(&dir.join("README.txt"), local(2026, 11, 23, 10, 0).into());
        recent.add(&dir.join("報告.docx"), local(2026, 11, 23, 11, 0).into());
        let rows = recent_rows(&ZH_HANS, &recent, None, now, None);
        let titles: Vec<_> = rows.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(titles, ["報告.docx", "README.txt", "会议纪要.md"]);
        assert_eq!(rows[0].kind, DocKind::Writer);
        assert_eq!(rows[1].kind, DocKind::Markdown);
        assert_eq!(rows[0].opened, "今天 11:00");
        let filter = Filter::new("readme").unwrap();
        let rows = recent_rows(&ZH_HANS, &recent, Some(&filter), now, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "README.txt");
    }

    #[test]
    fn notes_say_what_happened() {
        assert_eq!(
            note_text(
                &ZH_HANS,
                &Note::Omitted {
                    count: 12345,
                    at_least: false
                }
            ),
            "还有 12,345 项没有列出"
        );
        assert_eq!(
            note_text(&ZH_HANS, &Note::Failed(ReadError::Denied)),
            "没有读取权限"
        );
        assert_eq!(
            note_text(&EN, &Note::Failed(ReadError::Other("disk on fire".into()))),
            "Can’t read: disk on fire"
        );
        for language in Language::ALL {
            let strings = language.strings();
            for note in [
                Note::Reading,
                Note::Empty,
                Note::Omitted {
                    count: 3,
                    at_least: true,
                },
                Note::Failed(ReadError::Missing),
                Note::Failed(ReadError::NotFolder),
                Note::Failed(ReadError::Other("x".into())),
            ] {
                let text = note_text(strings, &note);
                assert!(
                    !text.is_empty() && !text.contains('{'),
                    "{language:?}: {text}"
                );
            }
        }
    }

    #[test]
    fn tree_rows_become_lines() {
        let rows = [
            Row {
                depth: 0,
                name: "2026".into(),
                path: PathBuf::from("/w/2026"),
                kind: RowKind::Folder { expanded: true },
            },
            Row {
                depth: 1,
                name: "預算.xlsx".into(),
                path: PathBuf::from("/w/2026/預算.xlsx"),
                kind: RowKind::File(FileKind::Spreadsheet),
            },
            Row {
                depth: 1,
                name: "a.md".into(),
                path: PathBuf::from("/w/2026/a.md"),
                kind: RowKind::File(FileKind::Markdown),
            },
            Row {
                depth: 0,
                name: String::new(),
                path: PathBuf::from("/w"),
                kind: RowKind::Note(Note::Failed(ReadError::Denied)),
            },
        ];
        let lines = tree_lines(&ZH_HANS, &rows);
        assert_eq!(lines[0].look, Look::Folder { expanded: true });
        assert_eq!(
            lines[1].look,
            Look::File {
                kind: DocKind::Sheets,
                opens: true
            }
        );
        assert_eq!(
            lines[2].look,
            Look::File {
                kind: DocKind::Markdown,
                opens: true
            }
        );
        assert_eq!(lines[3].look, Look::Note { problem: true });
        assert_eq!(lines[3].label, "没有读取权限");
    }
}
