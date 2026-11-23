//! What a tab holds: the view of a Writer tab, or the editor of a Markdown tab.
//!
//! It joins the document model from
//! `aster-markdown` to the text surface from `aster-surface`. Neither of those
//! knows about the other.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::stats::TextStats;
use crate::theme;
use crate::toolbar::ViewMode;
use aster_markdown::document::{floor_char_boundary, slice};
use aster_markdown::format;
use aster_markdown::highlight::{Highlighter, Marks, Span};
use aster_markdown::search::{Finder, Found, Query};
use aster_markdown::{Change, Document, EditCommand, Rope, SavePoint, Selection, TextFormat};
use aster_storage::{Fingerprint, RecordKey};
use aster_surface::preview::{PreviewJob, PreviewLayout, PreviewStyle, PreviewView};
use aster_surface::text_view::line_lengths;
use aster_surface::{
    Action, Decoration, ImeEvent, PageLayout, Palette, Rgba8, TextStyle, TextSurface, TextView,
};

/// Body text size of the Markdown editor, in logical pixels.
const MARKDOWN_FONT_SIZE: f32 = 15.0;

pub fn palette(tokens: &theme::Tokens) -> Palette {
    let c = &tokens.colors;
    Palette {
        desk: c.desk.to_array(),
        page: c.page.to_array(),
        page_edge: c.page_edge.to_array(),
        ink: c.ink.to_array(),
        selection: c.selection.to_array(),
        caret: c.caret.to_array(),
    }
}

/// The sheet of paper a Writer tab shows.
pub fn page_layout(tokens: &theme::Tokens) -> PageLayout {
    let page = &tokens.page;
    PageLayout {
        margin: f64::from(page.desk_margin),
        padding: f64::from(page.padding),
        max_width: Some(f64::from(page.width)),
        min_aspect: Some(f64::from(page.aspect)),
    }
}

/// A Markdown tab has no sheet of paper: the text sits on the window surface,
/// in a column that stops growing where lines would get too long to read.
fn editor_palette(tokens: &theme::Tokens) -> Palette {
    let c = &tokens.colors;
    Palette {
        desk: c.surface.to_array(),
        page: c.surface.to_array(),
        page_edge: [0, 0, 0, 0],
        ..palette(tokens)
    }
}

fn editor_layout(tokens: &theme::Tokens) -> PageLayout {
    let m = &tokens.metrics;
    PageLayout {
        margin: 0.0,
        padding: f64::from(m.space_xl) * 2.0,
        max_width: Some(f64::from(tokens.page.width) + 4.0 * f64::from(m.space_xl)),
        min_aspect: None,
    }
}

/// The colours of highlighted Markdown source.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SyntaxColors {
    /// Punctuation that is syntax: `#`, `*`, fences, list markers, pipes.
    marker: Rgba8,
    /// Quotes and struck-out text.
    secondary: Rgba8,
    link: Rgba8,
    /// Behind code.
    fill: Rgba8,
    /// Behind every match of a search, and behind the current one.
    found: Rgba8,
    current: Rgba8,
    /// Text of the current match.
    on_current: Rgba8,
}

impl SyntaxColors {
    fn of(tokens: &theme::Tokens) -> Self {
        let c = &tokens.colors;
        Self {
            marker: c.text_tertiary.to_array(),
            secondary: c.text_secondary.to_array(),
            link: c.link.to_array(),
            fill: c.fill.to_array(),
            found: c.accent_soft.to_array(),
            current: c.accent.to_array(),
            on_current: c.on_accent.to_array(),
        }
    }
}

/// How text with these marks is drawn in the editor. Only weight, slant,
/// colour and backgrounds change: never the size or the font, so the text
/// does not move while it is typed.
fn text_style(marks: Marks, colors: &SyntaxColors) -> TextStyle {
    let mut style = TextStyle {
        bold: marks.intersects(Marks::HEADING | Marks::STRONG),
        italic: marks.contains(Marks::EMPHASIS),
        ..TextStyle::default()
    };
    if marks.intersects(Marks::CODE | Marks::MATH) {
        style.background = Some(colors.fill);
    }
    if marks.contains(Marks::CODE_BLOCK) {
        style.line_background = Some(colors.fill);
    }
    if marks.intersects(Marks::QUOTE | Marks::STRIKE) {
        style.color = Some(colors.secondary);
    }
    if marks.contains(Marks::STRIKE) && !marks.is_syntax() {
        style.strike = true;
    }
    if marks.contains(Marks::LINK) {
        style.color = Some(colors.link);
    }
    if marks.is_syntax() || marks.intersects(Marks::URL | Marks::HTML) {
        style.color = Some(colors.marker);
    }
    style
}

/// Decorations for highlighted spans, joined where neighbours look the same.
fn decorations(spans: Vec<Span>, colors: &SyntaxColors) -> Vec<Decoration> {
    let mut out: Vec<Decoration> = Vec::with_capacity(spans.len());
    for span in spans {
        let style = text_style(span.marks, colors);
        if style == TextStyle::default() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.range.end == span.range.start && last.style == style => {
                last.range.end = span.range.end;
            }
            _ => out.push(Decoration {
                range: span.range,
                style,
            }),
        }
    }
    out
}

/// The style at an offset of a list of decorations in order.
fn decoration_at(decorations: &[Decoration], offset: usize) -> TextStyle {
    let index = decorations.partition_point(|d| d.range.end <= offset);
    match decorations.get(index) {
        Some(d) if d.range.start <= offset => d.style,
        _ => TextStyle::default(),
    }
}

/// Lay search highlights over the syntax decorations: a background behind
/// every match, and the accent with its own text colour behind the current
/// one. `matches` are in order; `current` is the range of the current match.
fn overlay(
    base: Vec<Decoration>,
    matches: &[Range<usize>],
    current: Option<Range<usize>>,
    colors: &SyntaxColors,
) -> Vec<Decoration> {
    if matches.is_empty() {
        return base;
    }
    let mut points: Vec<usize> = Vec::with_capacity(2 * (base.len() + matches.len()));
    for d in &base {
        points.extend([d.range.start, d.range.end]);
    }
    for m in matches {
        points.extend([m.start, m.end]);
    }
    points.sort_unstable();
    points.dedup();
    let mut out: Vec<Decoration> = Vec::with_capacity(points.len());
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let mut style = decoration_at(&base, a);
        let index = matches.partition_point(|m| m.end <= a);
        if let Some(m) = matches.get(index)
            && m.start <= a
        {
            if current.as_ref() == Some(m) {
                style.background = Some(colors.current);
                style.color = Some(colors.on_current);
            } else {
                style.background = Some(colors.found);
            }
        }
        if style == TextStyle::default() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.range.end == a && last.style == style => last.range.end = b,
            _ => out.push(Decoration { range: a..b, style }),
        }
    }
    out
}

/// Documents up to this size are searched right away on the interface
/// thread; larger ones on another thread, which can be cancelled.
pub const SEARCH_INLINE: usize = 2 * 1024 * 1024;
/// An edit larger than this (a replacement throughout a large document, or
/// its undo) is not highlighted incrementally: the highlighting is rebuilt
/// on another thread. Nor is it written to the recovery record as an entry:
/// the record is checkpointed instead.
pub const LARGE_CHANGE: usize = 4 * 1024 * 1024;

/// A formatting command for [`MarkdownEditor::format`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Inline(format::Inline),
    /// 0 for body text, else 1 to 6.
    Heading(u8),
    Block(format::Block),
    Link,
    Image,
    Table,
}

/// The find bar of a tab and what it found.
#[derive(Default)]
pub struct FindState {
    pub open: bool,
    /// The replace field is shown.
    pub replace_open: bool,
    pub query: Query,
    pub replacement: String,
    pub found: Found,
    /// Index of the current match in `found`.
    pub current: Option<usize>,
    /// A search is running on another thread.
    pub searching: bool,
    /// The text changed since the search: the matches were moved along with
    /// it, and a new search is due.
    pub stale: bool,
    /// Why the query is not a valid regular expression.
    pub error: Option<String>,
    /// Replacing every match is running on another thread. The document
    /// takes no edits until it is done.
    pub replacing: bool,
    /// How many places the last "replace all" changed.
    pub replaced: Option<usize>,
    /// The running search should move the caret to its first match.
    follow: bool,
    finder: Option<Finder>,
    generation: u64,
    cancel: Option<Arc<AtomicBool>>,
}

impl FindState {
    fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn current_range(&self) -> Option<Range<usize>> {
        self.current
            .and_then(|index| self.found.matches.get(index))
            .cloned()
    }
}

/// A search to run on another thread.
pub struct SearchJob {
    pub generation: u64,
    finder: Finder,
    text: Rope,
    cancel: Arc<AtomicBool>,
}

impl SearchJob {
    /// Search. `None` if cancelled.
    pub fn run(self) -> (u64, Option<Found>) {
        let text = self.text.to_string();
        drop(self.text);
        let found = self.finder.find_all(&text, &self.cancel, |_| {});
        (self.generation, found)
    }
}

