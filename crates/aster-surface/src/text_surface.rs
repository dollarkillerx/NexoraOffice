//! A plain-text document surface: it takes input events, owns the text and its
//! layout, and renders into a pixel buffer.
//!
//! No Slint and no winit types appear in this file. That boundary is the one the
//! architecture relies on: the shell can be replaced without touching anything
//! here. The code started as the Phase 0 gate G1 spike and still backs it.

use std::ops::Range;

use parley::fontique::Script;
use parley::layout::PositionedLayoutItem;
use parley::{
    Cursor, FontContext, GenericFamily, Layout, LayoutContext, PlainEditor, PlainEditorDriver,
    StyleProperty,
};
use vello_cpu::kurbo::{Affine, Rect, RoundedRect, Shape};
use vello_cpu::peniko::{Color, Fill};
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

use crate::glyphs::{self, GlyphPaint, SYNTHETIC_SKEW_DEGREES};

/// Body text size in logical pixels at 100 % zoom.
const FONT_SIZE: f32 = 18.0;
/// Language used to pick fallback fonts for Han characters.
pub(crate) const DEFAULT_LANGUAGE: &str = "zh-Hans";
/// Caret width in logical pixels.
pub(crate) const CARET_WIDTH: f32 = 1.5;
/// Space kept between the caret and the top or bottom edge when the view
/// scrolls to follow it, in logical pixels.
pub(crate) const CARET_SCROLL_MARGIN: f64 = 24.0;
/// Zoom is clamped to this range.
pub const ZOOM_RANGE: (f32, f32) = (0.5, 3.0);

/// A colour as straight (not premultiplied) 8-bit RGBA.
pub type Rgba8 = [u8; 4];

pub(crate) fn color(rgba: Rgba8) -> Color {
    Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3])
}

/// The colours the surface paints with. The shell fills this from its theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// Background around the page.
    pub desk: Rgba8,
    pub page: Rgba8,
    /// One-pixel outline of the page. Fully transparent draws none.
    pub page_edge: Rgba8,
    /// Text, and the underline of composition text.
    pub ink: Rgba8,
    pub selection: Rgba8,
    pub caret: Rgba8,
}

impl Default for Palette {
    /// The colours of the G1 spike.
    fn default() -> Self {
        Self {
            desk: [0xf1, 0xf1, 0xf0, 0xff],
            page: [0xff, 0xff, 0xff, 0xff],
            page_edge: [0, 0, 0, 0],
            ink: [0x0b, 0x0b, 0x0b, 0xff],
            selection: [0xcf, 0xe3, 0xfa, 0xff],
            caret: [0x1a, 0x5f, 0xd0, 0xff],
        }
    }
}

/// Where the page sits inside the surface. Lengths are logical pixels at 100 %
/// zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageLayout {
    /// Gap between the surface edge and the page.
    pub margin: f64,
    /// Gap between the page edge and the text. Shrinks on a narrow surface.
    pub padding: f64,
    /// Widest the page gets; it is centred when the surface is wider. `None`
    /// lets the page fill the surface.
    pub max_width: Option<f64>,
    /// Smallest page height as a multiple of its width, e.g. 297 / 210 for an
    /// A4 sheet. `None` makes the page at least as tall as the surface.
    pub min_aspect: Option<f64>,
}

impl Default for PageLayout {
    /// The layout of the G1 spike: one page that fills the window.
    fn default() -> Self {
        Self {
            margin: 16.0,
            padding: 32.0,
            max_width: None,
            min_aspect: None,
        }
    }
}

/// How a stretch of text is drawn, on top of the surface's own style. None of
/// it changes where glyphs are: bold and italic are drawn by thickening and
/// slanting the regular face, so a decoration never moves the text, the caret
/// or line breaks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextStyle {
    /// Text colour; `None` keeps the ink.
    pub color: Option<Rgba8>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Background behind the glyphs.
    pub background: Option<Rgba8>,
    /// Background across the whole text column on every line this touches.
    pub line_background: Option<Rgba8>,
}

/// A style for a byte range of the committed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoration {
    pub range: Range<usize>,
    pub style: TextStyle,
}

/// The brush Parley carries on every glyph run. Solid colour only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Brush(pub Color);

impl Default for Brush {
    fn default() -> Self {
        Self(color(Palette::default().ink))
    }
}

/// Input-method events, already detached from the windowing library.
#[derive(Clone, Debug, PartialEq)]
pub enum ImeEvent {
    Enabled,
    /// Composition text that is not committed yet. `cursor` is a byte range
    /// inside `text`; `None` asks for the caret to be hidden.
    Preedit {
        text: String,
        cursor: Option<(usize, usize)>,
    },
    Commit(String),
    Disabled,
}

/// Editing commands decoded from key presses by the shell.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Insert(String),
    Newline,
    /// Shift+Return: a line break and nothing else, where Return would also
    /// carry a list on.
    PlainNewline,
    /// Tab: a tab character here; editors with lists move items in a level.
    Indent,
    /// Shift+Tab: nothing here; editors with lists move items out a level.
    Outdent,
    Backspace,
    Delete,
    Left {
        select: bool,
        word: bool,
    },
    Right {
        select: bool,
        word: bool,
    },
    Up {
        select: bool,
    },
    Down {
        select: bool,
    },
    LineStart {
        select: bool,
    },
    LineEnd {
        select: bool,
    },
    SelectAll,
}

/// System families tried, in order, for Han characters. The first one that has a
/// glyph for a character wins.
///
/// This list exists because fontique 0.13 cannot be left to choose on its own:
/// it keeps a single cached fallback for "Han, default language", filled by
/// whichever query runs first. On a Mac whose system language is Japanese that is
/// Hiragino Sans, which has no glyphs for Simplified-only characters, and PingFang
/// is not usable at all. The product bundles its own fonts and sets this chain
/// from them; until then this uses fonts that ship with macOS. On other systems
/// none of these names exist and the system default is left in place.
pub const HAN_FALLBACK_FAMILIES: [&str; 4] =
    ["Hiragino Sans GB", "Heiti SC", "Songti SC", "Hiragino Sans"];

pub(crate) fn configure_han_fallback(font_cx: &mut FontContext) {
    let han = Script::from_bytes(*b"Hani");
    let families: Vec<_> = HAN_FALLBACK_FAMILIES
        .iter()
        .filter_map(|name| font_cx.collection.family_id(name))
        .collect();
    if !families.is_empty() {
        font_cx.collection.set_fallbacks(han, families.into_iter());
    }
}

