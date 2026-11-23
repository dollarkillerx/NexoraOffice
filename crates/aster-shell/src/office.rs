//! Excel and PowerPoint files in tabs: reading them on a worker thread,
//! calculating formulas after the workbook is on screen, and the read-only
//! views' colours and keys.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aster_ooxml::{ErrorKind, FormatCodec, Pptx, Xlsx};
use aster_sheets::{CalcEngine, CalcError, Cancel, Workbook};
use aster_slides::Presentation;
use aster_surface::sheet_view::{Direction, SheetKey, SheetStyle, SheetView};
use aster_surface::slide_view::{SlideStyle, SlideView};

use crate::i18n::Strings;
use crate::tabs::DocKind;
use crate::theme;

pub const SHEET_EXTENSIONS: &[&str] = &["xlsx", "xlsm"];
pub const SLIDE_EXTENSIONS: &[&str] = &["pptx", "pptm"];
/// Files larger than this are not read: the package limits would refuse
/// them anyway once inflated, and reading them takes the memory first.
pub const MAX_FILE: u64 = 1 << 30;

/// Which office view a file opens in, by its extension.
pub fn kind_of(path: &Path) -> Option<DocKind> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if SHEET_EXTENSIONS.contains(&extension.as_str()) {
        Some(DocKind::Sheets)
    } else if SLIDE_EXTENSIONS.contains(&extension.as_str()) {
        Some(DocKind::Slides)
    } else {
        None
    }
}

pub enum Loaded {
    Workbook(Workbook),
    Presentation(Presentation),
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    TooLarge,
    Format(aster_ooxml::Error),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io(err) => err.fmt(f),
            LoadError::TooLarge => f.write_str("file too large"),
            LoadError::Format(err) => err.fmt(f),
        }
    }
}

impl LoadError {
    /// What the error dialog says.
    pub fn message(&self, strings: &Strings) -> String {
        match self {
            LoadError::Io(err) => err.to_string(),
            LoadError::TooLarge => strings.office_too_large.to_owned(),
            LoadError::Format(err) => match err.kind() {
                ErrorKind::LegacyOrEncrypted => strings.office_legacy.to_owned(),
                ErrorKind::NotZip => strings.office_not_office.to_owned(),
                ErrorKind::WrongKind => strings.office_wrong_kind.to_owned(),
                ErrorKind::TooLarge => strings.office_too_large.to_owned(),
                ErrorKind::Broken => {
                    crate::i18n::fill(strings.office_broken, "reason", &err.to_string())
                }
            },
        }
    }
}