/// Replacing every match, to run on another thread.
pub struct ReplaceJob {
    generation: u64,
    finder: Finder,
    text: Rope,
    replacement: String,
    cancel: Arc<AtomicBool>,
}

/// What [`ReplaceJob::run`] worked out: the edit, and the whole text after
/// it, so that the interface thread only has to take it.
pub struct Replaced {
    pub generation: u64,
    range: Range<usize>,
    inserted: String,
    removed: String,
    text: Rope,
    pub count: usize,
}

impl ReplaceJob {
    /// `None` if cancelled; a count of 0 if nothing matched.
    pub fn run(self) -> Option<Replaced> {
        let string = self.text.to_string();
        let found = self.finder.find_all(&string, &self.cancel, |_| {})?;
        let Some((range, inserted)) =
            self.finder
                .replace_all(&string, &found.matches, &self.replacement, &self.cancel)
        else {
            return (!self.cancel.load(Ordering::Relaxed)).then(|| Replaced {
                generation: self.generation,
                range: 0..0,
                inserted: String::new(),
                removed: String::new(),
                text: self.text.clone(),
                count: 0,
            });
        };
        let removed = string[range.clone()].to_owned();
        drop(string);
        let mut text = self.text;
        aster_markdown::document::replace(&mut text, range.clone(), &inserted);
        Some(Replaced {
            generation: self.generation,
            range,
            inserted,
            removed,
            text,
            count: found.matches.len(),
        })
    }
}

/// The preview's colours and column, matching the editor's; callout titles
/// in the interface language.
pub fn preview_style(
    tokens: &theme::Tokens,
    strings: &'static crate::i18n::Strings,
) -> PreviewStyle {
    let c = &tokens.colors;
    PreviewStyle {
        background: c.surface.to_array(),
        text: c.ink.to_array(),
        secondary: c.text_secondary.to_array(),
        tertiary: c.text_tertiary.to_array(),
        accent: c.link.to_array(),
        fill: c.fill.to_array(),
        line: c.page_edge.to_array(),
        selection: c.selection.to_array(),
        callouts: theme::callout_colors(tokens.mode).map(|color| color.to_array()),
        callout_labels: [
            strings.callout_note,
            strings.callout_tip,
            strings.callout_important,
            strings.callout_warning,
            strings.callout_caution,
        ],
        font_size: MARKDOWN_FONT_SIZE,
        page: editor_layout(tokens),
    }
}

/// What a preview was last asked for with.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PreviewRequest {
    version: u64,
    width: u16,
    scale: f32,
    style: PreviewStyle,
}

/// A Writer tab: the plain-text surface on its own, with no document model, no
/// undo and no file behind it.
pub struct WriterView {
    pub surface: TextSurface,
    pub zoom_percent: u32,
    pub stats: TextStats,
}

impl WriterView {
    pub fn new(text: &str, tokens: &theme::Tokens) -> Self {
        let mut surface = TextSurface::new(text);
        surface.set_palette(palette(tokens));
        surface.set_page_layout(page_layout(tokens));
        Self {
            stats: TextStats::of(text),
            surface,
            zoom_percent: 100,
        }
    }

    pub fn restyle(&mut self, tokens: &theme::Tokens) {
        self.surface.set_palette(palette(tokens));
        self.surface.set_page_layout(page_layout(tokens));
    }
}

/// A Markdown tab. The document owns the text, the selection and the undo
/// history; the surface shows them, and answers what only layout can: where
/// the caret goes when it moves, what is under the pointer.
///
/// Every edit goes to the document first and is then repeated on the surface,
/// so the two never disagree about the text.
pub struct MarkdownEditor {
    pub document: Document,
    pub surface: TextView,
    /// The file the document is saved in. `None` until it has been saved once.
    pub path: Option<PathBuf>,
    /// Line endings and byte-order mark to write the file with.
    pub format: TextFormat,
    pub stats: TextStats,
    /// The counts could not be updated locally after an edit; the text has
    /// to be counted again, which the shell does off this thread for large
    /// documents.
    pub stats_stale: bool,
    /// A count of the whole text is running on another thread.
    pub counting: bool,
    /// A save is running in the background.
    pub saving: bool,
    /// What the file held when it was last read or written. Stored in the
    /// recovery record, so a restored document can tell whether another
    /// program changed the file in the meantime.
    pub disk: Option<Fingerprint>,
    /// Set on a restored document whose file changed or vanished while it was
    /// not open. Saving then asks where to, instead of overwriting the file.
    pub disk_warning: Option<DiskWarning>,
    /// The document's recovery record, while it has unsaved changes.
    pub record: Option<RecordKey>,
    /// Why the recovery record could not be written, if it could not.
    pub record_failed: Option<String>,
    /// Changes not yet handed to the recovery writer, encoded.
    unjournaled: Vec<Vec<u8>>,
    /// Entries and bytes written to the record since its last checkpoint.
    since_checkpoint: (u64, usize),
    highlighter: Highlighter,
    syntax: SyntaxColors,
    pub view_mode: ViewMode,
    pub preview: PreviewView,
    /// Goes up with every change to the text.
    text_version: u64,
    preview_request: Option<PreviewRequest>,
    /// The editor's scroll offset when the preview was last lined up with
    /// it, in split view.
    synced_scroll: Option<f64>,
    /// A source position the preview should scroll to once it has a layout.
    preview_target: Option<f64>,
    /// The last link clicked in the preview was not opened: it was not a web
    /// or e-mail link.
    pub link_refused: bool,
    /// The preview was clicked last, not the source: the keyboard scrolls
    /// it, and copying and "select all" act on it. Always so when the
    /// preview is on screen alone.
    preview_focused: bool,
    pub find: FindState,
    /// The highlighting is out of date after a large change and is being
    /// rebuilt elsewhere; the text shows plain until then.
    pub highlight_stale: bool,
    /// A rebuild of the highlighting is running on another thread.
    pub highlighting: bool,
    /// The next recovery step must checkpoint: a large change was not
    /// written as an entry.
    force_checkpoint: bool,
}

/// What became of a restored document's file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskWarning {
    /// Another program changed it, or it could not be read.
    Changed,
    Missing,
}

/// What the recovery record of a document needs next. See
/// [`MarkdownEditor::journal_step`]. Whole texts are ropes: cloning one
/// shares its pieces, so handing it to the writer thread copies nothing here.
#[derive(Debug, PartialEq)]
pub enum JournalStep {
    Nothing,
    /// Start a record holding this whole text.
    Open(Rope),
    /// Add these entries; then, if given, checkpoint this whole text.
    Append(Vec<Vec<u8>>, Option<Rope>),
    /// The document has nothing unsaved any more: delete the record.
    Discard,
}

/// A record is checkpointed after this many entries...
const CHECKPOINT_ENTRIES: u64 = 256;
/// ...or this many bytes of entries, whichever comes first. A checkpoint
/// writes the whole text, so it is not done on every key press; a replay
/// after a crash then has at most this much to apply.
const CHECKPOINT_BYTES: usize = 256 * 1024;
/// For large documents both limits grow with the text, so that the writer
/// thread spends a bounded share of its time on checkpoints: one per this
/// many bytes of text in entries, and one entry per this many bytes of text.
const CHECKPOINT_BYTES_PER_TEXT: usize = 16;
const CHECKPOINT_TEXT_PER_ENTRY: usize = 64 * 1024;
/// Documents up to this size are counted again in full when an edit cannot
/// be counted locally; larger ones are counted on another thread.
pub const RECOUNT_INLINE: usize = 1024 * 1024;
/// Larger documents get no preview: laying out all of it would take seconds
/// and hundreds of megabytes. The preview pane says so instead.
pub const PREVIEW_LIMIT: usize = 4 * 1024 * 1024;

/// The bytes of a rope, for the recovery writer.
pub fn rope_bytes(text: &Rope) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len_bytes());
    for chunk in text.chunks() {
        bytes.extend_from_slice(chunk.as_bytes());
    }
    bytes
}

/// Everything about a document that takes time in proportion to its size,
/// computed before the editor exists, on whatever thread read the file: the
/// rope, the line lengths the view needs, the highlighting and the counts.
pub struct Prepared {
    pub text: Rope,
    lens: Vec<u32>,
    highlighter: Highlighter,
    stats: TextStats,
}

/// A file read and prepared on a worker thread, ready for a tab.
pub struct Opened {
    pub prepared: Prepared,
    pub format: TextFormat,
    pub fingerprint: Fingerprint,
}

/// Prepare a file that was read. Slow for large files; call it off the
/// interface thread.
pub fn open(loaded: crate::files::Loaded) -> Opened {
    let crate::files::Loaded {
        decoded,
        fingerprint,
    } = loaded;
    let text = Rope::from_str(&decoded.text);
    drop(decoded.text);
    Opened {
        prepared: prepare(text),
        format: decoded.format,
        fingerprint,
    }
}

/// Prepare a document's text. Slow for large documents; call it off the
/// interface thread.
pub fn prepare(text: Rope) -> Prepared {
    Prepared {
        lens: line_lengths(&text),
        highlighter: Highlighter::new(&text),
        stats: TextStats::of_rope(&text),
        text,
    }
}

