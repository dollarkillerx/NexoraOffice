//! Word documents in tabs: a `.docx` or `.docm` file (or a new document)
//! laid out and edited by [`aster_surface::doc_view::DocView`]. The view looks
//! after pages, caret, selection, typing, find and the outline; this module
//! is the tab around it: the file's bytes (saving starts from them), what
//! saving writes, the recovery record, the find bar's state, the counts for
//! the status bar, the colours from the theme, the toolbar's state, and what
//! keys and clicks mean.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aster_surface::Action;
use aster_surface::doc_pdf::{DocPdfInfo, export_document_with};
use aster_surface::doc_view::{
    Align, DocCommand, DocQuery, DocStyle, DocView, FormatState, LineSpacing, LinkInfo, Move, Tri,
    Unit, Zoom,
};
use aster_writer::edit::Selection;
use aster_writer::{Break, Document, MarginalKind, OBJECT, StoryId};

use crate::i18n::{self, Strings};
use crate::stats::{TextStats, group_digits};
use crate::theme;
use crate::toolbar::{self, Command, DocLook};

/// A document in a tab.
pub struct DocTab {
    pub view: DocView,
    /// The file it is saved to; `None` for a document never saved.
    pub path: Option<PathBuf>,
    /// The file as read (or the blank template): every save starts from it,
    /// comparing the document with what was read.
    pub source: Arc<Vec<u8>>,
    /// Words and characters of the body, as of `stats_revision`.
    pub stats: TextStats,
    stats_revision: u64,
    /// The find bar's state; the matches are in the view.
    pub find: DocFind,
    /// A press on the pages that may turn out to be a click on a link.
    pub press: Option<Press>,
    /// The last link clicked was not opened: only web and e-mail links are.
    pub link_refused: bool,
    /// The heading of the outline the sidebar marks as the one the caret is
    /// under, as it was last shown.
    pub outline_shown: Option<usize>,
    /// The outline as the sidebar shows it: level and text.
    pub outline_rows: Vec<(u8, String)>,
    pub saving: bool,
    /// The recovery record that keeps unsaved changes through a crash.
    pub record: Option<u64>,
    /// The revision last written to the record, and when.
    pub recorded: Option<(u64, Instant)>,
    /// How long making the last record took, milliseconds, set on the
    /// writer thread.
    pub record_cost: Arc<AtomicU64>,
    /// What the file held when it was last read or saved.
    pub disk: Option<aster_storage::Fingerprint>,
    /// What the toolbar was last told the text looks like.
    pub shown_look: Option<(DocLook, bool, bool)>,
    /// Edits that the status bar's counts do not show yet.
    pub stats_due: bool,
    /// The address of the link under the pointer, for the status bar.
    pub hover_link: Option<String>,
    /// Where the caret was when the status bar was last told.
    pub shown_place: Place,
    /// The comments panel as last shown, to leave the list alone (and
    /// where it is scrolled to) when nothing in it changed.
    pub comments_shown: Vec<ShownComment>,
    /// The comment the last click was in, which the panel marks.
    pub comment_current: Option<i64>,
}

/// One row of the comments panel.
#[derive(Clone, Debug, PartialEq)]
pub struct ShownComment {
    pub id: i64,
    pub author: String,
    pub date: String,
    pub text: String,
    pub color: [u8; 4],
    pub current: bool,
}

/// What a save of a document writes.
pub struct DocSave {
    pub source: Arc<Vec<u8>>,
    pub document: Document,
    /// The view's revision the document is of.
    pub revision: u64,
}

/// What a finished write gives back: the file's bytes, and the document
/// read from them, whose sources are those bytes.
pub struct Written {
    pub bytes: Vec<u8>,
    pub document: Document,
}

impl DocSave {
    /// The file's bytes as they would be saved now.
    pub fn bytes(&self) -> Result<Vec<u8>, String> {
        aster_ooxml::docx_save::save((*self.source).clone(), &self.document)
            .map_err(|err| err.to_string())
    }

    /// Make the file and write it in place of `path` in one step, then read
    /// what was written back, so that the tab can go on from it. Runs on a
    /// worker thread.
    pub fn write(&self, path: &Path) -> Result<Written, String> {
        let bytes = self.bytes()?;
        aster_storage::write_atomically(path, &bytes).map_err(|err| err.to_string())?;
        let mut document = aster_ooxml::docx::read(bytes.clone()).map_err(|err| err.to_string())?;
        document.renumber();
        Ok(Written { bytes, document })
    }
}

