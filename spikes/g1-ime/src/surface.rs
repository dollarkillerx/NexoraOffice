//! The document surface for gate G1.
//!
//! No Slint and no winit types appear in this file. It takes plain input events,
//! owns the text and its layout, and renders into a pixel buffer. That boundary is
//! the one the architecture relies on: the shell can be replaced without touching
//! anything here.

use parley::fontique::Script;
use parley::layout::PositionedLayoutItem;
use parley::{
    FontContext, GenericFamily, LayoutContext, PlainEditor, PlainEditorDriver, StyleProperty,
};
use vello_cpu::kurbo::{Affine, Rect};
use vello_cpu::peniko::{Color, Fill};
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

/// Body text size in logical pixels.
const FONT_SIZE: f32 = 18.0;
/// Gap between the surface edge and the page, in logical pixels.
const PAGE_MARGIN: f64 = 16.0;
/// Gap between the page edge and the text, in logical pixels.
const PAGE_PADDING: f64 = 32.0;
/// Language used to pick fallback fonts for Han characters.
const DEFAULT_LANGUAGE: &str = "zh-Hans";
/// Caret width in logical pixels.
const CARET_WIDTH: f32 = 1.5;

const DESK: Color = Color::from_rgb8(0xf1, 0xf1, 0xf0);
const PAGE: Color = Color::from_rgb8(0xff, 0xff, 0xff);
const INK: Color = Color::from_rgb8(0x0b, 0x0b, 0x0b);
const SELECTION: Color = Color::from_rgb8(0xcf, 0xe3, 0xfa);
const CARET: Color = Color::from_rgb8(0x1a, 0x5f, 0xd0);

/// The brush Parley carries on every glyph run. Solid colour only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Brush(pub Color);

impl Default for Brush {
    fn default() -> Self {
        Self(INK)
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
/// from them; the spike uses fonts that ship with macOS. On other systems none of
/// these names exist and the system default is left in place.
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

pub struct Surface {
    font_cx: FontContext,
    layout_cx: LayoutContext<Brush>,
    editor: PlainEditor<Brush>,
    resources: Resources,
    ctx: RenderContext,
    pixmap: Pixmap,
    width: u16,
    height: u16,
    scale: f32,
    caret_on: bool,
}

impl Surface {
    pub fn new(text: &str) -> Self {
        let mut editor = PlainEditor::new(FONT_SIZE);
        editor.set_text(text);
        let styles = editor.edit_styles();
        styles.insert(GenericFamily::SystemUi.into());
        styles.insert(StyleProperty::Brush(Brush(INK)));
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
            caret_on: true,
        };
        surface.driver().move_to_text_end();
        surface
    }

    fn driver(&mut self) -> PlainEditorDriver<'_, Brush> {
        self.editor.driver(&mut self.font_cx, &mut self.layout_cx)
    }

    /// Top-left corner of the text block, in physical pixels.
    fn text_origin(&self) -> f64 {
        (PAGE_MARGIN + PAGE_PADDING) * f64::from(self.scale)
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
        self.editor.set_scale(scale);
        let text_width = f64::from(width) - 2.0 * self.text_origin();
        self.editor.set_width(Some(text_width.max(1.0) as f32));
    }

    pub fn size(&self) -> (u16, u16, f32) {
        (self.width, self.height, self.scale)
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
    }

    /// Pointer press at a position in physical pixels relative to the surface.
    pub fn pointer_down(&mut self, x: f64, y: f64, extend: bool) {
        if self.editor.is_composing() {
            return;
        }
        let origin = self.text_origin();
        let (x, y) = ((x - origin) as f32, (y - origin) as f32);
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
        let origin = self.text_origin();
        self.driver()
            .extend_selection_to_point((x - origin) as f32, (y - origin) as f32);
        self.caret_on = true;
    }

    /// Where the platform should put the candidate window: `(x, y, width, height)`
    /// in physical pixels relative to the surface. It covers the composition text
    /// when there is one, otherwise the caret.
    pub fn ime_cursor_area(&mut self) -> (f64, f64, f64, f64) {
        // Make sure the layout reflects the latest edit before measuring it.
        self.editor
            .refresh_layout(&mut self.font_cx, &mut self.layout_cx);
        let area = self.editor.ime_cursor_area();
        let origin = self.text_origin();
        (
            area.x0 + origin,
            area.y0 + origin,
            (area.x1 - area.x0).max(1.0),
            (area.y1 - area.y0).max(1.0),
        )
    }

    /// Draw the current state into the backing buffer and return it.
    /// Pixels are premultiplied RGBA8.
    pub fn render(&mut self) -> &Pixmap {
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
        } = self;
        let scale_f = f64::from(*scale);
        let (w, h) = (f64::from(*width), f64::from(*height));

        ctx.reset();
        ctx.set_fill_rule(Fill::NonZero);
        ctx.set_transform(Affine::IDENTITY);
        ctx.set_paint(DESK);
        ctx.fill_rect(&Rect::new(0.0, 0.0, w, h));
        let margin = PAGE_MARGIN * scale_f;
        ctx.set_paint(PAGE);
        ctx.fill_rect(&Rect::new(margin, margin, (w - margin).max(margin), h));

        let origin = (PAGE_MARGIN + PAGE_PADDING) * scale_f;
        ctx.set_transform(Affine::translate((origin, origin)));

        ctx.set_paint(SELECTION);
        editor.selection_geometry_with(|rect, _| {
            ctx.fill_rect(&Rect::new(rect.x0, rect.y0, rect.x1, rect.y1));
        });

        let layout = editor.layout(font_cx, layout_cx);
        for line in layout.lines() {
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
            ctx.set_paint(CARET);
            ctx.fill_rect(&Rect::new(caret.x0, caret.y0, caret.x1, caret.y1));
        }

        ctx.flush();
        ctx.render(&mut *pixmap, resources);
        pixmap
    }

    /// Number of glyphs in the current layout that the chosen fonts could not map.
    #[cfg(test)]
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

    fn surface(text: &str) -> Surface {
        let mut s = Surface::new(text);
        s.resize(800, 400, 2.0);
        s
    }

    fn preedit(text: &str) -> ImeEvent {
        ImeEvent::Preedit {
            text: text.to_owned(),
            cursor: Some((text.len(), text.len())),
        }
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
}