/// Page and text placement for the current size, zoom and scroll offset, in
/// physical pixels relative to the surface.
#[derive(Clone, Copy, Debug)]
struct Geometry {
    page_x: f64,
    page_y: f64,
    page_width: f64,
    page_height: f64,
    text_x: f64,
    text_y: f64,
    max_scroll: f64,
}

pub struct TextSurface {
    font_cx: FontContext,
    layout_cx: LayoutContext<Brush>,
    editor: PlainEditor<Brush>,
    resources: Resources,
    ctx: RenderContext,
    pixmap: Pixmap,
    width: u16,
    height: u16,
    scale: f32,
    zoom: f32,
    scroll: f64,
    caret_on: bool,
    palette: Palette,
    page: PageLayout,
    /// In order, not overlapping, in committed-text offsets.
    decorations: Vec<Decoration>,
}

/// Where a byte offset of the text Parley lays out — committed text with any
/// composition text spliced in at the caret — is in the committed text.
/// `None` inside the composition text.
fn committed_offset(compose: &Option<Range<usize>>, raw: usize) -> Option<usize> {
    match compose {
        Some(c) if raw >= c.end => Some(raw - c.len()),
        Some(c) if raw >= c.start => None,
        _ => Some(raw),
    }
}

/// The reverse of [`committed_offset`].
fn raw_offset(compose: &Option<Range<usize>>, committed: usize) -> usize {
    match compose {
        Some(c) if committed >= c.start => committed + c.len(),
        _ => committed,
    }
}

/// The style at a committed-text offset.
pub(crate) fn style_at(decorations: &[Decoration], offset: usize) -> TextStyle {
    let index = decorations.partition_point(|d| d.range.end <= offset);
    match decorations.get(index) {
        Some(d) if d.range.start <= offset => d.style,
        _ => TextStyle::default(),
    }
}

/// The line background for a committed-text range, if a decoration with
/// one overlaps it.
pub(crate) fn line_background_in(decorations: &[Decoration], range: Range<usize>) -> Option<Rgba8> {
    let first = decorations.partition_point(|d| d.range.end <= range.start);
    decorations[first..]
        .iter()
        .take_while(|d| d.range.start < range.end)
        .find_map(|d| d.style.line_background)
}

/// Index of the line holding byte `index` of the laid-out text.
fn line_index_of(layout: &Layout<Brush>, index: usize) -> usize {
    let (mut low, mut high) = (0, layout.len());
    while low + 1 < high {
        let mid = (low + high) / 2;
        match layout.get(mid) {
            Some(line) if line.text_range().start <= index => low = mid,
            _ => high = mid,
        }
    }
    low
}

impl TextSurface {
    pub fn new(text: &str) -> Self {
        let palette = Palette::default();
        let mut editor = PlainEditor::new(FONT_SIZE);
        editor.set_text(text);
        let styles = editor.edit_styles();
        styles.insert(GenericFamily::SystemUi.into());
        styles.insert(StyleProperty::Brush(Brush(color(palette.ink))));
        // Without a language, Han characters fall back to a Japanese system font
        // on macOS and Simplified-only characters come out as missing glyphs:
        // the fallback is chosen per script and language, not per character.
        styles.insert(StyleProperty::Locale(DEFAULT_LANGUAGE.parse().ok()));
        styles.insert(StyleProperty::LineHeight(
            parley::LineHeight::FontSizeRelative(1.6),
        ));

        let mut font_cx = FontContext::default();
        configure_han_fallback(&mut font_cx);

        let mut surface = Self {
            font_cx,
            layout_cx: LayoutContext::default(),
            editor,
            resources: Resources::new(),
            ctx: RenderContext::new(1, 1),
            pixmap: Pixmap::new(1, 1),
            width: 0,
            height: 0,
            scale: 1.0,
            zoom: 1.0,
            scroll: 0.0,
            caret_on: true,
            palette,
            page: PageLayout::default(),
            decorations: Vec::new(),
        };
        surface.driver().move_to_text_end();
        surface
    }