impl MarkdownEditor {
    /// A document as read from `path`, or a new one when `path` is `None`.
    pub fn new(
        text: &str,
        format: TextFormat,
        path: Option<PathBuf>,
        disk: Option<Fingerprint>,
        tokens: &theme::Tokens,
    ) -> Self {
        Self::from_prepared(prepare(Rope::from_str(text)), format, path, disk, tokens)
    }

    /// An editor for a document prepared with [`prepare`]. Quick at any size.
    pub fn from_prepared(
        prepared: Prepared,
        format: TextFormat,
        path: Option<PathBuf>,
        disk: Option<Fingerprint>,
        tokens: &theme::Tokens,
    ) -> Self {
        let Prepared {
            text,
            lens,
            highlighter,
            stats,
        } = prepared;
        let mut surface = TextView::with_line_lengths(text.clone(), lens);
        surface.set_font_size(MARKDOWN_FONT_SIZE);
        surface.set_palette(editor_palette(tokens));
        surface.set_page_layout(editor_layout(tokens));
        // A file opens at its beginning.
        surface.set_selection(0, 0);
        let mut preview = PreviewView::new();
        preview.set_background(tokens.colors.surface.to_array());
        preview.set_selection_color(tokens.colors.selection.to_array());
        Self {
            highlighter,
            syntax: SyntaxColors::of(tokens),
            view_mode: ViewMode::default(),
            preview,
            text_version: 0,
            preview_request: None,
            synced_scroll: None,
            preview_target: None,
            link_refused: false,
            preview_focused: false,
            find: FindState::default(),
            highlight_stale: false,
            highlighting: false,
            force_checkpoint: false,
            document: Document::from_rope(text),
            surface,
            path,
            format,
            stats,
            stats_stale: false,
            counting: false,
            saving: false,
            disk,
            disk_warning: None,
            record: None,
            record_failed: None,
            unjournaled: Vec::new(),
            since_checkpoint: (0, 0),
        }
    }

    /// A document restored from a recovery record. It has unsaved changes from
    /// the start: what was recovered has never been saved.
    pub fn recovered(
        prepared: Prepared,
        format: TextFormat,
        path: Option<PathBuf>,
        disk: Option<Fingerprint>,
        disk_warning: Option<DiskWarning>,
        tokens: &theme::Tokens,
    ) -> Self {
        let text = prepared.text.clone();
        Self {
            document: Document::unsaved(text),
            disk_warning,
            ..Self::from_prepared(prepared, format, path, disk, tokens)
        }
    }

    /// Decide what the recovery record needs after the edits since the last
    /// call, and forget those edits. The caller carries the step out; this
    /// keeps `record` in step with it.
    ///
    /// A record exists exactly while the document has unsaved changes. It
    /// starts with the whole text, so the edits that made the document
    /// modified are already in it; it is deleted when the document is saved
    /// or undone back to what is on disk.
    pub fn journal_step(&mut self, new_key: impl FnOnce() -> RecordKey) -> JournalStep {
        let entries = std::mem::take(&mut self.unjournaled);
        match (self.record, self.document.is_modified()) {
            (None, false) => JournalStep::Nothing,
            (None, true) => {
                self.force_checkpoint = false;
                self.record = Some(new_key());
                self.since_checkpoint = (0, 0);
                JournalStep::Open(self.document.text().clone())
            }
            (Some(_), false) => {
                self.record = None;
                self.record_failed = None;
                JournalStep::Discard
            }
            (Some(_), true) if entries.is_empty() && !self.force_checkpoint => JournalStep::Nothing,
            (Some(_), true) => {
                let (count, bytes) = &mut self.since_checkpoint;
                *count += entries.len() as u64;
                *bytes += entries.iter().map(Vec::len).sum::<usize>();
                let len = self.document.len();
                let entry_limit = CHECKPOINT_ENTRIES.max((len / CHECKPOINT_TEXT_PER_ENTRY) as u64);
                let byte_limit = CHECKPOINT_BYTES.max(len / CHECKPOINT_BYTES_PER_TEXT);
                let due = *count >= entry_limit || *bytes >= byte_limit;
                let checkpoint = (due || std::mem::take(&mut self.force_checkpoint))
                    .then(|| self.document.text().clone());
                if checkpoint.is_some() {
                    self.since_checkpoint = (0, 0);
                }
                JournalStep::Append(entries, checkpoint)
            }
        }
    }

    pub fn restyle(&mut self, tokens: &theme::Tokens) {
        self.surface.set_palette(editor_palette(tokens));
        self.surface.set_page_layout(editor_layout(tokens));
        self.syntax = SyntaxColors::of(tokens);
        self.preview
            .set_background(tokens.colors.surface.to_array());
        self.preview
            .set_selection_color(tokens.colors.selection.to_array());
    }

    /// Highlight what the editor shows. Called before every frame: only the
    /// visible lines are looked up, and the highlighter has already done the
    /// parsing when the text changed.
    pub fn decorate(&mut self) {
        if !self.view_mode.shows_source() {
            return;
        }
        let range = self.surface.visible_range();
        let spans = if self.highlight_stale {
            Vec::new()
        } else {
            self.highlighter.spans(range.clone())
        };
        let mut decorated = decorations(spans, &self.syntax);
        if self.find.open {
            let (_, visible) = self.find.found.within(range);
            decorated = overlay(decorated, visible, self.find.current_range(), &self.syntax);
        }
        self.surface.set_decorations(decorated);
    }

    /// Switch between source, split and preview. Both views keep showing the
    /// same part of the document.
    pub fn set_view_mode(&mut self, mode: ViewMode) {
        let old = self.view_mode;
        if mode == old {
            return;
        }
        let position = if old.shows_source() {
            self.surface.top_position()
        } else {
            self.preview.top_position()
        };
        self.view_mode = mode;
        self.synced_scroll = None;
        if mode.shows_source() && !old.shows_source() {
            self.surface.scroll_to_position(position);
        }
        if mode.shows_preview() && !old.shows_preview() {
            // The pane may show another tab's frame from before.
            self.preview.invalidate();
            if self.preview.layout().is_some() {
                self.preview.scroll_to_position(position);
            }
            self.preview_target = Some(position);
        }
    }

    /// A job for the preview worker, if the preview is on screen and what it
    /// shows is out of date. A change of size or colours always asks for a
    /// new layout; a change of the text only when `text_changes` is set,
    /// which the caller does once typing has paused.
    pub fn preview_job(
        &mut self,
        key: u64,
        text_changes: bool,
        style: PreviewStyle,
    ) -> Option<PreviewJob> {
        if !self.view_mode.shows_preview() || self.preview_too_large() {
            return None;
        }
        let (width, height, scale) = self.preview.size();
        if width == 0 || height == 0 {
            return None;
        }
        if let Some(last) = &self.preview_request
            && (last.width, last.scale, last.style) == (width, scale, style)
            && (last.version == self.text_version || !text_changes)
        {
            return None;
        }
        self.preview_request = Some(PreviewRequest {
            version: self.text_version,
            width,
            scale,
            style,
        });
        Some(PreviewJob {
            key,
            generation: self.text_version,
            source: self.document.text().to_string(),
            width,
            scale,
            style,
        })
    }

    /// The text changed since the preview was last asked for.
    pub fn preview_is_behind(&self) -> bool {
        self.view_mode.shows_preview()
            && self
                .preview_request
                .is_none_or(|last| last.version != self.text_version)
    }

    /// Show a layout from the worker, keeping the preview at the same place
    /// in the document.
    pub fn show_preview(&mut self, layout: PreviewLayout) {
        let position = match (self.preview_target.take(), self.view_mode) {
            (Some(target), _) => target,
            (None, ViewMode::Split) => self.surface.top_position(),
            (None, _) => self.preview.top_position(),
        };
        self.preview.set_layout(std::sync::Arc::new(layout));
        self.preview.scroll_to_position(position);
        self.synced_scroll = Some(self.surface.scroll_range().0);
    }

    /// In split view, bring the preview to where the editor is, if the
    /// editor has moved since they were last lined up.
    pub fn follow_editor(&mut self) {
        if self.view_mode != ViewMode::Split {
            return;
        }
        let scroll = self.surface.scroll_range().0;
        if self.synced_scroll == Some(scroll) {
            return;
        }
        self.preview.scroll_to_position(self.surface.top_position());
        self.synced_scroll = Some(scroll);
    }

    /// Whether the keyboard and the Edit menu act on the preview.
    pub fn preview_has_focus(&self) -> bool {
        match self.view_mode {
            ViewMode::Edit => false,
            ViewMode::Preview => true,
            ViewMode::Split => self.preview_focused,
        }
    }

    /// A press in the preview, in physical pixels relative to it: starts a
    /// selection there, or with `extend` moves the end of the current one.
    pub fn preview_press(&mut self, x: f64, y: f64, extend: bool) {
        self.link_refused = false;
        self.preview_focused = true;
        self.preview.press(x, y, extend);
    }

    pub fn preview_drag(&mut self, x: f64, y: f64) {
        self.preview.drag(x, y);
    }

