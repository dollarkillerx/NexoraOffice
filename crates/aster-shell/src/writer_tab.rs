//! Word documents in tabs: a `.docx` or `.docm` file laid out by
//! [`aster_surface::doc_view::DocView`] and shown read-only. The view looks
//! after pages, caret, selection, find and outline; this module is the tab
//! around it: the file's bytes (saving will start from them), the find bar's
//! state, the counts for the status bar, the colours from the theme, and what
//! keys and clicks mean.
//!
//! The old plain-text Writer tab (`content::WriterView`) is a different thing
//! and stays as it is.

use std::path::PathBuf;
use std::sync::Arc;

use aster_surface::Action;
use aster_surface::doc_view::{DocQuery, DocStyle, DocView, Move, Zoom};
use aster_writer::OBJECT;

use crate::i18n::{self, Strings};
use crate::stats::{TextStats, group_digits};
use crate::theme;

/// A document in a tab.
pub struct DocTab {
    pub view: DocView,
    pub path: PathBuf,
    /// The file as read: saving will start from it.
    pub source: Arc<Vec<u8>>,
    /// Words and characters of the body, counted when the file was opened.
    pub stats: TextStats,
    /// The find bar's state; the matches are in the view.
    pub find: DocFind,
    /// A press on the pages that may turn out to be a click on a link.
    pub press: Option<Press>,
    /// The last link clicked was not opened: only web and e-mail links are.
    pub link_refused: bool,
    /// The heading of the outline the sidebar marks as the one the caret is
    /// under, as it was last shown.
    pub outline_shown: Option<usize>,
}

/// The find bar of a document tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocFind {
    pub open: bool,
    pub query: DocQuery,
}

/// Where the pointer went down, to tell a click from the start of a drag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Press {
    pub x: f64,
    pub y: f64,
    pub clicks: u8,
    /// The pointer has moved far enough since to be a drag.
    pub dragged: bool,
}

/// How far (physical pixels) the pointer may move and still be a click.
const CLICK_SLOP: f64 = 4.0;

impl DocTab {
    pub fn new(
        mut view: DocView,
        source: Arc<Vec<u8>>,
        path: PathBuf,
        tokens: &theme::Tokens,
    ) -> Self {
        view.set_style(doc_style(tokens));
        let stats = TextStats::of(&view.document().plain_text().replace(OBJECT, ""));
        Self {
            view,
            path,
            source,
            stats,
            find: DocFind::default(),
            press: None,
            link_refused: false,
            outline_shown: None,
        }
    }

    pub fn restyle(&mut self, tokens: &theme::Tokens) {
        self.view.set_style(doc_style(tokens));
    }

    // --- Find ---

    /// Open the find bar; a query left in it is searched again.
    pub fn open_find(&mut self) {
        self.find.open = true;
        if !self.find.query.pattern.is_empty() {
            self.search();
        }
    }

    /// Close the find bar: the marks go.
    pub fn close_find(&mut self) {
        self.find.open = false;
        self.view.clear_find();
    }

    pub fn set_query(&mut self, pattern: String) {
        self.find.query.pattern = pattern;
        self.search();
    }

    pub fn toggle_match_case(&mut self) {
        self.find.query.case_sensitive = !self.find.query.case_sensitive;
        self.search();
    }

    pub fn toggle_whole_word(&mut self) {
        self.find.query.whole_word = !self.find.query.whole_word;
        self.search();
    }

    /// Search for the query now in the bar; the first match at or after the
    /// caret is shown.
    fn search(&mut self) {
        if self.find.query.pattern.is_empty() {
            self.view.clear_find();
            return;
        }
        self.view.set_find(self.find.query.clone());
        if let Some(current) = self.view.current_hit() {
            self.view.go_to_hit(current);
        }
    }

    /// Go to the next or previous match, wrapping round. False if none.
    pub fn find_step(&mut self, forward: bool) -> bool {
        self.view.find_step(forward)
    }

    // --- Zoom ---

    /// The zoom commands: steps up and down, and 100 %.
    pub fn zoom(&mut self, zoom_in: Option<bool>) -> bool {
        match zoom_in {
            Some(true) => self.view.zoom_in(),
            Some(false) => self.view.zoom_out(),
            None => self.view.set_zoom(Zoom::Percent(100.0)),
        }
    }

    /// "Fit width", or "fit page" when the width is fitted already.
    pub fn fit(&mut self) -> bool {
        let next = if self.view.zoom() == Zoom::FitWidth {
            Zoom::FitPage
        } else {
            Zoom::FitWidth
        };
        self.view.set_zoom(next)
    }

    // --- The pointer ---

    /// The pointer went down on the pages (physical pixels): `clicks` is 1,
    /// 2 or 3. Returns whether the picture changed.
    pub fn pointer_down(&mut self, x: f64, y: f64, clicks: u8, shift: bool) -> bool {
        self.link_refused = false;
        self.press = Some(Press {
            x,
            y,
            clicks,
            dragged: false,
        });
        self.view.press(x, y, clicks, shift)
    }