    fn driver(&mut self) -> PlainEditorDriver<'_, Brush> {
        self.editor.driver(&mut self.font_cx, &mut self.layout_cx)
    }

    /// Physical pixels per logical pixel of page content: display scale times zoom.
    fn content_scale(&self) -> f64 {
        f64::from(self.scale) * f64::from(self.zoom)
    }

    /// Horizontal placement, which does not depend on the text:
    /// `(page_x, page_width, padding)` in physical pixels.
    fn columns(&self) -> (f64, f64, f64) {
        let s = self.content_scale();
        let width = f64::from(self.width);
        let margin = self.page.margin * s;
        let available = (width - 2.0 * margin).max(1.0);
        let page_width = match self.page.max_width {
            Some(max) => (max * s).min(available),
            None => available,
        };
        let page_x = ((width - page_width) / 2.0).floor();
        let padding = (self.page.padding * s).min(page_width / 8.0);
        (page_x, page_width, padding)
    }

    fn geometry(&mut self) -> Geometry {
        let s = self.content_scale();
        let margin = self.page.margin * s;
        let (page_x, page_width, padding) = self.columns();
        let text_height = f64::from(
            self.editor
                .layout(&mut self.font_cx, &mut self.layout_cx)
                .height(),
        );
        let height = f64::from(self.height);
        let min_height = match self.page.min_aspect {
            Some(aspect) => page_width * aspect,
            None => height - 2.0 * margin,
        };
        let page_height = (text_height + 2.0 * padding).max(min_height).max(1.0);
        let max_scroll = (page_height + 2.0 * margin - height).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        let page_y = margin - self.scroll;
        Geometry {
            page_x,
            page_y,
            page_width,
            page_height,
            text_x: page_x + padding,
            text_y: page_y + padding,
            max_scroll,
        }
    }

    /// Push the current size and zoom into the editor.
    fn relayout(&mut self) {
        self.editor.set_scale(self.scale * self.zoom);
        let (_, page_width, padding) = self.columns();
        let text_width = page_width - 2.0 * padding;
        self.editor.set_width(Some(text_width.max(1.0) as f32));
    }

    /// Resize the backing buffer. Sizes are physical pixels.
    pub fn resize(&mut self, width: u16, height: u16, scale: f32) {
        if (width, height) == (self.width, self.height) && scale == self.scale {
            return;
        }
        self.width = width;
        self.height = height;
        self.scale = scale;
        self.ctx = RenderContext::new(width, height);
        self.pixmap = Pixmap::new(width, height);
        self.relayout();
    }

    pub fn size(&self) -> (u16, u16, f32) {
        (self.width, self.height, self.scale)
    }

    pub fn set_palette(&mut self, palette: Palette) {
        if palette == self.palette {
            return;
        }
        self.palette = palette;
        self.editor
            .edit_styles()
            .insert(StyleProperty::Brush(Brush(color(palette.ink))));
    }

    pub fn set_page_layout(&mut self, page: PageLayout) {
        if page == self.page {
            return;
        }
        self.page = page;
        self.relayout();
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Set the zoom factor, 1.0 being 100 %. Returns the value actually used.
    pub fn set_zoom(&mut self, zoom: f32) -> f32 {
        let zoom = zoom.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
        if zoom != self.zoom {
            // Keep the same part of the document at the top of the view.
            self.scroll *= f64::from(zoom / self.zoom);
            self.zoom = zoom;
            self.relayout();
            self.reveal_caret();
        }
        zoom
    }

    /// Scroll by a distance in physical pixels; positive moves down the document.
    /// Returns whether the view moved.
    pub fn scroll_by(&mut self, delta: f64) -> bool {
        let before = self.scroll;
        self.scroll += delta;
        self.geometry();
        self.scroll != before
    }

    /// Scroll offset and its upper bound, in physical pixels.
    pub fn scroll_range(&mut self) -> (f64, f64) {
        let geometry = self.geometry();
        (self.scroll, geometry.max_scroll)
    }

    /// Scroll just far enough to bring the caret, or the composition text, into
    /// the view.
    pub fn reveal_caret(&mut self) {
        if self.height == 0 {
            return;
        }
        let geometry = self.geometry();
        let area = match self.editor.cursor_geometry(CARET_WIDTH * self.scale) {
            Some(caret) => caret,
            None => self.editor.ime_cursor_area(),
        };
        let height = f64::from(self.height);
        let keep = (CARET_SCROLL_MARGIN * f64::from(self.scale)).min(height / 4.0);
        let top = geometry.text_y + area.y0;
        let bottom = geometry.text_y + area.y1;
        if top < keep {
            self.scroll -= keep - top;
        } else if bottom > height - keep {
            self.scroll += bottom - (height - keep);
        }
        self.scroll = self.scroll.clamp(0.0, geometry.max_scroll);
    }

    /// Change the body text size, in logical pixels at 100 % zoom.
    pub fn set_font_size(&mut self, size: f32) {
        self.editor
            .edit_styles()
            .insert(StyleProperty::FontSize(size));
    }

    // The next five methods are for a host that keeps the text in a document
    // model of its own and uses this surface as the view: the model decides
    // what an edit does, and the surface is told to show the result. Such a
    // host does not call `ime` or the text-changing variants of `apply`.

    /// The selection as byte offsets into the committed text: where it was
    /// started and where its moving end, the caret, is. Meaningless while a
    /// composition is in progress.
    pub fn selection(&self) -> (usize, usize) {
        let selection = self.editor.raw_selection();
        (selection.anchor().index(), selection.focus().index())
    }

    /// Put the selection at byte offsets into the committed text. Offsets that
    /// are not on a character boundary are ignored.
    pub fn set_selection(&mut self, anchor: usize, focus: usize) {
        if self.editor.is_composing() {
            return;
        }
        let mut drv = self.driver();
        drv.move_to_byte(anchor);
        if focus != anchor {
            drv.extend_selection_to_byte(focus);
        }
        self.caret_on = true;
    }

    /// Replace a byte range of the committed text, as an edit made in the
    /// host's model. Ends any composition first. The caret is left after the
    /// new text; the host normally follows up with `set_selection`.
    pub fn replace_range(&mut self, range: std::ops::Range<usize>, text: &str) {
        let mut drv = self.driver();
        drv.clear_compose();
        drv.select_byte_range(range.start, range.end);
        if text.is_empty() {
            drv.delete_selection();
        } else {
            drv.insert_or_replace_selection(text);
        }
        self.caret_on = true;
    }

    /// Show input-method composition text at the caret. `cursor` is a byte range
    /// inside `text`; `None` hides the caret. Empty text ends the composition.
    pub fn compose(&mut self, text: &str, cursor: Option<(usize, usize)>) {
        if text.is_empty() {
            self.driver().clear_compose();
        } else {
            self.driver().set_compose(text, cursor);
        }
        self.caret_on = true;
        self.reveal_caret();
    }

    /// Drop the composition text, if any. The committed text is untouched.
    pub fn end_composition(&mut self) {
        self.driver().clear_compose();
        self.caret_on = true;
    }

    /// The committed text. Composition text is not part of it.
    pub fn text(&self) -> String {
        self.editor.text().to_string()
    }

    pub fn is_composing(&self) -> bool {
        self.editor.is_composing()
    }

    pub fn set_caret_on(&mut self, on: bool) {
        self.caret_on = on;
    }

    pub fn ime(&mut self, event: ImeEvent) {
        match event {
            ImeEvent::Enabled => {}
            ImeEvent::Preedit { text, cursor } => {
                if text.is_empty() {
                    self.driver().clear_compose();
                } else {
                    self.driver().set_compose(&text, cursor);
                }
            }
            ImeEvent::Commit(text) => {
                // Some platforms commit without first sending an empty preedit.
                if self.editor.is_composing() {
                    self.driver().clear_compose();
                }
                self.driver().insert_or_replace_selection(&text);
            }
            ImeEvent::Disabled => self.driver().clear_compose(),
        }
        self.caret_on = true;
        self.reveal_caret();
    }

    pub fn apply(&mut self, action: Action) {
        // While the input method is composing it owns the keyboard.
        if self.editor.is_composing() {
            return;
        }
        let mut drv = self.driver();
        match action {
            Action::Insert(text) => drv.insert_or_replace_selection(&text),
            Action::Newline | Action::PlainNewline => drv.insert_or_replace_selection("\n"),
            Action::Indent => drv.insert_or_replace_selection("\t"),
            Action::Outdent => {}
            Action::Backspace => drv.backdelete(),
            Action::Delete => drv.delete(),
            Action::Left {
                select: false,
                word: false,
            } => drv.move_left(),
            Action::Left {
                select: true,
                word: false,
            } => drv.select_left(),
            Action::Left {
                select: false,
                word: true,
            } => drv.move_word_left(),
            Action::Left {
                select: true,
                word: true,
            } => drv.select_word_left(),
            Action::Right {
                select: false,
                word: false,
            } => drv.move_right(),
            Action::Right {
                select: true,
                word: false,
            } => drv.select_right(),
            Action::Right {
                select: false,
                word: true,
            } => drv.move_word_right(),
            Action::Right {
                select: true,
                word: true,
            } => drv.select_word_right(),
            Action::Up { select: false } => drv.move_up(),
            Action::Up { select: true } => drv.select_up(),
            Action::Down { select: false } => drv.move_down(),
            Action::Down { select: true } => drv.select_down(),
            Action::LineStart { select: false } => drv.move_to_line_start(),
            Action::LineStart { select: true } => drv.select_to_line_start(),
            Action::LineEnd { select: false } => drv.move_to_line_end(),
            Action::LineEnd { select: true } => drv.select_to_line_end(),
            Action::SelectAll => drv.select_all(),
        }
        self.caret_on = true;
        self.reveal_caret();
    }

    /// Pointer press at a position in physical pixels relative to the surface.
    pub fn pointer_down(&mut self, x: f64, y: f64, extend: bool) {
        if self.editor.is_composing() {
            return;
        }
        let geometry = self.geometry();
        let (x, y) = ((x - geometry.text_x) as f32, (y - geometry.text_y) as f32);
        let mut drv = self.driver();
        if extend {
            drv.shift_click_extension(x, y);
        } else {
            drv.move_to_point(x, y);
        }
        self.caret_on = true;
    }

    /// Pointer drag with the button held, in physical pixels relative to the surface.
    pub fn pointer_drag(&mut self, x: f64, y: f64) {
        if self.editor.is_composing() {
            return;
        }
        let geometry = self.geometry();
        self.driver()
            .extend_selection_to_point((x - geometry.text_x) as f32, (y - geometry.text_y) as f32);
        self.caret_on = true;
    }

    /// Where the platform should put the candidate window: `(x, y, width, height)`
    /// in physical pixels relative to the surface. It covers the composition text
    /// when there is one, otherwise the caret.
    pub fn ime_cursor_area(&mut self) -> (f64, f64, f64, f64) {
        // Make sure the layout reflects the latest edit before measuring it.
        let geometry = self.geometry();
        let area = self.editor.ime_cursor_area();
        (
            area.x0 + geometry.text_x,
            area.y0 + geometry.text_y,
            (area.x1 - area.x0).max(1.0),
            (area.y1 - area.y0).max(1.0),
        )
    }

    /// Styles to draw ranges of the committed text with, in order and not
    /// overlapping. They are kept until replaced; the host replaces them
    /// after every edit, normally for [`Self::visible_range`] only.
    pub fn set_decorations(&mut self, decorations: Vec<Decoration>) {
        debug_assert!(
            decorations
                .windows(2)
                .all(|pair| pair[0].range.end <= pair[1].range.start)
        );
        self.decorations = decorations;
    }

    /// The committed text on the lines inside the view, rounded out to whole
    /// lines.
    pub fn visible_range(&mut self) -> Range<usize> {
        let geometry = self.geometry();
        let top = -geometry.text_y as f32;
        let bottom = top + f32::from(self.height);
        let compose = self.editor.raw_compose().clone();
        let layout = self.editor.layout(&mut self.font_cx, &mut self.layout_cx);
        let first = Cursor::from_point(layout, 0.0, top).index();
        let last = Cursor::from_point(layout, 0.0, bottom).index();
        let start = layout
            .get(line_index_of(layout, first))
            .map_or(0, |line| line.text_range().start);
        let end = layout
            .get(line_index_of(layout, last))
            .map_or(start, |line| line.text_range().end);
        let start = committed_offset(&compose, start)
            .unwrap_or_else(|| compose.clone().map_or(0, |c| c.start));
        let end = committed_offset(&compose, end)
            .unwrap_or_else(|| compose.clone().map_or(0, |c| c.start));
        start..end.max(start)
    }

    /// Which part of the text is at the top edge of the view, as a byte
    /// offset into the committed text with a fraction for how far down its
    /// line the edge is. For keeping another view at the same place.
    pub fn top_position(&mut self) -> f64 {
        let geometry = self.geometry();
        let top = (-geometry.text_y).max(0.0);
        let compose = self.editor.raw_compose().clone();
        let layout = self.editor.layout(&mut self.font_cx, &mut self.layout_cx);
        let index = Cursor::from_point(layout, 0.0, top as f32).index();
        let Some(line) = layout.get(line_index_of(layout, index)) else {
            return 0.0;
        };
        let range = line.text_range();
        let metrics = line.metrics();
        let height = f64::from(metrics.block_max_coord - metrics.block_min_coord).max(1.0);
        let fraction = ((top - f64::from(metrics.block_min_coord)) / height).clamp(0.0, 1.0);
        let start = committed_offset(&compose, range.start).unwrap_or(range.start) as f64;
        let end = committed_offset(&compose, range.end).unwrap_or(range.end) as f64;
        start + fraction * (end - start)
    }

    /// Scroll so that a position from [`Self::top_position`] is at the top
    /// edge of the view.
    pub fn scroll_to_position(&mut self, position: f64) {
        let margin = self.page.margin * self.content_scale();
        let (_, _, padding) = self.columns();
        let compose = self.editor.raw_compose().clone();
        let layout = self.editor.layout(&mut self.font_cx, &mut self.layout_cx);
        let offset = position.max(0.0) as usize;
        let raw = raw_offset(&compose, offset).min(layout_text_len(layout));
        let Some(line) = layout.get(line_index_of(layout, raw)) else {
            return;
        };
        let range = line.text_range();
        let metrics = line.metrics();
        let start = committed_offset(&compose, range.start).unwrap_or(range.start) as f64;
        let end = committed_offset(&compose, range.end).unwrap_or(range.end) as f64;
        let fraction = if end > start {
            ((position - start) / (end - start)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let height = f64::from(metrics.block_max_coord - metrics.block_min_coord);
        let y = f64::from(metrics.block_min_coord) + fraction * height;
        self.scroll = margin + padding + y;
        // Clamp.
        self.geometry();
    }

    /// Draw the current state into the backing buffer and return it.
    /// Pixels are premultiplied RGBA8.
    pub fn render(&mut self) -> &Pixmap {
        let geometry = self.geometry();
        let (_, page_width, padding) = self.columns();
        let text_width = page_width - 2.0 * padding;
        let Self {
            font_cx,
            layout_cx,
            editor,
            resources,
            ctx,
            pixmap,
            width,
            height,
            scale,
            caret_on,
            palette,
            decorations,
            ..
        } = self;
        let (w, h) = (f64::from(*width), f64::from(*height));

        ctx.reset();
        ctx.set_fill_rule(Fill::NonZero);
        ctx.set_transform(Affine::IDENTITY);
        ctx.set_paint(color(palette.desk));
        ctx.fill_rect(&Rect::new(0.0, 0.0, w, h));

        let page = Rect::new(
            geometry.page_x,
            geometry.page_y,
            geometry.page_x + geometry.page_width,
            geometry.page_y + geometry.page_height,
        );
        if palette.page_edge[3] != 0 {
            let edge = f64::from(*scale).round().max(1.0);
            ctx.set_paint(color(palette.page_edge));
            ctx.fill_rect(&page.inflate(edge, edge));
        }
        ctx.set_paint(color(palette.page));
        ctx.fill_rect(&page);

        ctx.set_transform(Affine::translate((geometry.text_x, geometry.text_y)));

        // Lines outside the view are skipped; the range is in layout coordinates.
        let visible = (-geometry.text_y, h - geometry.text_y);
        let compose = editor.raw_compose().clone();
        let layout = editor.layout(font_cx, layout_cx);
        let first_line = line_index_of(
            layout,
            Cursor::from_point(layout, 0.0, visible.0 as f32).index(),
        );
        let lines = || {
            (first_line..layout.len())
                .filter_map(|index| layout.get(index))
                .take_while(|line| f64::from(line.metrics().block_min_coord) <= visible.1)
        };

        // Backgrounds first, so the selection and the text go over them.
        if !decorations.is_empty() {
            let inset =
                (f64::from(CODE_INSET) * f64::from(*scale)).min(geometry.text_x - geometry.page_x);
            for line in lines() {
                let metrics = line.metrics();
                let (top, bottom) = (
                    f64::from(metrics.block_min_coord),
                    f64::from(metrics.block_max_coord),
                );
                let range = line.text_range();
                let start = committed_offset(&compose, range.start).unwrap_or(range.start);
                let end = committed_offset(&compose, range.end).unwrap_or(range.end);
                if let Some(background) = line_background_in(decorations, start..end.max(start + 1))
                {
                    ctx.set_paint(color(background));
                    ctx.fill_rect(&Rect::new(-inset, top, text_width + inset, bottom));
                }
                let pad = (bottom - top) * 0.12;
                for_each_cluster(&line, |cluster, x0, x1| {
                    let Some(offset) = committed_offset(&compose, cluster.text_range().start)
                    else {
                        return;
                    };
                    if let Some(background) = style_at(decorations, offset).background {
                        let radius = 3.0 * f64::from(*scale);
                        let rect = RoundedRect::new(x0, top + pad, x1, bottom - pad, radius);
                        ctx.set_paint(color(background));
                        ctx.fill_path(&rect.to_path(0.1));
                    }
                });
            }
        }

        ctx.set_paint(color(palette.selection));
        editor.selection_geometry_with(|rect, _| {
            ctx.fill_rect(&Rect::new(rect.x0, rect.y0, rect.x1, rect.y1));
        });

        let layout = editor.layout(font_cx, layout_cx);
        let lines = (first_line..layout.len())
            .filter_map(|index| layout.get(index))
            .take_while(|line| f64::from(line.metrics().block_min_coord) <= visible.1);
        let mut glyph_buffer: Vec<Glyph> = Vec::new();
        for line in lines {
            let mut previous_run: Option<Range<usize>> = None;
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                    continue;
                };
                let style = glyph_run.style();
                let run = glyph_run.run();

                // Parley underlines the composition range; this is what makes the
                // preedit text visibly different from committed text.
                if let Some(underline) = &style.underline {
                    let metrics = run.font_metrics();
                    let offset = underline.offset.unwrap_or(metrics.underline_offset);
                    let size = underline.size.unwrap_or(metrics.underline_size);
                    let x0 = f64::from(glyph_run.offset());
                    let x1 = x0 + f64::from(glyph_run.advance());
                    glyphs::line(
                        ctx,
                        underline.brush.0,
                        (x0, x1),
                        glyph_run.baseline(),
                        offset,
                        size,
                        *scale,
                    );
                }

                if decorations.is_empty() {
                    glyph_buffer.clear();
                    glyph_buffer.extend(glyph_run.positioned_glyphs().map(|glyph| Glyph {
                        id: glyph.id,
                        x: glyph.x,
                        y: glyph.y,
                    }));
                    let paint = GlyphPaint {
                        color: style.brush.0,
                        embolden: false,
                        skew: None,
                    };
                    glyphs::draw(ctx, resources, run, &glyph_buffer, paint);
                    continue;
                }

                // With decorations the whole run is drawn at once, cluster by
                // cluster, since its glyph runs only follow Parley's own
                // styles. The composition splits a run into several.
                if previous_run.as_ref() == Some(&run.text_range()) {
                    continue;
                }
                previous_run = Some(run.text_range());
                draw_decorated_run(
                    ctx,
                    resources,
                    run,
                    glyph_run.offset(),
                    glyph_run.baseline(),
                    style.brush.0,
                    &|raw| committed_offset(&compose, raw),
                    decorations,
                    *scale,
                    &mut glyph_buffer,
                );
            }
        }

        if *caret_on && let Some(caret) = editor.cursor_geometry(CARET_WIDTH * *scale) {
            ctx.set_paint(color(palette.caret));
            ctx.fill_rect(&Rect::new(caret.x0, caret.y0, caret.x1, caret.y1));
        }

        ctx.flush();
        ctx.render(&mut *pixmap, resources);
        pixmap
    }

    /// Number of glyphs in the current layout that the chosen fonts could not map.
    #[cfg(all(test, target_os = "macos"))]
    fn missing_glyphs(&mut self) -> usize {
        let layout = self.editor.layout(&mut self.font_cx, &mut self.layout_cx);
        let mut missing = 0;
        for line in layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    missing += run.glyphs().filter(|glyph| glyph.id == 0).count();
                }
            }
        }
        missing
    }

    /// Render and encode as PNG. Used by `--dump` and by nothing on the hot path.
    pub fn encode_png(&mut self) -> Result<Vec<u8>, String> {
        let (width, height) = (self.width, self.height);
        let data = self.render().data().to_vec();
        Pixmap::from_parts(data, width, height)
            .into_png()
            .map_err(|err| err.to_string())
    }
}