    /// Scroll the preview to an offset in physical pixels; in split view the
    /// editor follows.
    pub fn scroll_preview_to(&mut self, offset: f64) -> bool {
        let (current, _) = self.preview.scroll_range();
        self.scroll_preview(offset - current)
    }

    /// Follow a `#` link in the preview, such as a footnote.
    pub fn follow_preview_link(&mut self, link: &str) -> bool {
        if !self.preview.follow(link) {
            return false;
        }
        if self.view_mode == ViewMode::Split {
            self.surface.scroll_to_position(self.preview.top_position());
            self.synced_scroll = Some(self.surface.scroll_range().0);
        }
        true
    }

    /// Scroll the preview by physical pixels; in split view the editor
    /// follows. Returns whether anything moved.
    pub fn scroll_preview(&mut self, delta: f64) -> bool {
        if !self.preview.scroll_by(delta) {
            return false;
        }
        if self.view_mode == ViewMode::Split {
            self.surface.scroll_to_position(self.preview.top_position());
            self.synced_scroll = Some(self.surface.scroll_range().0);
        }
        true
    }

    /// Repeat on the surface an edit the document has made.
    fn show(&mut self, change: Option<Change>) -> bool {
        let Some(change) = change else {
            return false;
        };
        let large = change.inserted.len() + change.removed.len() > LARGE_CHANGE;
        if large {
            self.highlight_stale = true;
            self.force_checkpoint = true;
        } else {
            if !self.highlight_stale {
                self.highlighter.edit(
                    self.document.text(),
                    change.range.clone(),
                    change.inserted.len(),
                );
            }
            self.unjournaled
                .push(aster_markdown::journal::encode(&change));
        }
        self.text_version += 1;
        self.link_refused = false;
        // "Replaced 12" describes the text before any later edit or undo.
        self.find.replaced = None;
        self.surface.apply_change(
            change.range.clone(),
            change.inserted.len(),
            self.document.text().clone(),
        );
        if self.find.finder.is_some() {
            self.find
                .found
                .edited(change.range.clone(), change.inserted.len());
            self.find.current = None;
            self.find.stale = true;
        }
        self.surface
            .set_selection(change.selection.anchor, change.selection.focus);
        self.surface.reveal_caret();
        if !self.stats_stale
            && !self.stats.edited(
                self.document.text(),
                change.range.start,
                &change.removed,
                &change.inserted,
            )
        {
            if self.document.len() <= RECOUNT_INLINE {
                self.stats = TextStats::of_rope(self.document.text());
            } else {
                self.stats_stale = true;
            }
        }
        true
    }

    /// The document takes no edits: a replacement throughout it is running.
    pub fn locked(&self) -> bool {
        self.find.replacing
    }

    /// A rebuild of the highlighting to run elsewhere, if one is due and not
    /// running: the text version and the text.
    pub fn highlight_job(&mut self) -> Option<(u64, Rope)> {
        if !self.highlight_stale || self.highlighting {
            return None;
        }
        self.highlighting = true;
        Some((self.text_version, self.document.text().clone()))
    }

    /// Highlighting rebuilt elsewhere for the text at `version`. Ignored if
    /// the text has changed since; the caller then rebuilds again.
    pub fn highlighted(&mut self, version: u64, highlighter: Highlighter) {
        self.highlighting = false;
        if version == self.text_version {
            self.highlighter = highlighter;
            self.highlight_stale = false;
        }
    }

    // --- Find and replace -------------------------------------------------

    /// Show the find bar, with the replace field if `replace`.
    pub fn open_find(&mut self, replace: bool) {
        self.find.open = true;
        self.find.replace_open = replace;
        self.find.replaced = None;
    }

    /// Hide the find bar. The current match, if any, becomes the selection.
    pub fn close_find(&mut self) {
        self.find.cancel();
        self.find.open = false;
        self.find.replace_open = false;
        self.find.searching = false;
        if let Some(m) = self.find.current_range()
            && !self.find.stale
            && !self.surface.is_composing()
        {
            self.document.set_selection(Selection {
                anchor: m.start,
                focus: m.end,
            });
            self.surface.set_selection(m.start, m.end);
            self.surface.reveal_caret();
        }
    }

    /// Look for `query`. A small document is searched at once; for a large
    /// one this returns a job to run on another thread, and the result comes
    /// back through [`Self::search_finished`].
    pub fn set_query(&mut self, query: Query) -> Option<SearchJob> {
        self.find.query = query;
        self.search()
    }

    /// Search again for the current query, and go to the first match at
    /// or after the selection.
    pub fn search(&mut self) -> Option<SearchJob> {
        self.find.follow = true;
        self.run_search()
    }

    /// Search again after the text changed, without moving the caret: the
    /// user may be typing in the document.
    pub fn refresh_search(&mut self) -> Option<SearchJob> {
        self.find.follow = false;
        self.run_search()
    }