/// Read and decode a workbook or presentation. Runs on a worker thread.
pub fn load(path: &Path) -> Result<Loaded, LoadError> {
    let size = std::fs::metadata(path).map_err(LoadError::Io)?.len();
    if size > MAX_FILE {
        return Err(LoadError::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(LoadError::Io)?;
    match kind_of(path) {
        Some(DocKind::Slides) => Pptx
            .read(bytes)
            .map(Loaded::Presentation)
            .map_err(LoadError::Format),
        _ => Xlsx
            .read(bytes)
            .map(Loaded::Workbook)
            .map_err(LoadError::Format),
    }
}

/// Where a workbook's calculation stands.
#[derive(Clone, Debug)]
pub enum Calc {
    /// Running on a worker thread; cancelled when the tab closes.
    Running(Cancel),
    /// Done: how many formulas there are.
    Done(usize),
    Failed(String),
}

pub struct SheetTab {
    pub view: SheetView,
    pub path: PathBuf,
    pub calc: Calc,
}

impl SheetTab {
    pub fn new(workbook: Workbook, path: PathBuf, tokens: &theme::Tokens) -> Self {
        let mut view = SheetView::new(Arc::new(workbook));
        view.set_style(sheet_style(tokens));
        Self {
            view,
            path,
            calc: Calc::Done(0),
        }
    }

    pub fn restyle(&mut self, tokens: &theme::Tokens) {
        self.view.set_style(sheet_style(tokens));
    }
}

impl Drop for SheetTab {
    fn drop(&mut self) {
        if let Calc::Running(cancel) = &self.calc {
            cancel.cancel();
        }
    }
}

pub struct SlidesTab {
    pub view: SlideView,
    pub path: PathBuf,
}

impl SlidesTab {
    pub fn new(
        presentation: Presentation,
        path: PathBuf,
        tokens: &theme::Tokens,
        strings: &Strings,
    ) -> Self {
        let mut view = SlideView::new(Arc::new(presentation));
        view.set_style(slide_style(tokens, strings));
        Self { view, path }
    }

    pub fn restyle(&mut self, tokens: &theme::Tokens, strings: &Strings) {
        self.view.set_style(slide_style(tokens, strings));
    }
}

/// Calculate every formula of `workbook` and return a copy with the results
/// and how many formulas there are. Runs on a worker thread.
pub fn calculate(workbook: &Workbook, cancel: &Cancel) -> Result<(Workbook, usize), CalcError> {
    let formulas = workbook.formulas().count();
    if formulas == 0 {
        return Ok((workbook.clone(), 0));
    }
    let mut engine = aster_calc::Formualizer::load(workbook)?;
    let results = engine.apply(&[], cancel)?;
    if cancel.is_cancelled() {
        return Err(CalcError::Cancelled);
    }
    let (updated, _) = workbook.with_results(&results);
    Ok((updated, formulas))
}

fn opaque(color: theme::Rgba, under: theme::Rgba) -> [u8; 4] {
    color.over(under).to_array()
}

pub fn sheet_style(tokens: &theme::Tokens) -> SheetStyle {
    let c = &tokens.colors;
    SheetStyle {
        background: c.surface.to_array(),
        text: c.text.to_array(),
        secondary: opaque(c.text_secondary, c.surface),
        grid: opaque(c.hairline, c.surface),
        header: opaque(c.panel, c.surface),
        header_text: opaque(c.text_secondary, c.panel),
        header_active: opaque(c.accent_soft, c.panel),
        accent: c.accent.to_array(),
    }
}

pub fn slide_style(tokens: &theme::Tokens, strings: &Strings) -> SlideStyle {
    let c = &tokens.colors;
    SlideStyle {
        background: opaque(c.desk, c.surface),
        strip: opaque(c.panel, c.surface),
        hairline: opaque(c.hairline, c.surface),
        text: c.text.to_array(),
        secondary: opaque(c.text_secondary, c.panel),
        accent: c.accent.to_array(),
        picture: strings.shape_picture.to_owned(),
        table: strings.shape_table.to_owned(),
        chart: strings.shape_chart.to_owned(),
        graphic: strings.shape_graphic.to_owned(),
        hidden: strings.slide_hidden.to_owned(),
    }
}

/// Keys the views understand, from what the window reports. Kept apart from
/// the toolkit's key codes: `key` is the name the shell decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Tab,
    Enter,
    Space,
}

pub fn sheet_key(key: Key, shift: bool, primary: bool) -> Option<SheetKey> {
    Some(match (key, primary) {
        (Key::Up, true) => SheetKey::Jump(Direction::Up),
        (Key::Down, true) => SheetKey::Jump(Direction::Down),
        (Key::Left, true) => SheetKey::Jump(Direction::Left),
        (Key::Right, true) => SheetKey::Jump(Direction::Right),
        (Key::Home, true) => SheetKey::Start,
        (Key::End, true) => SheetKey::Last,
        (Key::Up, false) => SheetKey::Up,
        (Key::Down, false) => SheetKey::Down,
        (Key::Left, false) => SheetKey::Left,
        (Key::Right, false) => SheetKey::Right,
        (Key::PageUp, _) => SheetKey::PageUp,
        (Key::PageDown, _) => SheetKey::PageDown,
        (Key::Home, false) => SheetKey::Home,
        (Key::End, false) => SheetKey::End,
        (Key::Tab, _) if shift => SheetKey::Left,
        (Key::Tab, _) => SheetKey::Right,
        (Key::Enter, _) if shift => SheetKey::Up,
        (Key::Enter, _) => SheetKey::Down,
        (Key::Space, _) => return None,
    })
}

/// Move through slides. Returns whether the slide changed.
pub fn slide_key(view: &mut SlideView, key: Key, shift: bool) -> bool {
    match key {
        Key::Up | Key::Left | Key::PageUp => view.previous_slide(),
        Key::Space | Key::Enter if shift => view.previous_slide(),
        Key::Down | Key::Right | Key::PageDown | Key::Space | Key::Enter | Key::Tab => {
            view.next_slide()
        }
        Key::Home => view.go_to(0),
        Key::End => view.last_slide(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_choose_the_view() {
        assert_eq!(kind_of(Path::new("预算.XLSX")), Some(DocKind::Sheets));
        assert_eq!(kind_of(Path::new("a/b/マクロ.xlsm")), Some(DocKind::Sheets));
        assert_eq!(kind_of(Path::new("発表.pptx")), Some(DocKind::Slides));
        assert_eq!(kind_of(Path::new("old.xls")), None);
        assert_eq!(kind_of(Path::new("notes.md")), None);
        assert_eq!(kind_of(Path::new("xlsx")), None);
    }

    #[test]
    fn errors_have_a_message_in_every_language() {
        for language in crate::i18n::Language::ALL {
            let strings = language.strings();
            let legacy = LoadError::Format(aster_ooxml::Error::LegacyOrEncrypted);
            assert_eq!(legacy.message(strings), strings.office_legacy);
            let broken = LoadError::Format(aster_ooxml::Error::Broken("xl/workbook.xml: x".into()));
            assert!(broken.message(strings).contains("xl/workbook.xml"));
        }
    }

    #[test]
    fn a_missing_file_and_a_text_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            load(&dir.path().join("无.xlsx")),
            Err(LoadError::Io(_))
        ));
        let fake = dir.path().join("假的.xlsx");
        std::fs::write(&fake, "这不是表格").unwrap();
        let Err(LoadError::Format(err)) = load(&fake) else {
            panic!("expected a format error")
        };
        assert_eq!(err.kind(), ErrorKind::NotZip);
    }

    #[test]
    fn keys_map_to_moves() {
        assert_eq!(
            sheet_key(Key::Down, false, true),
            Some(SheetKey::Jump(Direction::Down))
        );
        assert_eq!(sheet_key(Key::Tab, true, false), Some(SheetKey::Left));
        assert_eq!(sheet_key(Key::Enter, false, false), Some(SheetKey::Down));
        assert_eq!(sheet_key(Key::Home, false, true), Some(SheetKey::Start));
        assert_eq!(sheet_key(Key::Space, false, false), None);
    }

    #[test]
    fn calculating_an_empty_workbook_needs_no_engine() {
        let workbook = Workbook {
            sheets: vec![aster_sheets::Sheet::empty(
                "Sheet1",
                aster_sheets::SheetKind::Worksheet,
            )],
            ..Workbook::default()
        };
        let (_, formulas) = calculate(&workbook, &Cancel::new()).unwrap();
        assert_eq!(formulas, 0);
    }
}