/// A change to the table the caret is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableEdit {
    RowAbove,
    RowBelow,
    ColumnLeft,
    ColumnRight,
    DeleteRow,
    DeleteColumn,
    DeleteTable,
    MergeCells,
    UnmergeCell,
}

/// Where the caret is, for the menus and the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Body,
    Header,
    Footer,
    Footnote,
    Endnote,
}

/// The most rows and columns the insert table dialog makes.
pub const TABLE_MAX_ROWS: usize = 500;
pub const TABLE_MAX_COLUMNS: usize = 63;

/// What a Word document is exported to a PDF from: the document as it was
/// when the export began, to be laid out and written on a worker thread.
pub struct DocPdf {
    document: Document,
    info: DocPdfInfo,
    /// Tracked changes and comments are drawn as on the screen: the export
    /// follows the Review menu's switch.
    pub show_markup: bool,
}

impl DocPdf {
    /// The PDF file's bytes.
    pub fn bytes(&self) -> Result<Vec<u8>, String> {
        export_document_with(&self.document, &self.info, self.show_markup)
    }
}

/// Why a file was not put in a document as a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureProblem {
    /// Not a PNG, JPEG, GIF, BMP or TIFF file, or one that is damaged.
    NotAPicture,
    /// More pixels than pictures may have.
    TooLarge,
}

/// A picture file's size in pixels, if it is one the Writer tab inserts: a
/// PNG or JPEG with at most [`aster_image::MAX_PIXELS`] pixels that decodes
/// (tried at a small size). A Word file can hold GIF, BMP and TIFF pictures
/// too, but nothing here draws them, so they are not offered: a picture
/// that shows as a grey box is not what was asked for.
pub fn read_picture(bytes: &[u8]) -> Result<(u32, u32), PictureProblem> {
    if !matches!(
        aster_image::sniff(bytes),
        aster_image::Kind::Png | aster_image::Kind::Jpeg
    ) {
        return Err(PictureProblem::NotAPicture);
    }
    let (w, h) = image_size(bytes).ok_or(PictureProblem::NotAPicture)?;
    if w == 0 || h == 0 {
        return Err(PictureProblem::NotAPicture);
    }
    if u64::from(w) * u64::from(h) > aster_image::MAX_PIXELS {
        return Err(PictureProblem::TooLarge);
    }
    match aster_image::decode(bytes, 64) {
        Ok(_) => Ok((w, h)),
        Err(aster_image::Error::TooLarge { .. }) => Err(PictureProblem::TooLarge),
        Err(_) => Err(PictureProblem::NotAPicture),
    }
}

/// The width and height in pixels a PNG, JPEG, GIF, BMP or TIFF file's
/// header gives.
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let be16 = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|b| u32::from(u16::from_be_bytes([b[0], b[1]])))
    };
    let le16 = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|b| u32::from(u16::from_le_bytes([b[0], b[1]])))
    };
    let le32 = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let be32 = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    if bytes.starts_with(b"\x89PNG") {
        return Some((be32(16)?, be32(20)?));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut at = 2;
        while at + 9 < bytes.len() {
            if bytes[at] != 0xFF {
                at += 1;
                continue;
            }
            let marker = bytes[at + 1];
            if marker == 0xFF {
                at += 1;
                continue;
            }
            if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
                return Some((be16(at + 7)?, be16(at + 5)?));
            }
            at += 2 + be16(at + 2)? as usize;
        }
        return None;
    }
    if bytes.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if bytes.starts_with(b"BM") {
        // The size of the header says which kind it is.
        return if le32(14)? == 12 {
            Some((le16(18)?, le16(20)?))
        } else {
            let height = le32(22)? as i32;
            Some((le32(18)?, height.unsigned_abs()))
        };
    }
    let big = match bytes.get(..4)? {
        b"II*\0" => false,
        b"MM\0*" => true,
        _ => return None,
    };
    let u16_at = |at: usize| if big { be16(at) } else { le16(at) };
    let u32_at = |at: usize| if big { be32(at) } else { le32(at) };
    let entries = u16_at(u32_at(4)? as usize)? as usize;
    let first = u32_at(4)? as usize + 2;
    let (mut width, mut height) = (None, None);
    for i in 0..entries.min(200) {
        let at = first + i * 12;
        let value = match u16_at(at + 2)? {
            3 => u16_at(at + 8)?,
            4 => u32_at(at + 8)?,
            _ => continue,
        };
        match u16_at(at)? {
            256 => width = Some(value),
            257 => height = Some(value),
            _ => {}
        }
    }
    Some((width?, height?))
}