/// Horizontal extra of a code block's background on each side of the text
/// column, in logical pixels.
pub(crate) const CODE_INSET: f32 = 12.0;

fn layout_text_len(layout: &Layout<Brush>) -> usize {
    layout
        .len()
        .checked_sub(1)
        .and_then(|last| layout.get(last))
        .map_or(0, |line| line.text_range().end)
}

/// Call `f` with every cluster of a line and its left and right edge.
pub(crate) fn for_each_cluster(
    line: &parley::Line<'_, Brush>,
    mut f: impl FnMut(&parley::Cluster<'_, Brush>, f64, f64),
) {
    let mut previous_run: Option<Range<usize>> = None;
    for item in line.items() {
        let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
            continue;
        };
        let run = glyph_run.run();
        if previous_run.as_ref() == Some(&run.text_range()) {
            continue;
        }
        previous_run = Some(run.text_range());
        let mut x = f64::from(glyph_run.offset());
        for cluster in run.visual_clusters() {
            let advance = f64::from(cluster.advance());
            f(&cluster, x, x + advance);
            x += advance;
        }
    }
}

/// Draw one run with the decorations that cover its clusters: consecutive
/// clusters of the same style become one call.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_decorated_run(
    ctx: &mut RenderContext,
    resources: &mut Resources,
    run: &parley::Run<'_, Brush>,
    offset: f32,
    baseline: f32,
    ink: Color,
    committed: &dyn Fn(usize) -> Option<usize>,
    decorations: &[Decoration],
    scale: f32,
    buffer: &mut Vec<Glyph>,
) {
    let metrics = *run.font_metrics();
    let flush = |ctx: &mut RenderContext,
                 resources: &mut Resources,
                 buffer: &mut Vec<Glyph>,
                 style: TextStyle,
                 x: (f64, f64)| {
        let paint = GlyphPaint {
            color: style.color.map_or(ink, color),
            embolden: style.bold,
            skew: style.italic.then_some(SYNTHETIC_SKEW_DEGREES),
        };
        glyphs::draw(ctx, resources, run, buffer, paint);
        buffer.clear();
        if style.underline {
            glyphs::line(
                ctx,
                paint.color,
                x,
                baseline,
                metrics.underline_offset,
                metrics.underline_size,
                scale,
            );
        }
        if style.strike {
            glyphs::line(
                ctx,
                paint.color,
                x,
                baseline,
                metrics.strikethrough_offset,
                metrics.strikethrough_size,
                scale,
            );
        }
    };

    buffer.clear();
    let mut x = offset;
    let mut current = TextStyle::default();
    let mut from = f64::from(x);
    for cluster in run.visual_clusters() {
        let style = committed(cluster.text_range().start)
            .map_or_else(TextStyle::default, |at| style_at(decorations, at));
        if style != current {
            flush(ctx, resources, buffer, current, (from, f64::from(x)));
            current = style;
            from = f64::from(x);
        }
        for glyph in cluster.glyphs() {
            buffer.push(Glyph {
                id: glyph.id,
                x: x + glyph.x,
                y: baseline + glyph.y,
            });
            x += glyph.advance;
        }
    }
    flush(ctx, resources, buffer, current, (from, f64::from(x)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface(text: &str) -> TextSurface {
        let mut s = TextSurface::new(text);
        s.resize(800, 400, 2.0);
        s
    }

    fn preedit(text: &str) -> ImeEvent {
        ImeEvent::Preedit {
            text: text.to_owned(),
            cursor: Some((text.len(), text.len())),
        }
    }

    /// RGBA of the pixel at a position in physical pixels.
    fn pixel(s: &mut TextSurface, x: usize, y: usize) -> [u8; 4] {
        let width = usize::from(s.size().0);
        let data = s.render().data_as_u8_slice();
        let at = (y * width + x) * 4;
        [data[at], data[at + 1], data[at + 2], data[at + 3]]
    }

    #[test]
    fn preedit_is_not_committed_text() {
        let mut s = surface("ab");
        s.ime(preedit("ni"));
        assert!(s.is_composing());
        assert_eq!(s.text(), "ab");
    }

    #[test]
    fn commit_after_cleared_preedit_inserts_once() {
        let mut s = surface("ab");
        s.ime(preedit("ni"));
        s.ime(preedit("nihao"));
        s.ime(ImeEvent::Preedit {
            text: String::new(),
            cursor: None,
        });
        s.ime(ImeEvent::Commit("你好".to_owned()));
        assert!(!s.is_composing());
        assert_eq!(s.text(), "ab你好");
    }

    #[test]
    fn commit_without_cleared_preedit_does_not_duplicate() {
        let mut s = surface("");
        s.ime(preedit("にほんご"));
        s.ime(ImeEvent::Commit("日本語".to_owned()));
        assert!(!s.is_composing());
        assert_eq!(s.text(), "日本語");
    }

    #[test]
    fn disabling_the_input_method_drops_the_preedit() {
        let mut s = surface("x");
        s.ime(preedit("zhong"));
        s.ime(ImeEvent::Disabled);
        assert!(!s.is_composing());
        assert_eq!(s.text(), "x");
    }

    #[test]
    fn keys_are_ignored_while_composing() {
        let mut s = surface("ab");
        s.ime(preedit("ni"));
        s.apply(Action::Backspace);
        s.apply(Action::Insert("z".to_owned()));
        assert_eq!(s.text(), "ab");
        assert!(s.is_composing());
    }

    #[test]
    fn candidate_area_follows_the_caret() {
        let mut s = surface("");
        let (x0, y0, _, h0) = s.ime_cursor_area();
        s.apply(Action::Insert("hello".to_owned()));
        let (x1, y1, _, _) = s.ime_cursor_area();
        assert!(x1 > x0, "caret did not advance: {x0} -> {x1}");
        assert_eq!(y0, y1);
        assert!(h0 > 1.0);

        s.apply(Action::Newline);
        let (_, y2, _, _) = s.ime_cursor_area();
        assert!(y2 > y1, "caret did not move down: {y1} -> {y2}");
    }

    #[test]
    fn candidate_area_covers_the_preedit() {
        let mut s = surface("");
        let (_, _, caret_width, _) = s.ime_cursor_area();
        s.ime(preedit("nihao"));
        let (_, _, preedit_width, _) = s.ime_cursor_area();
        assert!(preedit_width > caret_width);
    }

    // Simplified-only, Traditional-only and Japanese-only characters side by
    // side. With a single cached Han fallback some of these have no glyph.
    #[cfg(target_os = "macos")]
    #[test]
    fn mixed_cjk_text_has_no_missing_glyphs() {
        let mut s =
            surface("这张图请输入 這張圖請輸入 図書館で読む にほんご カタカナ 한국어 English");
        assert_eq!(s.missing_glyphs(), 0);
    }

    #[test]
    fn text_is_painted() {
        let mut s = surface("");
        let blank = s.render().data().to_vec();
        s.apply(Action::Insert("Aster".to_owned()));
        s.set_caret_on(false);
        let inked = s.render().data().to_vec();
        let changed = blank.iter().zip(&inked).filter(|(a, b)| a != b).count();
        assert!(changed > 100, "only {changed} pixels changed");
    }

    #[test]
    fn a_host_can_read_and_place_the_selection() {
        let mut s = surface("中文 text");
        // The caret starts at the end.
        assert_eq!(s.selection(), (11, 11));
        s.set_selection(0, 6);
        assert_eq!(s.selection(), (0, 6));
        // A backwards selection keeps its direction.
        s.set_selection(6, 3);
        assert_eq!(s.selection(), (6, 3));
        s.apply(Action::Left {
            select: false,
            word: false,
        });
        let (anchor, focus) = s.selection();
        assert_eq!(anchor, focus);
    }

    #[test]
    fn a_host_can_replace_ranges_of_the_text() {
        let mut s = surface("hello world");
        s.replace_range(6..11, "世界");
        assert_eq!(s.text(), "hello 世界");
        assert_eq!(s.selection(), (12, 12));
        s.replace_range(0..5, "");
        assert_eq!(s.text(), " 世界");
        s.replace_range(0..0, "你好");
        assert_eq!(s.text(), "你好 世界");
    }

    #[test]
    fn host_driven_composition_never_touches_the_committed_text() {
        let mut s = surface("ab");
        s.compose("ni", Some((2, 2)));
        assert!(s.is_composing());
        assert_eq!(s.text(), "ab");
        s.compose("", None);
        assert!(!s.is_composing());
        s.compose("nihao", Some((5, 5)));
        s.end_composition();
        assert!(!s.is_composing());
        assert_eq!(s.text(), "ab");
        // Replacing a range ends a composition rather than mixing with it.
        s.compose("x", None);
        s.replace_range(2..2, "你好");
        assert!(!s.is_composing());
        assert_eq!(s.text(), "ab你好");
    }

    #[test]
    fn the_text_size_can_be_changed() {
        let mut s = surface("hello");
        let (_, _, _, before) = s.ime_cursor_area();
        s.set_font_size(FONT_SIZE / 2.0);
        let (_, _, _, after) = s.ime_cursor_area();
        assert!(after < before * 0.7, "caret height {before} -> {after}");
    }

    #[test]
    fn short_text_does_not_scroll() {
        let mut s = surface("one line");
        assert!(!s.scroll_by(500.0));
        assert_eq!(s.scroll_range(), (0.0, 0.0));
    }

    #[test]
    fn typing_past_the_bottom_scrolls_the_caret_into_view() {
        let mut s = surface("");
        for _ in 0..40 {
            s.apply(Action::Insert("line".to_owned()));
            s.apply(Action::Newline);
        }
        let (scroll, max) = s.scroll_range();
        assert!(max > 0.0);
        assert!(scroll > 0.0, "view did not follow the caret");
        let (_, y, _, h) = s.ime_cursor_area();
        assert!(y >= 0.0 && y + h <= 400.0, "caret at {y}, outside the view");
    }

    #[test]
    fn scrolling_is_clamped_to_the_page() {
        let mut s = surface(&"line\n".repeat(60));
        let (_, max) = s.scroll_range();
        assert!(max > 0.0);
        assert!(s.scroll_by(1.0e6));
        assert_eq!(s.scroll_range().0, max);
        assert!(!s.scroll_by(10.0));
        assert!(s.scroll_by(-1.0e6));
        assert_eq!(s.scroll_range().0, 0.0);
        assert!(!s.scroll_by(-10.0));
    }

    #[test]
    fn a_click_lands_on_the_same_text_after_scrolling() {
        let mut s = surface(&"line\n".repeat(60));
        s.scroll_by(-1.0e6);
        // Put the caret on a visible line, scroll, and click where that line
        // now is: the caret must be on the same text, 30 pixels further up.
        s.pointer_down(200.0, 120.0, false);
        let (x0, y0, _, _) = s.ime_cursor_area();
        assert!(s.scroll_by(30.0));
        s.pointer_down(200.0, 120.0 - 30.0, false);
        let (x1, y1, _, _) = s.ime_cursor_area();
        assert_eq!(x0, x1);
        assert_eq!(y0 - 30.0, y1);
    }

    #[test]
    fn zoom_scales_the_text_and_is_clamped() {
        let mut s = surface("hello");
        let (_, _, _, h1) = s.ime_cursor_area();
        assert_eq!(s.set_zoom(2.0), 2.0);
        let (_, _, _, h2) = s.ime_cursor_area();
        assert!(
            (h2 / h1 - 2.0).abs() < 0.1,
            "caret height {h1} -> {h2} at 200 %"
        );
        assert_eq!(s.set_zoom(100.0), ZOOM_RANGE.1);
        assert_eq!(s.set_zoom(0.0), ZOOM_RANGE.0);
    }

    #[test]
    fn a_capped_page_is_centred() {
        let mut s = TextSurface::new("");
        s.set_page_layout(PageLayout {
            margin: 10.0,
            padding: 20.0,
            max_width: Some(200.0),
            min_aspect: None,
        });
        s.set_caret_on(false);
        s.resize(800, 400, 1.0);
        let palette = Palette::default();
        // Page from x = 300 to 500; desk on both sides.
        assert_eq!(pixel(&mut s, 150, 200), palette.desk);
        assert_eq!(pixel(&mut s, 400, 200), palette.page);
        assert_eq!(pixel(&mut s, 650, 200), palette.desk);
    }

    #[test]
    fn the_palette_recolours_page_and_text() {
        let mut s = surface("");
        let dark = Palette {
            desk: [0x10, 0x10, 0x10, 0xff],
            page: [0x20, 0x20, 0x20, 0xff],
            page_edge: [0x40, 0x40, 0x40, 0xff],
            ink: [0xf0, 0xf0, 0xf0, 0xff],
            selection: [0x50, 0x40, 0x90, 0xff],
            caret: [0x7c, 0x5c, 0xe0, 0xff],
        };
        s.set_palette(dark);
        s.set_caret_on(false);
        assert_eq!(pixel(&mut s, 2, 2), dark.desk);
        assert_eq!(pixel(&mut s, 400, 300), dark.page);

        // Plain capitals: every system font has them, and their stems are wide
        // enough to hold pixels of the pure ink colour.
        s.apply(Action::Insert("HHHHHHHH".to_owned()));
        s.set_caret_on(false);
        let data = s.render().data_as_u8_slice();
        let (pixels, _) = data.as_chunks::<4>();
        let ink = pixels.iter().filter(|px| **px == dark.ink).count();
        assert!(ink > 50, "only {ink} pixels in the ink colour");
    }

    fn count(s: &mut TextSurface, rgba: [u8; 4]) -> usize {
        let data = s.render().data_as_u8_slice();
        let (pixels, _) = data.as_chunks::<4>();
        pixels.iter().filter(|px| **px == rgba).count()
    }

    const GREY: [u8; 4] = [0x80, 0x80, 0x80, 0xff];
    const TINT: [u8; 4] = [0xe8, 0xe8, 0xe8, 0xff];

    fn styled(range: Range<usize>, style: TextStyle) -> Vec<Decoration> {
        vec![Decoration { range, style }]
    }

    #[test]
    fn decorations_never_move_text_or_caret() {
        let text = "# 标题 **粗体** *斜体* `code`\n第二行";
        let mut s = surface(text);
        s.set_selection(2, 11);
        let caret = s.ime_cursor_area();
        let visible = s.visible_range();
        s.set_decorations(vec![
            Decoration {
                range: 0..10,
                style: TextStyle {
                    bold: true,
                    italic: true,
                    underline: true,
                    strike: true,
                    color: Some(GREY),
                    background: Some(TINT),
                    line_background: Some(TINT),
                },
            },
            Decoration {
                range: 20..text.len(),
                style: TextStyle {
                    bold: true,
                    ..TextStyle::default()
                },
            },
        ]);
        s.render();
        assert_eq!(s.ime_cursor_area(), caret);
        assert_eq!(s.selection(), (2, 11));
        assert_eq!(s.visible_range(), visible);
        assert_eq!(s.text(), text);
    }

    #[test]
    fn a_decoration_recolours_its_range_only() {
        let mut s = surface("HHHHHHHH");
        s.set_caret_on(false);
        let ink = Palette::default().ink;
        let plain_ink = count(&mut s, ink);
        assert!(plain_ink > 50);
        s.set_decorations(styled(
            0..4,
            TextStyle {
                color: Some(GREY),
                ..TextStyle::default()
            },
        ));
        let grey = count(&mut s, GREY);
        let ink_left = count(&mut s, ink);
        assert!(grey > 20, "{grey} grey pixels");
        assert!(
            ink_left > 20 && ink_left < plain_ink,
            "{ink_left} of {plain_ink}"
        );
    }

    #[test]
    fn bold_is_heavier_and_a_line_background_spans_the_column() {
        let mut s = surface("HHHH\nHHHH");
        s.set_caret_on(false);
        let ink = Palette::default().ink;
        let regular = count(&mut s, ink);
        s.set_decorations(styled(
            0..4,
            TextStyle {
                bold: true,
                ..TextStyle::default()
            },
        ));
        assert!(count(&mut s, ink) > regular, "bold is not heavier");

        s.set_decorations(styled(
            5..9,
            TextStyle {
                line_background: Some(TINT),
                ..TextStyle::default()
            },
        ));
        // The second line's band reaches the right of the text column.
        let (_, y, _, height) = {
            s.set_selection(6, 6);
            s.ime_cursor_area()
        };
        let row = (y + height / 2.0) as usize;
        assert_eq!(pixel(&mut s, 700, row), TINT);
        let (_, y0, _, _) = {
            s.set_selection(1, 1);
            s.ime_cursor_area()
        };
        assert_ne!(pixel(&mut s, 700, (y0 + 2.0) as usize), TINT);
    }

    #[test]
    fn decorations_stay_on_committed_text_while_composing() {
        let compose = Some(2..5);
        assert_eq!(committed_offset(&compose, 1), Some(1));
        assert_eq!(committed_offset(&compose, 3), None);
        assert_eq!(committed_offset(&compose, 5), Some(2));
        assert_eq!(raw_offset(&compose, 1), 1);
        assert_eq!(raw_offset(&compose, 2), 5);
        assert_eq!(committed_offset(&None, 7), Some(7));

        let mut s = surface("ab**cd**");
        s.set_selection(2, 2);
        s.compose("中文", Some((6, 6)));
        s.set_decorations(styled(
            2..8,
            TextStyle {
                bold: true,
                color: Some(GREY),
                ..TextStyle::default()
            },
        ));
        s.render();
        assert_eq!(s.text(), "ab**cd**");
        assert!(s.is_composing());
    }

    #[test]
    fn the_top_position_round_trips() {
        let text: String = (0..200).map(|i| format!("第 {i} 行 line\n")).collect();
        let mut s = surface(&text);
        s.set_selection(0, 0);
        s.reveal_caret();
        assert_eq!(s.top_position(), 0.0);
        let line_100 = text.find("第 100 行").unwrap();
        s.scroll_to_position(line_100 as f64);
        let top = s.top_position();
        assert!((top - line_100 as f64).abs() < 2.0, "{top} vs {line_100}");
        let visible = s.visible_range();
        assert!(visible.contains(&line_100), "{visible:?}");
        assert!(visible.start > 0 && visible.end < text.len());
        // Past the end clamps to the bottom.
        s.scroll_to_position(1.0e12);
        let (scroll, max) = s.scroll_range();
        assert_eq!(scroll, max);
    }

    #[test]
    fn visible_range_of_short_and_empty_text() {
        let mut s = surface("");
        assert_eq!(s.visible_range(), 0..0);
        let mut s = surface("一\n二");
        assert_eq!(s.visible_range(), 0.."一\n二".len());
    }
}