    /// The pointer moved with the button down.
    pub fn pointer_drag(&mut self, x: f64, y: f64) -> bool {
        if let Some(press) = &mut self.press
            && ((x - press.x).abs() > CLICK_SLOP || (y - press.y).abs() > CLICK_SLOP)
        {
            press.dragged = true;
        }
        self.view.drag(x, y)
    }

    /// The button went up. If it was a click on a link, the link's target.
    pub fn pointer_up(&mut self) -> Option<String> {
        self.view.release();
        let press = self.press.take()?;
        if press.dragged || press.clicks != 1 || self.view.has_selection() {
            return None;
        }
        self.view.link_at(press.x, press.y)
    }

    // --- The keys ---

    /// A key as an action: the movements and select all are the view's;
    /// anything that would change the text does nothing here. Returns
    /// whether the key was used.
    pub fn key(&mut self, action: &Action) -> bool {
        if matches!(action, Action::SelectAll) {
            self.view.select_all();
            return true;
        }
        let Some((movement, select)) = movement_of(action) else {
            return false;
        };
        self.view.move_caret(movement, select);
        true
    }

    /// The heading the caret is under: the last one at or before it.
    pub fn outline_position(&self) -> Option<usize> {
        let caret = self.view.caret()?;
        self.view
            .outline()
            .iter()
            .rposition(|entry| &entry.pos <= caret)
    }

    /// Page `n` of `count`, counting from 1, as the status bar says it.
    pub fn page_position(&self) -> (usize, usize) {
        (self.view.current_page() + 1, self.view.page_count())
    }
}

/// What a key action means as a movement of the caret and whether it
/// selects.
pub fn movement_of(action: &Action) -> Option<(Move, bool)> {
    Some(match *action {
        Action::Left { select, word } => (if word { Move::WordLeft } else { Move::Left }, select),
        Action::Right { select, word } => {
            (if word { Move::WordRight } else { Move::Right }, select)
        }
        Action::Up { select } => (Move::Up, select),
        Action::Down { select } => (Move::Down, select),
        Action::LineStart { select } => (Move::LineStart, select),
        Action::LineEnd { select } => (Move::LineEnd, select),
        Action::TextStart { select } => (Move::DocStart, select),
        Action::TextEnd { select } => (Move::DocEnd, select),
        Action::PageUp { select } => (Move::PageUp, select),
        Action::PageDown { select } => (Move::PageDown, select),
        _ => return None,
    })
}

/// What the find bar says: nothing for no query, "No results", or "3 / 12"
/// (with "more than" when the search stopped at its limit).
pub fn find_position(strings: &Strings, tab: &DocTab) -> String {
    if tab.find.query.pattern.is_empty() {
        return String::new();
    }
    let hits = tab.view.hit_count();
    if hits == 0 {
        return strings.find_none.to_owned();
    }
    let mut count = group_digits(hits);
    if tab.view.find_truncated() {
        count = i18n::fill(strings.find_too_many, "count", &count);
    }
    let index = tab
        .view
        .current_hit()
        .map_or_else(|| "–".to_owned(), |index| group_digits(index + 1));
    i18n::fill(
        &i18n::fill(strings.find_position, "index", &index),
        "count",
        &count,
    )
}

/// The view's colours from the theme: the canvas and its edge as the other
/// viewers have them, the selection as the PDF view has it (the pages are
/// white in both themes), the caret the theme's.
pub fn doc_style(tokens: &theme::Tokens) -> DocStyle {
    let c = &tokens.colors;
    let opaque = |color: theme::Rgba, under: theme::Rgba| color.over(under).to_array();
    DocStyle {
        background: opaque(c.desk, c.surface),
        hairline: opaque(c.hairline, c.surface),
        caret: c.caret.to_array(),
        selection: match tokens.mode {
            theme::ThemeMode::Light => c.accent.with_alpha(0.35),
            theme::ThemeMode::Dark => c.caret.with_alpha(0.42),
        }
        .to_array(),
        hit: theme::Rgba::rgb(0xffe135).with_alpha(0.55).to_array(),
        current_hit: theme::Rgba::rgb(0xff8c00).with_alpha(0.65).to_array(),
    }
}