/// The find bar of a document tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocFind {
    pub open: bool,
    pub query: DocQuery,
    /// The replace row is shown.
    pub replace_open: bool,
    pub replacement: String,
}

/// Where the pointer went down, to tell a click from the start of a drag.
#[derive(Clone, Debug, PartialEq)]
pub struct Press {
    pub x: f64,
    pub y: f64,
    pub clicks: u8,
    /// The pointer has moved far enough since to be a drag.
    pub dragged: bool,
    /// The address of the link the press was on with Command (Control
    /// elsewhere) held: letting go without a drag opens it.
    pub link: Option<String>,
}

/// How far (physical pixels) the pointer may move and still be a click.
const CLICK_SLOP: f64 = 4.0;

impl DocTab {
    pub fn new(
        mut view: DocView,
        source: Arc<Vec<u8>>,
        path: Option<PathBuf>,
        tokens: &theme::Tokens,
    ) -> Self {
        view.set_style(doc_style(tokens));
        let stats = count(view.document());
        let disk = path
            .as_deref()
            .map(|path| aster_storage::Fingerprint::of(path, &source));
        let stats_revision = view.revision();
        Self {
            view,
            path,
            source,
            stats,
            stats_revision,
            find: DocFind::default(),
            press: None,
            link_refused: false,
            outline_shown: None,
            outline_rows: Vec::new(),
            saving: false,
            record: None,
            recorded: None,
            record_cost: Arc::default(),
            disk,
            shown_look: None,
            stats_due: false,
            hover_link: None,
            shown_place: Place::Body,
            comments_shown: Vec::new(),
            comment_current: None,
        }
    }

    /// A new document made from the blank template.
    pub fn blank(tokens: &theme::Tokens) -> Option<Self> {
        let source = aster_ooxml::docx_save::blank();
        let mut document = aster_ooxml::docx::read(source.clone()).ok()?;
        document.renumber();
        Some(Self::new(
            DocView::new(document),
            Arc::new(source),
            None,
            tokens,
        ))
    }

    pub fn restyle(&mut self, tokens: &theme::Tokens) {
        self.view.set_style(doc_style(tokens));
    }

    pub fn is_modified(&self) -> bool {
        self.view.is_modified()
    }

    /// Count the words again if edits since the last count are due.
    pub fn recount(&mut self) -> bool {
        if self.stats_revision == self.view.revision() {
            self.stats_due = false;
            return false;
        }
        self.stats = count(self.view.document());
        self.stats_revision = self.view.revision();
        self.stats_due = false;
        true
    }

    // --- Tables, links and pictures ---

    /// A table of `rows` by `columns` at the caret.
    pub fn insert_table(&mut self, rows: usize, columns: usize) -> bool {
        self.view.command(DocCommand::InsertTable {
            rows: rows.clamp(1, TABLE_MAX_ROWS),
            columns: columns.clamp(1, TABLE_MAX_COLUMNS),
        })
    }

    /// A change to the table the caret is in.
    pub fn edit_table(&mut self, edit: TableEdit) -> bool {
        self.view.command(match edit {
            TableEdit::RowAbove => DocCommand::InsertRows { above: true },
            TableEdit::RowBelow => DocCommand::InsertRows { above: false },
            TableEdit::ColumnLeft => DocCommand::InsertColumns { left: true },
            TableEdit::ColumnRight => DocCommand::InsertColumns { left: false },
            TableEdit::DeleteRow => DocCommand::DeleteRows,
            TableEdit::DeleteColumn => DocCommand::DeleteColumns,
            TableEdit::DeleteTable => DocCommand::DeleteTable,
            TableEdit::MergeCells => DocCommand::MergeCells,
            TableEdit::UnmergeCell => DocCommand::UnmergeCell,
        })
    }

    /// Where the caret is.
    pub fn place(&self) -> Place {
        match self.view.active_story() {
            StoryId::Body => Place::Body,
            StoryId::Marginal(_) => match self.view.marginal_kind() {
                Some(MarginalKind::Footer) => Place::Footer,
                _ => Place::Header,
            },
            StoryId::Footnote(_) => Place::Footnote,
            StoryId::Endnote(_) => Place::Endnote,
        }
    }

    /// What the link dialog starts from.
    pub fn link_info(&self) -> LinkInfo {
        self.view.link_info()
    }

    /// The link dialog's result: see [`DocCommand::EditLink`].
    pub fn edit_link(&mut self, text: &str, target: &str) -> bool {
        self.view.command(DocCommand::EditLink {
            text: text.to_owned(),
            target: target.to_owned(),
        })
    }

