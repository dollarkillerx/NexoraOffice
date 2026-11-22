//! A plain-text document surface: it takes input events, owns the text and its
//! layout, and renders into a pixel buffer.
//!
//! No Slint and no winit types appear in this file. That boundary is the one the
//! architecture relies on: the shell can be replaced without touching anything
//! here. The code started as the Phase 0 gate G1 spike and still backs it.

use parley::fontique::Script;
use parley::layout::PositionedLayoutItem;
use parley::{
    FontContext, GenericFamily, LayoutContext, PlainEditor, PlainEditorDriver, StyleProperty,
};
use vello_cpu::kurbo::{Affine, Rect};
use vello_cpu::peniko::{Color, Fill};
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

/// Body text size in logical pixels at 100 % zoom.
const FONT_SIZE: f32 = 18.0;
/// Language used to pick fallback fonts for Han characters.
const DEFAULT_LANGUAGE: &str = "zh-Hans";
/// Caret width in logical pixels.
const CARET_WIDTH: f32 = 1.5;
/// Space kept between the caret and the top or bottom edge when the view
/// scrolls to follow it, in logical pixels.
const CARET_SCROLL_MARGIN: f64 = 24.0;
/// Zoom is clamped to this range.
pub const ZOOM_RANGE: (f32, f32) = (0.5, 3.0);

/// A colour as straight (not premultiplied) 8-bit RGBA.
pub type Rgba8 = [u8; 4];

fn color(rgba: Rgba8) -> Color {
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
    Backspace,
    Delete,
    Left { select: bool, word: bool },
    Right { select: bool, word: bool },
    Up { select: bool },
    Down { select: bool },
    LineStart { select: bool },
    LineEnd { select: bool },
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

fn configure_han_fallback(font_cx: &mut FontContext) {
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
    fn reveal_caret(&mut self) {
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
            Action::Newline => drv.insert_or_replace_selection("\n"),
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

    /// Draw the current state into the backing buffer and return it.
    /// Pixels are premultiplied RGBA8.
    pub fn render(&mut self) -> &Pixmap {
        let geometry = self.geometry();
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

        ctx.set_paint(color(palette.selection));
        editor.selection_geometry_with(|rect, _| {
            ctx.fill_rect(&Rect::new(rect.x0, rect.y0, rect.x1, rect.y1));
        });

        // Lines outside the view are skipped; the range is in layout coordinates.
        let visible = (-geometry.text_y, h - geometry.text_y);
        let layout = editor.layout(font_cx, layout_cx);
        for line in layout.lines() {
            let metrics = line.metrics();
            let (baseline, line_height) =
                (f64::from(metrics.baseline), f64::from(metrics.line_height));
            if baseline + line_height < visible.0 || baseline - 2.0 * line_height > visible.1 {
                continue;
            }
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
                    let size = underline.size.unwrap_or(metrics.underline_size).max(*scale);
                    let y = f64::from(glyph_run.baseline() - offset);
                    let x0 = f64::from(glyph_run.offset());
                    let x1 = x0 + f64::from(glyph_run.advance());
                    ctx.set_paint(underline.brush.0);
                    ctx.fill_rect(&Rect::new(x0, y, x1, y + f64::from(size)));
                }

                ctx.set_paint(style.brush.0);
                let coords = bytemuck::cast_slice::<_, i16>(run.normalized_coords());
                ctx.glyph_run(resources, run.font())
                    .font_size(run.font_size())
                    .hint(true)
                    .normalized_coords(coords)
                    .fill_glyphs(glyph_run.positioned_glyphs().map(|glyph| Glyph {
                        id: glyph.id,
                        x: glyph.x,
                        y: glyph.y,
                    }));
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
}