    fn run_search(&mut self) -> Option<SearchJob> {
        let find = &mut self.find;
        find.cancel();
        find.generation += 1;
        find.error = None;
        find.stale = false;
        find.searching = false;
        let finder = match Finder::new(&find.query) {
            Ok(Some(finder)) => finder,
            Ok(None) => {
                find.finder = None;
                find.found = Found::default();
                find.current = None;
                return None;
            }
            Err(err) => {
                find.finder = None;
                find.error = Some(err);
                find.found = Found::default();
                find.current = None;
                return None;
            }
        };
        find.finder = Some(finder.clone());
        if self.document.len() <= SEARCH_INLINE {
            let text = self.document.text().to_string();
            let found = finder
                .find_all(&text, &AtomicBool::new(false), |_| {})
                .unwrap_or_default();
            self.found(found);
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        find.cancel = Some(cancel.clone());
        find.searching = true;
        Some(SearchJob {
            generation: find.generation,
            finder,
            text: self.document.text().clone(),
            cancel,
        })
    }

    /// A search from [`SearchJob::run`] finished. Ignored if another search
    /// has started since.
    pub fn search_finished(&mut self, generation: u64, found: Option<Found>) {
        if generation != self.find.generation {
            return;
        }
        self.find.searching = false;
        self.find.cancel = None;
        if let Some(found) = found {
            self.found(found);
        }
    }

    /// Take new matches; the current one is the first at or after the
    /// selection, and the caret goes there unless the search only followed
    /// an edit.
    fn found(&mut self, found: Found) {
        let from = self.document.selection().range().start;
        self.find.current = found.next_from(from);
        self.find.found = found;
        self.find.stale = false;
        if self.find.follow
            && let Some(index) = self.find.current
        {
            self.go_to_match(index);
        }
    }

    fn go_to_match(&mut self, index: usize) {
        let Some(m) = self.find.found.matches.get(index).cloned() else {
            return;
        };
        self.find.current = Some(index);
        if self.surface.is_composing() {
            return;
        }
        // The caret goes to the match, so it scrolls into view; the match
        // becomes the selection when the bar closes.
        self.document.set_selection(Selection::caret(m.start));
        self.surface.set_selection(m.start, m.start);
        self.surface.reveal_caret();
    }

    /// Go to the next match, or the previous one. If the matches are out
    /// of date they are looked for again first; for a large document that
    /// returns a job, and the step happens when it is done.
    pub fn find_step(&mut self, forward: bool) -> Option<SearchJob> {
        if self.find.finder.is_none() || self.find.stale {
            if self.find.query.pattern.is_empty() {
                return None;
            }
            let job = self.search();
            if job.is_some() {
                return job;
            }
        }
        let found = &self.find.found;
        let caret = self.document.selection().range().start;
        let next = match (self.find.current, forward) {
            (Some(index), true) => Some((index + 1) % found.len().max(1)),
            (Some(index), false) => Some(
                index
                    .checked_sub(1)
                    .unwrap_or(found.len().saturating_sub(1)),
            ),
            (None, true) => found.next_from(caret),
            (None, false) => found.previous_before(caret),
        };
        if let Some(index) = next.filter(|_| !found.is_empty()) {
            self.go_to_match(index);
        }
        None
    }

    /// Search for the selected text, as Cmd+E does.
    pub fn find_selection(&mut self) -> Option<SearchJob> {
        let selected = self.document.selected_text();
        if selected.is_empty() {
            return None;
        }
        self.find.query.pattern = selected;
        self.find.query.regex = false;
        self.search()
    }

    /// Replace the current match and go to the next one. Returns whether the
    /// text changed.
    pub fn replace_one(&mut self) -> bool {
        if self.locked() || self.surface.is_composing() {
            return false;
        }
        let (Some(m), Some(finder)) = (self.find.current_range(), self.find.finder.clone()) else {
            return false;
        };
        // Enough of the text around the match for look-around such as `\b`.
        let text = self.document.text();
        let from = floor_char_boundary(text, m.start.saturating_sub(1024));
        let to = floor_char_boundary(text, (m.end + 1024).min(text.len_bytes()));
        let window = slice(text, from..to);
        // The matches may have moved with edits since the search: make sure
        // this one still matches before replacing it.
        let local = m.start - from..m.end - from;
        if finder.find_from(&window, local.start) != Some(local) {
            self.find.stale = true;
            return false;
        }
        let replacement = finder.replacement(
            &window,
            m.start - from..m.end - from,
            &self.find.replacement,
        );
        let change = self.document.replace_range(m.clone(), &replacement);
        let changed = self.show(change);
        // The other matches moved along; go to the next one after this.
        self.find.stale = false;
        let after = m.start + replacement.len();
        if let Some(index) = self.find.found.next_from(after) {
            self.go_to_match(index);
        } else {
            self.find.current = None;
        }
        // A replacement may itself match, or join two pieces into a match:
        // look again soon.
        self.find.stale = true;
        changed
    }

    /// Replace every match. A small document is done at once, and this
    /// returns `None`; a large one returns a job to run on another thread,
    /// and the document takes no edits until [`Self::replace_all_finished`].
    pub fn replace_all(&mut self) -> Option<ReplaceJob> {
        if self.locked() || self.surface.is_composing() {
            return None;
        }
        let finder = match Finder::new(&self.find.query) {
            Ok(Some(finder)) => finder,
            _ => return None,
        };
        self.find.cancel();
        self.find.generation += 1;
        if self.document.len() <= SEARCH_INLINE {
            let text = self.document.text().to_string();
            let never = AtomicBool::new(false);
            let found = finder.find_all(&text, &never, |_| {}).unwrap_or_default();
            let count = found.len();
            if let Some((range, inserted)) =
                finder.replace_all(&text, &found.matches, &self.find.replacement, &never)
            {
                let change = self.document.replace_range(range, &inserted);
                self.show(change);
            }
            // Nothing left to replace (a second press, say) keeps the count
            // of the last replacement; the bar already says there is no match.
            if count > 0 {
                self.find.replaced = Some(count);
            }
            self.find.finder = Some(finder);
            self.search();
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.find.cancel = Some(cancel.clone());
        self.find.replacing = true;
        self.find.searching = false;
        Some(ReplaceJob {
            generation: self.find.generation,
            finder,
            text: self.document.text().clone(),
            replacement: self.find.replacement.clone(),
            cancel,
        })
    }

    /// A replacement from [`ReplaceJob::run`] finished. Returns whether the
    /// text changed. Ignored if it was cancelled and replaced since.
    pub fn replace_all_finished(&mut self, replaced: Option<Replaced>) -> bool {
        let Some(replaced) = replaced else {
            self.find.replacing = false;
            return false;
        };
        if replaced.generation != self.find.generation {
            return false;
        }
        self.find.replacing = false;
        self.find.cancel = None;
        if replaced.count == 0 {
            return false;
        }
        let count = replaced.count;
        let change = self.document.replace_with(
            replaced.range,
            replaced.inserted,
            replaced.removed,
            replaced.text,
        );
        let changed = self.show(Some(change));
        self.find.replaced = Some(count);
        self.find.found = Found::default();
        self.find.current = None;
        self.find.stale = true;
        changed
    }

    /// Stop a running "replace all": the document is left as it was.
    pub fn cancel_replace_all(&mut self) {
        if self.find.replacing {
            self.find.cancel();
            self.find.replacing = false;
            self.find.generation += 1;
        }
    }

    /// The text version, for matching a count made elsewhere to the text it
    /// was made of.
    pub fn text_version(&self) -> u64 {
        self.text_version
    }

    /// Counts made of the text at `version` on another thread. Ignored if
    /// the text has changed since.
    pub fn counted(&mut self, version: u64, stats: TextStats) {
        if version == self.text_version {
            self.stats = stats;
            self.stats_stale = false;
        }
    }

    /// Tell the document where the surface put the selection.
    fn adopt_selection(&mut self) {
        let (anchor, focus) = self.surface.selection();
        self.document.set_selection(Selection { anchor, focus });
    }

    /// Carry out an edit. Returns whether the text changed. While the input
    /// method is composing it owns the keyboard, and edits are ignored.
    pub fn edit(&mut self, command: EditCommand) -> bool {
        if self.surface.is_composing() || self.locked() {
            return false;
        }
        let change = self.document.apply(command);
        self.show(change)
    }

    /// Format the selection: emphasis, a heading level, a list or quote, or
    /// insert a link, image or table. One undo step. Returns whether the
    /// text changed. Not while composing, while a replacement throughout the
    /// document runs, or with only the preview on screen.
    pub fn format(&mut self, command: Format) -> bool {
        if self.surface.is_composing() || self.locked() || !self.view_mode.shows_source() {
            return false;
        }
        let text = self.document.text();
        let selection = self.document.selection();
        let edit = match command {
            Format::Inline(kind) => match self.fresh_highlighter() {
                Some(highlighter) => format::toggle_inline_in(text, highlighter, selection, kind),
                None => format::toggle_inline(text, selection, kind),
            },
            Format::Heading(level) => format::set_heading(text, selection, level),
            Format::Block(kind) => format::toggle_block(text, selection, kind),
            Format::Link => format::insert_link(text, selection, false),
            Format::Image => format::insert_link(text, selection, true),
            Format::Table => format::insert_table(text, selection),
        };
        let Some(mut change) = self.document.replace_range(edit.range, &edit.text) else {
            return false;
        };
        self.document.set_selection(edit.selection);
        change.selection = edit.selection;
        self.show(Some(change))
    }

    /// The highlighting, while it matches the text.
    fn fresh_highlighter(&self) -> Option<&Highlighter> {
        (!self.highlight_stale).then_some(&self.highlighter)
    }

    /// The styles a format button would take away from the selection, for
    /// showing those buttons pressed.
    pub fn styles(&self) -> format::Styles {
        format::styles(
            self.document.text(),
            self.fresh_highlighter(),
            self.document.selection(),
        )
    }

    /// The heading level of the line with the caret, 0 for body text.
    pub fn heading_level(&self) -> u8 {
        format::heading_level(self.document.text(), self.document.selection().focus)
    }

    /// Carry out what a key press decoded to. Returns whether the text changed.
    pub fn key(&mut self, action: Action) -> bool {
        match action {
            Action::Insert(text) => self.edit(EditCommand::Type(text)),
            Action::Newline => self.edit(EditCommand::LineBreak),
            Action::Backspace => self.edit(EditCommand::Backspace),
            Action::Delete => self.edit(EditCommand::Delete),
            movement => {
                // Only layout knows where "up" or "end of line" is.
                self.surface.apply(movement);
                self.adopt_selection();
                false
            }
        }
    }

    pub fn pointer_down(&mut self, x: f64, y: f64, extend: bool) {
        self.link_refused = false;
        self.preview_focused = false;
        self.surface.pointer_down(x, y, extend);
        if !self.surface.is_composing() {
            self.adopt_selection();
        }
    }

    pub fn pointer_drag(&mut self, x: f64, y: f64) {
        self.surface.pointer_drag(x, y);
        if !self.surface.is_composing() {
            self.adopt_selection();
        }
    }

    /// Handle an input-method event. Returns whether the text changed.
    ///
    /// Composition text lives on the surface only. The document hears of it
    /// once, when it is committed, so a whole composition is one undo step and
    /// nothing typed on the way to it is ever in the history.
    pub fn ime(&mut self, event: ImeEvent) -> bool {
        match event {
            ImeEvent::Enabled => false,
            ImeEvent::Preedit { .. } if self.locked() => false,
            ImeEvent::Preedit { text, cursor } => {
                let mut changed = false;
                if !text.is_empty() && !self.surface.is_composing() {
                    // The composition types over the selection.
                    let removed = self.document.begin_composition();
                    changed = self.show(removed);
                }
                self.surface.compose(&text, cursor);
                changed
            }
            ImeEvent::Commit(text) => {
                // Some platforms commit without first sending an empty preedit.
                self.surface.end_composition();
                self.edit(EditCommand::Commit(text))
            }
            ImeEvent::Disabled => {
                self.surface.end_composition();
                false
            }
        }
    }

    pub fn undo(&mut self) -> bool {
        if self.surface.is_composing() || self.locked() {
            return false;
        }
        let change = self.document.undo();
        self.show(change)
    }

    pub fn redo(&mut self) -> bool {
        if self.surface.is_composing() || self.locked() {
            return false;
        }
        let change = self.document.redo();
        self.show(change)
    }

    /// Put the caret at a byte offset, as a click there would, and bring it
    /// into view.
    pub fn place_caret(&mut self, at: usize) {
        if self.surface.is_composing() {
            return;
        }
        self.document.set_selection(Selection::caret(at));
        let selection = self.document.selection();
        self.surface
            .set_selection(selection.anchor, selection.focus);
        self.surface.reveal_caret();
    }

    pub fn select_all(&mut self) {
        if self.surface.is_composing() {
            return;
        }
        self.document.select_all();
        let selection = self.document.selection();
        self.surface
            .set_selection(selection.anchor, selection.focus);
    }

    /// The selected text, for the clipboard. `None` if nothing is selected.
    pub fn copy(&self) -> Option<String> {
        let text = self.document.selected_text();
        (!text.is_empty()).then_some(text)
    }

    /// Remove the selected text and return it for the clipboard.
    pub fn cut(&mut self) -> Option<String> {
        let text = self.copy()?;
        self.edit(EditCommand::DeleteSelection).then_some(text)
    }

    pub fn paste(&mut self, text: &str) -> bool {
        self.edit(EditCommand::Paste(text.to_owned()))
    }

    /// The text to write to disk, and the name of the state it is.
    pub fn snapshot(&mut self) -> (Rope, SavePoint) {
        (self.document.text().clone(), self.document.save_point())
    }

    /// The document is too large for the preview; the pane says so.
    pub fn preview_too_large(&self) -> bool {
        self.document.len() > PREVIEW_LIMIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeMode;

    fn editor(text: &str) -> MarkdownEditor {
        let tokens = theme::Tokens::of(ThemeMode::Light);
        let mut editor = MarkdownEditor::new(text, TextFormat::default(), None, None, &tokens);
        editor.surface.resize(1200, 600, 2.0);
        editor
    }

    /// The invariant everything rests on.
    fn assert_in_step(editor: &MarkdownEditor) {
        assert_eq!(editor.surface.text(), editor.document.text().to_string());
        if !editor.surface.is_composing() {
            let selection = editor.document.selection();
            assert_eq!(
                editor.surface.selection(),
                (selection.anchor, selection.focus)
            );
        }
    }

    fn type_text(editor: &mut MarkdownEditor, text: &str) {
        for c in text.chars() {
            editor.key(Action::Insert(c.to_string()));
            assert_in_step(editor);
        }
    }

    fn preedit(text: &str) -> ImeEvent {
        ImeEvent::Preedit {
            text: text.to_owned(),
            cursor: Some((text.len(), text.len())),
        }
    }

    const END: Action = Action::LineEnd { select: false };

    #[test]
    fn a_file_opens_at_its_beginning_unmodified() {
        let editor = editor("# Title\n\nBody\n");
        assert_in_step(&editor);
        assert_eq!(editor.document.selection(), Selection::caret(0));
        assert!(!editor.document.is_modified());
        assert_eq!(editor.stats.words, 2);
    }

    #[test]
    fn typing_and_keys_keep_document_and_surface_in_step() {
        let mut editor = editor("");
        type_text(&mut editor, "hello");
        editor.key(Action::Newline);
        type_text(&mut editor, "world");
        assert_eq!(editor.document.text(), "hello\nworld");
        editor.key(Action::Backspace);
        editor.key(Action::Up { select: false });
        assert_in_step(&editor);
        editor.key(Action::LineStart { select: false });
        editor.key(Action::Delete);
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "ello\nworl");
        assert!(editor.document.is_modified());
        assert_eq!(editor.stats.words, 2);
    }

    #[test]
    fn selecting_with_shift_then_typing_replaces_the_selection() {
        let mut editor = editor("hello world");
        editor.key(END);
        for _ in 0..5 {
            editor.key(Action::Left {
                select: true,
                word: false,
            });
        }
        assert_in_step(&editor);
        assert_eq!(editor.copy().as_deref(), Some("world"));
        type_text(&mut editor, "X");
        assert_eq!(editor.document.text(), "hello X");
    }

    #[test]
    fn pointer_selection_reaches_the_document() {
        let mut editor = editor("hello world, this is a line of text");
        editor.pointer_down(120.0, 110.0, false);
        editor.pointer_drag(360.0, 110.0);
        assert_in_step(&editor);
        let selection = editor.document.selection();
        assert!(!selection.is_caret(), "the drag selected nothing");
        assert_eq!(editor.copy(), Some(editor.document.selected_text()));
    }

    #[test]
    fn undo_and_redo_restore_text_and_selection_on_both_sides() {
        let mut editor = editor("");
        type_text(&mut editor, "one");
        editor.key(Action::Newline);
        type_text(&mut editor, "two");
        assert!(editor.undo());
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "one\n");
        assert!(editor.undo());
        assert!(editor.undo());
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "");
        assert!(!editor.undo());
        assert!(editor.redo());
        assert!(editor.redo());
        assert!(editor.redo());
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "one\ntwo");
        assert!(!editor.redo());
    }

    #[test]
    fn a_composition_is_invisible_to_the_document_until_it_commits() {
        let mut editor = editor("ab");
        editor.key(END);
        assert!(!editor.ime(preedit("n")));
        assert!(!editor.ime(preedit("ni")));
        assert!(!editor.ime(preedit("nihao")));
        assert!(editor.surface.is_composing());
        assert_eq!(editor.document.text(), "ab");
        assert!(!editor.document.is_modified());
        assert!(!editor.document.can_undo());

        // Clearing the preedit changes no committed text.
        assert!(!editor.ime(ImeEvent::Preedit {
            text: String::new(),
            cursor: None,
        }));
        assert!(editor.ime(ImeEvent::Commit("你好".to_owned())));
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "ab你好");

        // One step back takes the whole commit, and nothing else.
        assert!(editor.undo());
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "ab");
        assert!(!editor.document.can_undo());
    }

    #[test]
    fn a_commit_without_a_cleared_preedit_inserts_once() {
        let mut editor = editor("");
        editor.ime(preedit("にほんご"));
        assert!(editor.ime(ImeEvent::Commit("日本語".to_owned())));
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "日本語");
        assert!(!editor.surface.is_composing());
    }

    #[test]
    fn keys_and_commands_are_ignored_while_composing() {
        let mut editor = editor("ab");
        editor.key(END);
        type_text(&mut editor, "c");
        editor.ime(preedit("ni"));
        assert!(!editor.key(Action::Backspace));
        assert!(!editor.key(Action::Insert("z".to_owned())));
        assert!(!editor.undo());
        assert!(!editor.paste("x"));
        editor.select_all();
        assert!(editor.surface.is_composing());
        assert_eq!(editor.document.text(), "abc");
    }

    #[test]
    fn a_cancelled_composition_leaves_no_trace() {
        let mut editor = editor("x");
        editor.key(END);
        editor.ime(preedit("zhong"));
        editor.ime(ImeEvent::Disabled);
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "x");
        assert!(!editor.document.is_modified());
        // Typing works again afterwards.
        type_text(&mut editor, "y");
        assert_eq!(editor.document.text(), "xy");
    }

    #[test]
    fn composing_over_a_selection_replaces_it_as_one_undo_step() {
        let mut editor = editor("hello world");
        editor.key(END);
        for _ in 0..5 {
            editor.key(Action::Left {
                select: true,
                word: false,
            });
        }
        assert!(editor.ime(preedit("shijie")));
        assert_eq!(editor.document.text(), "hello ");
        assert!(editor.ime(ImeEvent::Commit("世界".to_owned())));
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "hello 世界");
        assert!(editor.undo());
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "hello world");
        assert_eq!(editor.copy().as_deref(), Some("world"));
    }

    #[test]
    fn cut_copy_and_paste() {
        let mut editor = editor("alpha beta");
        assert_eq!(editor.copy(), None);
        assert_eq!(editor.cut(), None);
        editor.select_all();
        assert_in_step(&editor);
        assert_eq!(editor.cut().as_deref(), Some("alpha beta"));
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "");
        assert!(editor.paste("中文\r\n日本語"));
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "中文\n日本語");
        assert!(editor.undo());
        assert!(editor.undo());
        assert_eq!(editor.document.text(), "alpha beta");
    }

    #[test]
    fn the_view_follows_the_caret_when_typing_runs_off_the_bottom() {
        let mut editor = editor("");
        for _ in 0..60 {
            type_text(&mut editor, "line");
            editor.key(Action::Newline);
        }
        let (scroll, max) = editor.surface.scroll_range();
        assert!(max > 0.0 && scroll > 0.0);
        let (_, y, _, height) = editor.surface.ime_cursor_area();
        assert!(y >= 0.0 && y + height <= 600.0, "caret at {y}");
        // Undoing everything brings the view back to the top.
        while editor.undo() {}
        assert_eq!(editor.surface.scroll_range().0, 0.0);
    }

    #[test]
    fn saving_clears_the_modified_flag_for_the_state_that_was_written() {
        let mut editor = editor("");
        type_text(&mut editor, "ab");
        let (text, point) = editor.snapshot();
        assert_eq!(text, "ab");
        // The user keeps typing while the save runs.
        type_text(&mut editor, "c");
        editor.document.mark_saved(point);
        assert!(editor.document.is_modified());
        editor.undo();
        assert!(!editor.document.is_modified());
    }

    /// Apply the steps to a model of the record, the way the writer would.
    #[derive(Default)]
    struct Record {
        open: bool,
        base: String,
        entries: Vec<Vec<u8>>,
    }

    impl Record {
        fn take(&mut self, step: JournalStep) {
            match step {
                JournalStep::Nothing => {}
                JournalStep::Open(text) => {
                    *self = Record {
                        open: true,
                        base: text.to_string(),
                        entries: Vec::new(),
                    };
                }
                JournalStep::Append(entries, checkpoint) => {
                    assert!(self.open, "appending to a record that is not open");
                    self.entries.extend(entries);
                    if let Some(text) = checkpoint {
                        self.base = text.to_string();
                        self.entries.clear();
                    }
                }
                JournalStep::Discard => *self = Record::default(),
            }
        }

        fn text(&self) -> String {
            aster_markdown::journal::rebuild(self.base.as_bytes(), &self.entries)
                .unwrap()
                .to_string()
        }
    }

    #[test]
    fn the_record_follows_the_document() {
        let mut editor = editor("saved\n");
        let mut record = Record::default();
        let mut keys = 0..;
        let mut sync = |editor: &mut MarkdownEditor, record: &mut Record| {
            let step = editor.journal_step(|| keys.next().unwrap());
            record.take(step);
        };

        // Nothing unsaved, no record.
        sync(&mut editor, &mut record);
        assert!(!record.open && editor.record.is_none());

        editor.key(END);
        editor.key(Action::Down { select: false });
        type_text(&mut editor, "中文");
        sync(&mut editor, &mut record);
        assert!(record.open);
        assert_eq!(editor.record, Some(0));
        assert_eq!(record.text(), editor.document.text().to_string());

        editor.ime(preedit("nihao"));
        // A composition is not an edit.
        sync(&mut editor, &mut record);
        assert_eq!(record.text(), "saved\n中文");
        editor.ime(ImeEvent::Commit("你好".to_owned()));
        editor.undo();
        editor.redo();
        assert!(editor.paste("日本語"));
        sync(&mut editor, &mut record);
        assert_eq!(record.text(), editor.document.text().to_string());

        // Saved: the record goes.
        let (_, point) = editor.snapshot();
        editor.document.mark_saved(point);
        sync(&mut editor, &mut record);
        assert!(!record.open && editor.record.is_none());

        // Modified again: a new record, starting from the text as it is.
        type_text(&mut editor, "!");
        sync(&mut editor, &mut record);
        assert_eq!(editor.record, Some(1));
        assert_eq!(record.text(), editor.document.text().to_string());

        // Undone back to the saved text: the record goes again.
        editor.undo();
        sync(&mut editor, &mut record);
        assert!(!record.open);
    }

    #[test]
    fn long_editing_checkpoints_the_record() {
        let mut editor = editor("");
        let mut record = Record::default();
        let mut checkpoints = 0;
        for n in 0..(CHECKPOINT_ENTRIES * 3) {
            type_text(&mut editor, if n % 40 == 39 { "\n" } else { "字" });
            let step = editor.journal_step(|| 7);
            if matches!(step, JournalStep::Append(_, Some(_))) {
                checkpoints += 1;
            }
            record.take(step);
            assert!(record.entries.len() as u64 <= CHECKPOINT_ENTRIES);
        }
        assert!(checkpoints >= 2, "{checkpoints} checkpoints");
        assert_eq!(record.text(), editor.document.text().to_string());
    }

    #[test]
    fn a_restored_document_has_unsaved_changes_and_a_record_at_once() {
        let tokens = theme::Tokens::of(ThemeMode::Light);
        let mut editor = MarkdownEditor::recovered(
            prepare(Rope::from_str("restored")),
            TextFormat::default(),
            Some(PathBuf::from("/tmp/a.md")),
            None,
            Some(DiskWarning::Changed),
            &tokens,
        );
        assert!(editor.document.is_modified());
        assert_eq!(editor.disk_warning, Some(DiskWarning::Changed));
        assert_eq!(
            editor.journal_step(|| 3),
            JournalStep::Open(Rope::from_str("restored"))
        );
    }

    #[test]
    fn writer_and_markdown_views_are_styled_differently() {
        let tokens = theme::Tokens::of(ThemeMode::Dark);
        assert_ne!(palette(&tokens).desk, editor_palette(&tokens).desk);
        assert_eq!(editor_palette(&tokens).desk, editor_palette(&tokens).page);
        assert_eq!(page_layout(&tokens).max_width, Some(794.0));
        assert!(editor_layout(&tokens).min_aspect.is_none());
    }

    fn style() -> PreviewStyle {
        preview_style(&theme::Tokens::of(ThemeMode::Light), &crate::i18n::ZH_HANS)
    }

    /// Lay out the preview here instead of on the worker thread.
    fn lay_out(editor: &mut MarkdownEditor) {
        let job = editor.preview_job(1, true, style()).expect("a preview job");
        let layout = aster_surface::preview::PreviewBuilder::new().build(
            &job.source,
            job.width,
            job.scale,
            &job.style,
        );
        editor.show_preview(layout);
    }

    fn decorated(editor: &mut MarkdownEditor, needle: &str) -> TextStyle {
        let at = editor
            .document
            .text()
            .to_string()
            .find(needle)
            .expect("needle");
        let spans = editor.highlighter.spans(at..at + 1);
        decorations(spans, &editor.syntax)
            .into_iter()
            .find(|d| d.range.contains(&at))
            .map_or_else(TextStyle::default, |d| d.style)
    }

    #[test]
    fn highlighting_follows_typing() {
        let mut e = editor("");
        type_text(&mut e, "# 标题\n\n正文 **粗**");
        assert!(decorated(&mut e, "标题").bold);
        let marker = decorated(&mut e, "# ");
        assert_eq!(marker.color, Some(e.syntax.marker));
        assert!(decorated(&mut e, "粗").bold);
        assert_eq!(decorated(&mut e, "正文"), TextStyle::default());
        // Undo takes the highlighting back with the text.
        e.undo();
        assert!(!e.document.text().to_string().contains("**粗**"));
        e.decorate();
        assert_in_step(&e);
    }

    #[test]
    fn code_and_links_are_coloured_from_the_tokens() {
        let tokens = theme::Tokens::of(ThemeMode::Dark);
        let mut e = MarkdownEditor::new(
            "`code` [链接](https://a.example)\n\n```\nblock\n```\n",
            TextFormat::default(),
            None,
            None,
            &tokens,
        );
        let c = &tokens.colors;
        assert_eq!(
            decorated(&mut e, "code").background,
            Some(c.fill.to_array())
        );
        assert_eq!(decorated(&mut e, "链接").color, Some(c.link.to_array()));
        assert_eq!(
            decorated(&mut e, "https").color,
            Some(c.text_tertiary.to_array())
        );
        assert_eq!(
            decorated(&mut e, "block").line_background,
            Some(c.fill.to_array())
        );
    }

    #[test]
    fn composition_with_highlighting_commits_once() {
        let mut e = editor("## ");
        e.key(END);
        e.ime(preedit("biao"));
        e.decorate();
        e.surface.render();
        e.ime(preedit("标题"));
        e.ime(ImeEvent::Commit("标题".to_owned()));
        assert_eq!(e.document.text(), "## 标题");
        assert_in_step(&e);
        assert!(decorated(&mut e, "标题").bold);
    }

    #[test]
    fn the_preview_is_asked_for_only_when_shown_and_out_of_date() {
        let mut e = editor("# 一\n");
        assert!(e.preview_job(1, true, style()).is_none(), "edit mode");
        e.set_view_mode(ViewMode::Split);
        assert!(e.preview_job(1, true, style()).is_none(), "no size yet");
        e.preview.resize(800, 600, 2.0);
        let job = e.preview_job(1, true, style()).expect("first layout");
        assert_eq!(job.source, "# 一\n");
        assert!(e.preview_job(1, true, style()).is_none(), "nothing changed");
        type_text(&mut e, "二");
        assert!(e.preview_is_behind());
        assert!(e.preview_job(1, false, style()).is_none(), "still typing");
        assert!(e.preview_job(1, true, style()).is_some(), "typing paused");
        // A new size or theme asks again even while typing.
        e.preview.resize(900, 600, 2.0);
        assert!(e.preview_job(1, false, style()).is_some());
        let dark = preview_style(&theme::Tokens::of(ThemeMode::Dark), &crate::i18n::ZH_HANS);
        assert!(e.preview_job(1, false, dark).is_some());
    }

    #[test]
    fn switching_views_keeps_the_same_part_of_the_document() {
        let text: String = (0..300).map(|i| format!("第 {i} 段文字。\n\n")).collect();
        let mut e = editor(&text);
        let target = text.find("第 150 段").unwrap() as f64;
        e.surface.scroll_to_position(target);

        e.set_view_mode(ViewMode::Split);
        e.preview.resize(1200, 600, 2.0);
        lay_out(&mut e);
        let near = |a: f64, b: f64| (a - b).abs() < 40.0;
        assert!(
            near(e.preview.top_position(), target),
            "{}",
            e.preview.top_position()
        );

        // Scrolling the preview takes the editor along, and the editor does
        // not pull the preview back.
        assert!(e.scroll_preview(2000.0));
        let preview_top = e.preview.top_position();
        assert!(near(e.surface.top_position(), preview_top));
        e.follow_editor();
        assert_eq!(e.preview.top_position(), preview_top);

        // Scrolling the editor takes the preview along.
        e.surface.scroll_by(-1500.0);
        e.follow_editor();
        assert!(near(e.preview.top_position(), e.surface.top_position()));

        // Preview only, then back: the editor comes back where the preview was.
        e.set_view_mode(ViewMode::Preview);
        e.scroll_preview(3000.0);
        let seen = e.preview.top_position();
        e.set_view_mode(ViewMode::Edit);
        assert!(near(e.surface.top_position(), seen));
        assert!(e.preview_job(1, true, style()).is_none());
    }

    fn query(pattern: &str) -> Query {
        Query {
            pattern: pattern.to_owned(),
            case_sensitive: true,
            ..Query::default()
        }
    }

    fn text_of(editor: &MarkdownEditor) -> String {
        editor.document.text().to_string()
    }

    #[test]
    fn finding_moves_through_the_matches_and_closing_selects() {
        let mut e = editor("中文 one 中文 two 中文");
        e.open_find(false);
        assert!(
            e.set_query(query("中文")).is_none(),
            "small: searched at once"
        );
        assert_eq!(e.find.found.len(), 3);
        assert_eq!(e.find.current, Some(0));
        e.find_step(true);
        assert_eq!(e.find.current, Some(1));
        e.find_step(true);
        e.find_step(true);
        assert_eq!(e.find.current, Some(0), "wraps");
        e.find_step(false);
        assert_eq!(e.find.current, Some(2));
        e.close_find();
        let m = e.find.found.matches[2].clone();
        assert_eq!(e.document.selection().range(), m);
        assert_eq!(e.surface.selection(), (m.start, m.end));
        assert_in_step(&e);
        // A query that is not a valid regular expression says so.
        e.open_find(false);
        e.set_query(Query {
            regex: true,
            ..query("(")
        });
        assert!(e.find.error.is_some());
        assert!(e.find.found.is_empty());
        // An empty query matches nothing.
        e.set_query(query(""));
        assert!(e.find.found.is_empty() && e.find.error.is_none());
    }

    #[test]
    fn replacing_one_goes_on_to_the_next() {
        let mut e = editor("cat cat cat");
        e.open_find(true);
        e.set_query(query("cat"));
        e.find.replacement = "狗".into();
        assert!(e.replace_one());
        assert_eq!(text_of(&e), "狗 cat cat");
        assert_eq!(e.find.current_range(), Some(4..7));
        assert!(e.replace_one());
        assert_eq!(text_of(&e), "狗 狗 cat");
        e.undo();
        assert_eq!(text_of(&e), "狗 cat cat");
        assert_in_step(&e);
    }

    #[test]
    fn formatting_is_one_undo_step_and_keeps_the_text_selected() {
        use aster_markdown::format::{Block, Inline};
        let mut e = editor("会议纪要\n第一项\n第二项");
        // Select 纪要 and make it bold.
        e.document.set_selection(Selection {
            anchor: 6,
            focus: 12,
        });
        e.surface.set_selection(6, 12);
        assert!(e.format(Format::Inline(Inline::Bold)));
        assert_eq!(text_of(&e), "会议**纪要**\n第一项\n第二项");
        assert_eq!(e.document.selection().range(), 8..14);
        assert_in_step(&e);
        // The first line becomes a heading; the level is what the toolbar shows.
        assert!(e.format(Format::Heading(1)));
        assert_eq!(e.heading_level(), 1);
        // The two items become a numbered list.
        let start = text_of(&e).find("第一项").unwrap();
        let end = text_of(&e).len();
        e.document.set_selection(Selection {
            anchor: start,
            focus: end,
        });
        assert!(e.format(Format::Block(Block::Numbered)));
        assert_eq!(text_of(&e), "# 会议**纪要**\n1. 第一项\n2. 第二项");
        assert_in_step(&e);
        // Each command is one step back.
        e.undo();
        assert_eq!(text_of(&e), "# 会议**纪要**\n第一项\n第二项");
        e.undo();
        e.undo();
        assert_eq!(text_of(&e), "会议纪要\n第一项\n第二项");
    }

    #[test]
    fn the_caret_in_bold_text_shows_bold_and_takes_it_off() {
        use aster_markdown::format::{Block, Inline};
        let mut e = editor("- 会议**纪要**");
        // The caret between 纪 and 要.
        e.document.set_selection(Selection::caret(13));
        let styles = e.styles();
        assert!(styles.bold && !styles.italic);
        assert_eq!(styles.block, Some(Block::Bullet));
        assert!(e.format(Format::Inline(Inline::Bold)));
        assert_eq!(text_of(&e), "- 会议纪要");
        assert_eq!(e.document.selection(), Selection::caret(11));
        assert!(!e.styles().bold);
        assert_in_step(&e);
        e.undo();
        assert_eq!(text_of(&e), "- 会议**纪要**");
    }

    #[test]
    fn formatting_waits_for_the_source() {
        let mut e = editor("文字");
        e.set_view_mode(ViewMode::Preview);
        assert!(!e.format(Format::Table));
        assert_eq!(text_of(&e), "文字");
        e.set_view_mode(ViewMode::Split);
        assert!(e.format(Format::Table));
    }

    #[test]
    fn replace_all_is_one_undo_step_with_groups() {
        let mut e = editor("2026-11-23\n2025-01-02 和 2024-12-31");
        e.open_find(true);
        e.set_query(Query {
            regex: true,
            ..query(r"(\d{4})-(\d{2})-(\d{2})")
        });
        e.find.replacement = "$3/$2/$1".into();
        assert!(e.replace_all().is_none(), "small: done at once");
        assert_eq!(e.find.replaced, Some(3));
        assert_eq!(text_of(&e), "23/11/2026\n02/01/2025 和 31/12/2024");
        assert!(e.find.found.is_empty(), "nothing left to find");
        e.undo();
        assert_eq!(text_of(&e), "2026-11-23\n2025-01-02 和 2024-12-31");
        assert_eq!(e.find.replaced, None, "the count went with the undo");
        assert_in_step(&e);
    }

    #[test]
    fn matches_follow_typing() {
        let mut e = editor("ab ab");
        e.open_find(false);
        e.set_query(query("ab"));
        e.place_caret(0);
        type_text(&mut e, "x");
        assert!(e.find.stale);
        assert_eq!(e.find.found.matches, [1..3, 4..6]);
        assert!(e.refresh_search().is_none());
        assert_eq!(e.find.found.matches, [1..3, 4..6]);
        assert!(!e.find.stale);
        // Searching again after typing leaves the caret where it was typed.
        assert_eq!(e.document.selection().range(), 1..1);
    }

    #[test]
    fn finding_during_a_composition_leaves_the_caret_alone() {
        let mut e = editor("one two one");
        e.place_caret(3);
        e.ime(preedit("ni"));
        e.open_find(false);
        e.set_query(query("one"));
        assert!(e.surface.is_composing());
        assert_eq!(e.document.selection().range(), 3..3);
        e.ime(ImeEvent::Commit("你".into()));
        assert_eq!(text_of(&e), "one你 two one");
        assert_in_step(&e);
    }

    /// Above the inline limit, searching and replacing go through jobs.
    #[test]
    fn a_large_document_searches_and_replaces_through_jobs() {
        let line = "大文件里的一行 with a needle 和更多文字。\n";
        let text = line.repeat(LARGE_CHANGE / line.len() + 10);
        let count = text.matches("needle").count();
        let mut e = editor(&text);
        e.open_find(true);
        let job = e.set_query(query("needle")).expect("a job");
        assert!(e.find.searching);
        let (generation, found) = job.run();
        e.search_finished(generation, found);
        assert!(!e.find.searching);
        assert_eq!(e.find.found.len(), count);

        // A cancelled search reports nothing and changes nothing.
        let job = e.set_query(query("needle")).unwrap();
        let stale = e.set_query(query("文字")).unwrap();
        let (old_generation, old) = job.run();
        assert!(old.is_none(), "replaced by a newer search");
        e.search_finished(old_generation, old);
        let (generation, found) = stale.run();
        e.search_finished(generation, found);
        assert!(e.find.found.len() >= count);

        // Replace all: locked until it is done, then one undo step.
        if let Some(job) = e.set_query(query("needle")) {
            let (generation, found) = job.run();
            e.search_finished(generation, found);
        }
        e.find.replacement = "针".into();
        let job = e.replace_all().expect("a job");
        assert!(e.locked());
        assert!(
            !e.key(Action::Insert("x".into())),
            "no edits while replacing"
        );
        let replaced = job.run();
        assert!(e.replace_all_finished(replaced));
        assert!(!e.locked());
        assert_eq!(e.find.replaced, Some(count));
        assert_eq!(text_of(&e), text.replace("needle", "针"));
        assert_in_step(&e);
        // Too large to highlight incrementally: rebuilt elsewhere.
        assert!(e.highlight_stale);
        let (version, rope) = e.highlight_job().expect("a rebuild");
        e.highlighted(version, Highlighter::new(&rope));
        assert!(!e.highlight_stale);
        // The recovery record is checkpointed rather than given the edit.
        let step = e.journal_step(|| 1);
        assert!(matches!(step, JournalStep::Open(_)));
        e.undo();
        assert_eq!(text_of(&e), text);
        assert_in_step(&e);
        let step = e.journal_step(|| 1);
        assert!(
            matches!(step, JournalStep::Discard),
            "back to what is on disk"
        );
    }
}