    /// Put a picture file's bytes in at the caret; `pixels` is its size.
    /// False when nothing was put in.
    pub fn insert_picture(&mut self, bytes: Vec<u8>, pixels: (u32, u32)) -> bool {
        self.view
            .insert_picture(Arc::from(bytes), pixels, None)
            .unwrap_or(false)
    }

    // --- Export ---

    /// What a PDF export of the document as it is now needs: the document
    /// (cheap to copy), the title, and the language of its settings.
    pub fn pdf_job(&self, title: &str) -> DocPdf {
        DocPdf {
            document: self.view.document().clone(),
            info: DocPdfInfo {
                title: Some(title.to_owned()).filter(|t| !t.is_empty()),
                language: self.view.document().settings.language.clone(),
            },
            show_markup: self.view.show_markup(),
        }
    }

    // --- Saving ---

    /// What to write, unless a save is running.
    pub fn begin_save(&mut self) -> Option<DocSave> {
        if self.saving {
            return None;
        }
        self.saving = true;
        Some(DocSave {
            source: self.source.clone(),
            document: self.view.document().clone(),
            revision: self.view.revision(),
        })
    }

    /// A save has finished. On success the tab remembers what it wrote: the
    /// document as it was when the save began is what the file holds, and
    /// the tab is modified exactly when its document differs from that (so
    /// undoing back to it clears the mark). Nothing else changes: the undo
    /// history, the caret and the scroll stay, and saving goes on starting
    /// from the bytes the tab was opened from, which the blocks' sources
    /// refer to. A document edited while the file was being written stays
    /// modified.
    pub fn finish_save(&mut self, job: &DocSave, path: &Path, written: Option<Written>) {
        self.saving = false;
        let Some(written) = written else {
            return;
        };
        self.disk = Some(aster_storage::Fingerprint::of(path, &written.bytes));
        self.path = Some(path.to_owned());
        self.view.mark_saved_as(job.document.clone(), job.revision);
    }