#[cfg(test)]
#[path = "../../aster-ooxml/tests/common/docx_sample.rs"]
mod docx_sample;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::office::{Loaded, load};
    use crate::theme::{ThemeMode, Tokens};

    /// The sample document of the DOCX reader's tests, written to a folder.
    fn sample_file(dir: &std::path::Path, name: &str) -> PathBuf {
        let bytes = sample_bytes();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn sample_bytes() -> Vec<u8> {
        super::docx_sample::sample()
    }

    fn tab(dir: &std::path::Path) -> DocTab {
        let path = sample_file(dir, "报告.docx");
        let Ok(Loaded::Document(view, source)) = load(&path) else {
            panic!("the document loads");
        };
        let mut tab = DocTab::new(*view, source, path, &Tokens::of(ThemeMode::Light));
        tab.view.resize(900, 700, 1.0);
        tab
    }

    #[test]
    fn a_docx_file_loads_into_a_laid_out_view_and_keeps_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let tab = tab(dir.path());
        assert_eq!(*tab.source, sample_bytes());
        assert!(tab.view.page_count() >= 3);
        assert!(tab.stats.words > 20 && tab.stats.chars >= tab.stats.words);
        assert!(!tab.view.outline().is_empty());
        let (page, count) = tab.page_position();
        assert_eq!(page, 1);
        assert_eq!(count, tab.view.page_count());
    }

    #[test]
    fn bad_files_are_reported_as_for_pptx() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("无.docx");
        assert!(matches!(
            load(&missing),
            Err(crate::office::LoadError::Io(_))
        ));
        let text = dir.path().join("假的.docx");
        std::fs::write(&text, "这不是文档").unwrap();
        let Err(crate::office::LoadError::Format(error)) = load(&text) else {
            panic!("expected a format error");
        };
        assert_eq!(error.kind(), aster_ooxml::ErrorKind::NotZip);
        // A workbook with the wrong extension is the wrong kind.
        let book = dir.path().join("表格.docx");
        std::fs::write(&book, aster_ooxml::xlsx_save::blank("Sheet1", "Calibri")).unwrap();
        let Err(crate::office::LoadError::Format(error)) = load(&book) else {
            panic!("expected a format error");
        };
        assert_eq!(error.kind(), aster_ooxml::ErrorKind::WrongKind);
        for language in i18n::Language::ALL {
            let strings = language.strings();
            assert!(
                !crate::office::LoadError::Format(error.clone())
                    .message(strings)
                    .is_empty()
            );
        }
    }

    #[test]
    fn keys_move_the_caret_and_edits_do_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut tab = tab(dir.path());
        let before = tab.view.document().clone();
        assert!(tab.key(&Action::Right {
            select: true,
            word: false
        }));
        assert!(tab.view.has_selection());
        assert!(tab.key(&Action::TextEnd { select: false }));
        assert!(tab.key(&Action::PageUp { select: false }));
        assert!(tab.key(&Action::SelectAll));
        assert!(tab.view.selected_text().len() > 200);
        for action in [
            Action::Insert("x".into()),
            Action::Backspace,
            Action::Delete,
            Action::Newline,
            Action::Indent,
        ] {
            assert!(!tab.key(&action), "{action:?}");
        }
        assert_eq!(*tab.view.document(), before);
    }

    #[test]
    fn the_find_bar_counts_steps_and_closes() {
        let dir = tempfile::tempdir().unwrap();
        let mut tab = tab(dir.path());
        tab.open_find();
        assert_eq!(find_position(i18n::Language::En.strings(), &tab), "");
        tab.set_query("the".into());
        let hits = tab.view.hit_count();
        assert!(hits > 1);
        let strings = i18n::Language::En.strings();
        let said = |index: usize| {
            i18n::fill(
                &i18n::fill(strings.find_position, "index", &index.to_string()),
                "count",
                &group_digits(hits),
            )
        };
        assert_eq!(find_position(strings, &tab), said(1));
        assert!(tab.find_step(true));
        assert_eq!(find_position(strings, &tab), said(2));
        tab.toggle_match_case();
        assert!(tab.view.hit_count() <= hits);
        tab.toggle_whole_word();
        tab.set_query("no such words anywhere".into());
        assert_eq!(find_position(strings, &tab), strings.find_none);
        tab.close_find();
        assert_eq!(tab.view.hit_count(), 0);
        assert!(!tab.find.open);
    }

    #[test]
    fn zoom_and_fit_commands() {
        let dir = tempfile::tempdir().unwrap();
        let mut tab = tab(dir.path());
        assert!(tab.zoom(None));
        assert_eq!(tab.view.zoom_percent(), 100.0);
        assert!(tab.zoom(Some(true)));
        assert!(tab.view.zoom_percent() > 100.0);
        assert!(tab.zoom(Some(false)));
        assert!(tab.fit());
        assert_eq!(tab.view.zoom(), Zoom::FitWidth);
        assert!(tab.fit());
        assert_eq!(tab.view.zoom(), Zoom::FitPage);
    }

    #[test]
    fn a_click_without_a_drag_is_a_click() {
        let dir = tempfile::tempdir().unwrap();
        let mut tab = tab(dir.path());
        tab.zoom(None);
        tab.pointer_down(300.0, 200.0, 1, false);
        tab.pointer_drag(302.0, 201.0);
        assert!(!tab.press.expect("pressed").dragged);
        tab.pointer_drag(340.0, 201.0);
        assert!(tab.press.expect("pressed").dragged);
        assert_eq!(tab.pointer_up(), None);
        assert!(tab.press.is_none());
    }

    #[test]
    fn the_colours_follow_the_theme() {
        let light = doc_style(&Tokens::of(ThemeMode::Light));
        let dark = doc_style(&Tokens::of(ThemeMode::Dark));
        assert_ne!(light.background, dark.background);
        assert_ne!(light.selection, dark.selection);
        assert_ne!(light.caret, dark.caret);
        assert!(light.background[3] == 255 && dark.background[3] == 255);
    }
}