    /// The file as saving would write it now, made later on the record
    /// writer's thread.
    pub fn snapshot(&self) -> impl FnOnce() -> Option<Vec<u8>> + Send + 'static {
        let source = self.source.clone();
        let document = self.view.document().clone();
        let cost = self.record_cost.clone();
        move || {
            let started = Instant::now();
            let made = aster_ooxml::docx_save::save((*source).clone(), &document).ok();
            let took = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            cost.store(took, Ordering::Relaxed);
            made
        }
    }

    /// The least time between two writes of the record: five times what the
    /// last one took, and never under two seconds.
    pub fn record_every(&self) -> Duration {
        let cost = self.record_cost.load(Ordering::Relaxed);
        Duration::from_millis(cost.saturating_mul(5)).max(Duration::from_secs(2))
    }

    /// Change the document for the assistant or an MCP client: `edit` works
    /// on a copy of the document and says where the selection goes (`None`
    /// when it changed nothing and the document stays as it is). It is one
    /// step of undo, like a user's edit, so the modified mark, the recovery
    /// record and the toolbar follow in the same way (the caller then calls
    /// the shell's after-edit). Returns whether the document changed. Refused
    /// while an input method is composing in the tab.
    pub fn edit_document(
        &mut self,
        edit: impl FnOnce(&mut Document, &Selection) -> Option<Selection>,
    ) -> Result<bool, &'static str> {
        if self.view.is_composing() {
            return Err(crate::doc_tools::COMPOSING);
        }
        Ok(self.view.edit_document(edit))
    }

    /// The document came back from a recovery record: it has changes the
    /// file does not.
    pub fn mark_recovered(&mut self) {
        self.view.mark_unsaved();
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
        self.pointer_down_with(x, y, clicks, shift, false)
    }

    /// Like [`DocTab::pointer_down`]; with `primary` (Command, Control
    /// elsewhere) held, a click on a link opens it when the button goes up.
    /// Without it a click on a link only places the caret.
    pub fn pointer_down_with(
        &mut self,
        x: f64,
        y: f64,
        clicks: u8,
        shift: bool,
        primary: bool,
    ) -> bool {
        self.link_refused = false;
        let link = if primary && !shift && clicks == 1 && !self.view.is_composing() {
            self.view.link_at(x, y)
        } else {
            None
        };
        self.press = Some(Press {
            x,
            y,
            clicks,
            dragged: false,
            link,
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

    /// The button went up. If it was a click on a link with Command (or
    /// Control) held, the link's target.
    pub fn pointer_up(&mut self) -> Option<String> {
        self.view.release();
        let press = self.press.take()?;
        if press.dragged || press.clicks != 1 || self.view.has_selection() {
            return None;
        }
        press.link
    }

    // --- The keys ---

    /// A key as an action: movements, select all and editing. Returns
    /// whether the key was used. `word` and `line` say Option or Command was
    /// held, which makes Backspace and Delete go further.
    pub fn key(&mut self, action: &Action) -> bool {
        self.key_with(action, false, false)
    }

    pub fn key_with(&mut self, action: &Action, word: bool, line: bool) -> bool {
        if self.view.is_composing() {
            // Keys wait for the composition to end.
            return true;
        }
        let unit = if line {
            Unit::Line
        } else if word {
            Unit::Word
        } else {
            Unit::Cluster
        };
        match action {
            Action::SelectAll => {
                self.view.select_all();
                true
            }
            Action::Insert(text) => self.view.type_text(text) || true,
            Action::Newline => self.view.enter(),
            Action::PlainNewline => self.view.insert_break(Break::Line),
            Action::Indent => self.view.tab(false) || true,
            Action::Outdent => self.view.tab(true) || true,
            Action::Backspace => self.view.backspace(unit) || true,
            Action::Delete => self.view.delete_forward(unit) || true,
            _ => {
                let Some((movement, select)) = movement_of(action) else {
                    return false;
                };
                self.view.move_caret(movement, select);
                true
            }
        }
    }

    // --- The toolbar ---

    /// How the text at the caret looks, for the toolbar.
    pub fn look(&self) -> DocLook {
        let state = self.view.format_state();
        look_of(&state, &self.view)
    }

    /// A toolbar button. Returns whether the document or the view changed.
    pub fn run(&mut self, command: Command) -> bool {
        if self.view.is_composing() {
            return false;
        }
        let view = &mut self.view;
        let doc = match command {
            Command::Undo => DocCommand::Undo,
            Command::Redo => DocCommand::Redo,
            Command::Bold => DocCommand::Bold,
            Command::Italic => DocCommand::Italic,
            Command::Underline => DocCommand::Underline,
            Command::Strikethrough => DocCommand::Strikethrough,
            Command::FontSizeUp => DocCommand::SizeStep(1),
            Command::FontSizeDown => DocCommand::SizeStep(-1),
            Command::AlignLeft => DocCommand::Align(Align::Left),
            Command::AlignCenter => DocCommand::Align(Align::Center),
            Command::AlignRight => DocCommand::Align(Align::Right),
            Command::AlignJustify => DocCommand::Align(Align::Justify),
            Command::IndentMore => DocCommand::Indent(1),
            Command::IndentLess => DocCommand::Indent(-1),
            Command::BulletList => DocCommand::Bullets,
            Command::NumberedList => DocCommand::Numbering,
            _ => return false,
        };
        view.command(doc)
    }

    /// An entry chosen from a toolbar list: a paragraph style, a font, a
    /// size, a line spacing, or a colour (index 0 for automatic or none).
    pub fn option(&mut self, command: Command, index: usize) -> bool {
        if self.view.is_composing() {
            return false;
        }
        let doc = match command {
            Command::ParagraphStyle => {
                let styles = self.view.paragraph_styles();
                match styles.get(index) {
                    Some((id, _)) => DocCommand::Style(id.clone()),
                    None => return false,
                }
            }
            Command::FontFamily => match toolbar::FONT_FAMILIES.get(index) {
                Some(name) => DocCommand::Font(Some((*name).to_owned())),
                None => return false,
            },
            Command::FontSize => match toolbar::FONT_SIZES.get(index) {
                Some(size) => DocCommand::Size(*size),
                None => return false,
            },
            Command::LineSpacing => match toolbar::LINE_SPACINGS.get(index) {
                Some(multiple) => DocCommand::LineSpacing(LineSpacing::Multiple(*multiple)),
                None => return false,
            },
            Command::TextColor => match toolbar::palette_color(index) {
                Some(color) => DocCommand::Color(color),
                None => return false,
            },
            Command::Highlight => match toolbar::palette_color(index) {
                Some(color) => DocCommand::Highlight(color),
                None => return false,
            },
            _ => return false,
        };
        self.view.command(doc)
    }

    /// The style names the paragraph style list offers, in order, in the
    /// interface language where the style is a built-in one.
    pub fn style_names(&self, strings: &Strings) -> Vec<String> {
        self.view
            .paragraph_styles()
            .into_iter()
            .map(|(_, name)| shown_style_name(strings, name))
            .collect()
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

/// Words and characters of the body: objects (pictures, note references)
/// are not counted.
pub fn count(document: &Document) -> TextStats {
    TextStats::of(&document.plain_text().replace(OBJECT, ""))
}

/// The name of a style as the interface shows it: a built-in style in the
/// interface language, any other as the file gives it.
pub fn shown_style_name(strings: &Strings, name: String) -> String {
    strings.style_name(&name).unwrap_or(name)
}

fn look_of(state: &FormatState, view: &DocView) -> DocLook {
    let _ = view;
    DocLook {
        bold: state.bold == Tri::On,
        italic: state.italic == Tri::On,
        underline: state.underline == Tri::On,
        strike: state.strike == Tri::On,
        font: state.font.clone().unwrap_or_default(),
        size: state.size.unwrap_or(0.0),
        style: state
            .style
            .as_deref()
            .and_then(|id| view.style_name(id))
            .or_else(|| {
                state
                    .style
                    .as_deref()
                    .filter(|id| id.is_empty())
                    .and_then(|_| view.style_name(&view.default_style_id()))
            })
            .unwrap_or_default(),
        align: match state.align {
            Some(Align::Left) => 1,
            Some(Align::Center) => 2,
            Some(Align::Right) => 3,
            Some(Align::Justify | Align::Distribute) => 4,
            None => 0,
        },
        bullets: state.bullets == Tri::On,
        numbering: state.numbering == Tri::On,
        color: state.color.flatten(),
        highlight: state.highlight.flatten(),
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
        object: c.accent.to_array(),
    }
}

#[cfg(test)]
#[path = "../../aster-ooxml/tests/common/docx_sample.rs"]
pub(crate) mod docx_sample;

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
        let mut tab = DocTab::new(*view, source, Some(path), &Tokens::of(ThemeMode::Light));
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
    fn keys_move_the_caret_and_edit() {
        let dir = tempfile::tempdir().unwrap();
        let mut tab = tab(dir.path());
        assert!(!tab.is_modified());
        assert!(tab.key(&Action::Right {
            select: true,
            word: false
        }));
        assert!(tab.view.has_selection());
        assert!(tab.key(&Action::TextEnd { select: false }));
        assert!(tab.key(&Action::PageUp { select: false }));
        assert!(tab.key(&Action::SelectAll));
        assert!(tab.view.selected_text().len() > 200);
        assert!(!tab.is_modified());
        tab.key(&Action::TextStart { select: false });
        let before = tab.view.document().plain_text();
        tab.key(&Action::Insert("x".into()));
        tab.key(&Action::Newline);
        tab.key(&Action::Insert("y".into()));
        assert!(tab.is_modified());
        assert!(tab.view.document().plain_text().starts_with("x\ny"));
        // Backspace by cluster, by word (Option) and to the line start
        // (Command).
        tab.key_with(&Action::Backspace, false, false);
        tab.key_with(&Action::Backspace, true, false);
        tab.key_with(&Action::Backspace, false, true);
        while tab.view.can_undo() {
            tab.run(Command::Undo);
        }
        assert_eq!(tab.view.document().plain_text(), before);
        assert!(tab.run(Command::Redo));
    }

    #[test]
    fn saving_writes_the_file_and_the_tab_goes_on_from_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut tab = tab(dir.path());
        let path = dir.path().join("saved.docx");
        tab.key(&Action::Insert("Saved: ".into()));
        tab.run(Command::Bold);
        let caret_before = tab.view.caret().cloned();
        let job = tab.begin_save().expect("not saving");
        assert!(tab.begin_save().is_none(), "one save at a time");
        let written = job.write(&path).expect("it writes");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes, written.bytes);
        tab.finish_save(&job, &path, Some(written));
        assert!(!tab.is_modified());
        assert_eq!(tab.path.as_deref(), Some(path.as_path()));
        // Saving goes on from the bytes the tab was opened from, which the
        // document's blocks refer to; the undo history stays.
        assert_eq!(*tab.source, sample_bytes());
        assert!(tab.view.can_undo());
        assert_eq!(tab.view.caret().cloned(), caret_before);
        // The file reads back as what the tab holds, and saving again from
        // the kept bytes works.
        let mut read = aster_ooxml::docx::read(bytes).unwrap();
        read.renumber();
        assert!(aster_writer::edit::same_content(tab.view.document(), &read));
        tab.key(&Action::Insert("more ".into()));
        assert!(tab.is_modified());
        let job = tab.begin_save().unwrap();
        let written = job.write(&path).expect("it writes again");
        tab.finish_save(&job, &path, Some(written));
        assert!(!tab.is_modified());
        // Edited while the write was running: the tab stays modified and
        // keeps the bytes its model matches.
        tab.key(&Action::Insert("a".into()));
        let job = tab.begin_save().unwrap();
        let written = job.write(&path).expect("it writes");
        let kept = tab.source.clone();
        tab.key(&Action::Insert("b".into()));
        tab.finish_save(&job, &path, Some(written));
        assert!(tab.is_modified());
        assert_eq!(tab.source, kept);
        // A failed save changes nothing.
        let job = tab.begin_save().unwrap();
        tab.finish_save(&job, &path, None);
        assert!(!tab.saving && tab.is_modified());
    }

    #[test]
    fn a_new_document_is_blank_untitled_and_saves() {
        let mut tab = DocTab::blank(&Tokens::of(ThemeMode::Light)).expect("the template reads");
        assert!(tab.path.is_none() && !tab.is_modified());
        tab.view.resize(600, 500, 1.0);
        tab.key(&Action::Insert("你好 world".into()));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.docx");
        let job = tab.begin_save().unwrap();
        let written = job.write(&path).unwrap();
        tab.finish_save(&job, &path, Some(written));
        let again = aster_ooxml::docx::read(std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(again.plain_text(), "你好 world");
        // The look the toolbar shows.
        tab.run(Command::Bold);
        assert!(tab.look().bold);
        tab.run(Command::AlignCenter);
        assert_eq!(tab.look().align, 2);
        assert!(!tab.style_names(i18n::Language::En.strings()).is_empty());
    }

    #[test]
    fn style_names_are_shown_in_the_interface_language_and_picking_still_works() {
        let dir = tempfile::tempdir().unwrap();
        let mut tab = tab(dir.path());
        let raw: Vec<String> = tab
            .view
            .paragraph_styles()
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        let zh = i18n::Language::ZhHans.strings();
        let shown = tab.style_names(zh);
        // Same entries in the same order; built-in names are translated.
        assert_eq!(shown.len(), raw.len());
        let mut translated = 0;
        for (file, name) in raw.iter().zip(&shown) {
            match zh.style_name(file) {
                Some(expected) => {
                    assert_eq!(name, &expected);
                    translated += 1;
                }
                None => assert_eq!(name, file),
            }
        }
        assert!(translated > 0, "{raw:?}");
        let index = raw
            .iter()
            .position(|name| name.eq_ignore_ascii_case("heading 1"))
            .expect("the sample has a first-level heading style");
        assert_eq!(shown[index], "标题 1");
        // Choosing the entry by its index applies that style, and the style
        // box then shows the same text as the list entry.
        assert!(tab.option(Command::ParagraphStyle, index));
        let look = tab.look();
        assert!(
            look.style.eq_ignore_ascii_case("heading 1"),
            "{}",
            look.style
        );
        assert_eq!(shown_style_name(zh, look.style), shown[index]);
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
        assert!(!tab.press.as_ref().expect("pressed").dragged);
        tab.pointer_drag(340.0, 201.0);
        assert!(tab.press.as_ref().expect("pressed").dragged);
        assert_eq!(tab.pointer_up(), None);
        assert!(tab.press.is_none());
    }

    /// A 1 by 1 pixel PNG file.
    const DOT: [u8; 70] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64,
        0x60, 0xf8, 0x5f, 0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn picture_files_are_measured_from_their_headers_and_checked() {
        assert_eq!(read_picture(&DOT), Ok((1, 1)));
        // GIF: little-endian width and height after the signature.
        let mut gif = b"GIF89a".to_vec();
        gif.extend([3, 0, 2, 0, 0, 0, 0]);
        assert_eq!(image_size(&gif), Some((3, 2)));
        // Measured, but not inserted: nothing here draws a GIF.
        assert_eq!(read_picture(&gif), Err(PictureProblem::NotAPicture));
        // BMP: a 40-byte header; a negative height is a top-down picture.
        let mut bmp = b"BM".to_vec();
        bmp.resize(14, 0);
        bmp.extend(40u32.to_le_bytes());
        bmp.extend(5i32.to_le_bytes());
        bmp.extend((-4i32).to_le_bytes());
        bmp.resize(54, 0);
        assert_eq!(image_size(&bmp), Some((5, 4)));
        assert_eq!(read_picture(&bmp), Err(PictureProblem::NotAPicture));
        // TIFF, both byte orders: the first directory's width and height.
        for big in [false, true] {
            let u16b = |v: u16| {
                if big {
                    v.to_be_bytes()
                } else {
                    v.to_le_bytes()
                }
            };
            let u32b = |v: u32| {
                if big {
                    v.to_be_bytes()
                } else {
                    v.to_le_bytes()
                }
            };
            let mut tiff = if big {
                b"MM\0*".to_vec()
            } else {
                b"II*\0".to_vec()
            };
            tiff.extend(u32b(8));
            tiff.extend(u16b(2));
            for (tag, value) in [(256u16, 7u16), (257, 9)] {
                tiff.extend(u16b(tag));
                tiff.extend(u16b(3));
                tiff.extend(u32b(1));
                tiff.extend(u16b(value));
                tiff.extend([0, 0]);
            }
            tiff.extend(u32b(0));
            assert_eq!(image_size(&tiff), Some((7, 9)), "big endian: {big}");
            assert_eq!(read_picture(&tiff), Err(PictureProblem::NotAPicture));
        }
        // JPEG: the start of frame after another segment.
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00,
            0x10, 0x00, 0x20, 0x01, 0x01, 0x11, 0x00, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(image_size(&jpeg), Some((32, 16)));
        // Not pictures, and ones that say they have no size or too many.
        assert_eq!(read_picture(b"hello"), Err(PictureProblem::NotAPicture));
        assert_eq!(read_picture(b""), Err(PictureProblem::NotAPicture));
        assert_eq!(read_picture(&DOT[..20]), Err(PictureProblem::NotAPicture));
        let mut broken = DOT.to_vec();
        broken[40] ^= 0xFF;
        assert!(read_picture(&broken).is_err() || read_picture(&broken) == Ok((1, 1)));
        let mut huge = DOT[..24].to_vec();
        huge[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        huge[20..24].copy_from_slice(&20_000u32.to_be_bytes());
        assert_eq!(read_picture(&huge), Err(PictureProblem::TooLarge));
        let mut flat = DOT[..24].to_vec();
        flat[16..20].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(read_picture(&flat), Err(PictureProblem::NotAPicture));
    }

    #[test]
    fn a_link_opens_only_on_a_click_with_the_command_key_held() {
        let mut tab = DocTab::blank(&Tokens::of(ThemeMode::Light)).expect("the template reads");
        tab.view.resize(900, 700, 1.0);
        tab.key(&Action::Insert("go to the site now".into()));
        let start = tab.view.document().start().unwrap();
        let at = |offset: usize| aster_writer::Pos::new(start.path.clone(), offset);
        tab.view.set_selection(at(6), at(14));
        assert!(
            tab.view
                .command(DocCommand::SetLink(Some("https://example.com/".into())))
        );
        tab.view.go_to(&at(9));
        let (x, y, _, h) = tab.view.caret_rect().unwrap();
        let (x, y) = (x + 1.0, y + h / 2.0);
        // A plain click only places the caret.
        tab.pointer_down(x, y, 1, false);
        assert_eq!(tab.pointer_up(), None);
        assert!(tab.view.caret().is_some());
        // With the command key held, letting go opens it.
        tab.pointer_down_with(x, y, 1, false, true);
        assert_eq!(tab.pointer_up().as_deref(), Some("https://example.com/"));
        // Not when the pointer was dragged, on a double click, or off it.
        tab.pointer_down_with(x, y, 1, false, true);
        tab.pointer_drag(x + 30.0, y);
        assert_eq!(tab.pointer_up(), None);
        tab.pointer_down_with(x, y, 2, false, true);
        assert_eq!(tab.pointer_up(), None);
        tab.view.go_to(&at(1));
        let (x, y, _, h) = tab.view.caret_rect().unwrap();
        tab.pointer_down_with(x + 1.0, y + h / 2.0, 1, false, true);
        assert_eq!(tab.pointer_up(), None);
        // Saving keeps the link in the file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("link.docx");
        let job = tab.begin_save().unwrap();
        let written = job.write(&path).expect("it writes");
        tab.finish_save(&job, &path, Some(written));
        let read = aster_ooxml::docx::read(std::fs::read(&path).unwrap()).unwrap();
        assert!(
            read.paragraphs()
                .iter()
                .any(|(_, p)| p.runs.iter().any(|r| r
                    .link
                    .as_ref()
                    .is_some_and(|l| l.target == "https://example.com/")))
        );
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
