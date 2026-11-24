//! A presentation on screen: a strip of slide thumbnails on the left and the
//! current slide, fitted, on the right; and the editor of that slide (see
//! [`editing`]): shapes are selected, moved and resized, text is typed into
//! text boxes and placeholders, slides are added, copied, moved and
//! deleted, and every change can be undone. In a slide show the current
//! slide fills the window on black.
//!
//! A slide is drawn in its own colours whatever the theme of the window:
//! its background, the fills and outlines of its shapes, pictures, tables,
//! and text in its colours and fonts. Charts, diagrams and pictures in
//! formats we do not decode are labelled boxes. Laid-out slides are kept
//! per size, so moving through the slides and scrolling the strip do not
//! lay text out again; pictures are decoded once.

mod editing;

use std::collections::HashMap;
use std::sync::Arc;

use aster_slides::{
    Align as SlideAlign, Anchor, EMU_PER_POINT, Fill, Geometry, Outline, Presentation, Role, Shape,
    Slide, Table, TextBody,
};

use aster_text::{Align, Family, Paragraph, Rules, Shaper, Span, Spec, Style as TextStyle};
pub use editing::{CaretMove, Cursor, NewShape, Selection, TextFormat, TextLook};
use vello_cpu::kurbo::{Affine, BezPath, Ellipse, Line, Rect, RoundedRect, Shape as _, Stroke};
use vello_cpu::peniko::{Color, Fill as FillRule};
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

use crate::picture::{ImageCache, draw_image};
use crate::preview::draw_text;
use crate::text_surface::{DEFAULT_LANGUAGE, HAN_FALLBACK_FAMILIES, Rgba8};

/// Width of the thumbnail strip and of a thumbnail, logical pixels.
const STRIP_WIDTH: f64 = 196.0;
const THUMB_WIDTH: f64 = 148.0;
const THUMB_GAP: f64 = 14.0;
/// Below this window width the strip is not shown.
const STRIP_MIN_WINDOW: f64 = 560.0;
const MARGIN: f64 = 28.0;
/// Laid-out slides kept; past this the cache starts over.
const CACHE_LIMIT: usize = 96;
const LINE_HEIGHT: f32 = 1.2;

const PAGE: Rgba8 = [0xff, 0xff, 0xff, 0xff];
const PAGE_TEXT: Rgba8 = [0x1f, 0x1f, 0x1f, 0xff];
const PAGE_LINE: Rgba8 = [0x8a, 0x8a, 0x8a, 0xff];
const PAGE_BOX: Rgba8 = [0xee, 0xee, 0xee, 0xff];
const PAGE_LABEL: Rgba8 = [0x70, 0x70, 0x70, 0xff];

fn color(rgba: Rgba8) -> Color {
    Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3])
}

/// Colours around the slides and the words on placeholder boxes.
#[derive(Clone, Debug, PartialEq)]
pub struct SlideStyle {
    pub background: Rgba8,
    pub strip: Rgba8,
    pub hairline: Rgba8,
    pub text: Rgba8,
    pub secondary: Rgba8,
    pub accent: Rgba8,
    /// Shown on boxes where a picture, table, chart or other graphic is.
    pub picture: String,
    pub table: String,
    pub chart: String,
    pub graphic: String,
    /// Shown in empty placeholders while editing: a title's, a subtitle's,
    /// any other's.
    pub prompt_title: String,
    pub prompt_subtitle: String,
    pub prompt_text: String,
}

impl Default for SlideStyle {
    fn default() -> Self {
        Self {
            background: [0xf3, 0xf3, 0xf3, 0xff],
            strip: [0xfa, 0xfa, 0xfa, 0xff],
            hairline: [0xe3, 0xe3, 0xe3, 0xff],
            text: [0x1f, 0x1f, 0x1f, 0xff],
            secondary: [0x6b, 0x6b, 0x6b, 0xff],
            accent: [0x7c, 0x5c, 0xe0, 0xff],
            picture: "Picture".to_owned(),
            table: "Table".to_owned(),
            chart: "Chart".to_owned(),
            graphic: "Graphic".to_owned(),
            prompt_title: "Click to add title".to_owned(),
            prompt_subtitle: "Click to add subtitle".to_owned(),
            prompt_text: "Click to add text".to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Drawn {
    current: usize,
    strip_scroll: f64,
    size: (u16, u16),
    scale: f32,
    revision: u64,
    caret_on: bool,
}

/// A slide's texts laid out for one page width.
struct LaidSlide {
    shapes: Vec<LaidShape>,
}

#[derive(Clone)]
struct LaidShape {
    /// Index into the slide's shapes.
    index: usize,
    /// Paragraph boxes: position inside the text area (physical pixels from
    /// the shape's top-left) and the laid-out text.
    texts: Vec<(f64, f64, Paragraph)>,
    /// Where each paragraph of its text is, for the caret and clicks.
    paras: Vec<LaidPara>,
    label: Option<Paragraph>,
    /// A table's cells.
    cells: Vec<LaidCell>,
    /// Drawn: it has text, a fill, an outline, a picture or a box.
    visible: bool,
}

/// One paragraph of a shape's text, physical pixels from the shape's
/// top-left.
#[derive(Clone)]
struct LaidPara {
    x: f64,
    y: f64,
    /// The room for its lines.
    width: f64,
    height: f64,
    align: SlideAlign,
    /// `None` for an empty paragraph.
    laid: Option<Paragraph>,
}

/// A table cell placed in its table's frame.
#[derive(Clone)]
struct LaidCell {
    /// Physical pixels from the frame's top-left.
    rect: Rect,
    fill: Option<Fill>,
    borders: [Option<Outline>; 4],
    /// Relative to the cell's top-left.
    texts: Vec<(f64, f64, Paragraph)>,
}

pub struct SlideView {
    presentation: Arc<Presentation>,
    current: usize,
    strip_scroll: f64,
    /// Wheel movement over the slide not yet turned into a page step.
    wheel: f64,
    width: u16,
    height: u16,
    scale: f32,
    style: SlideStyle,
    shaper: Shaper,
    /// By slide and page width in physical pixels.
    laid: HashMap<(usize, u32), Arc<LaidSlide>>,
    numbers: HashMap<(usize, bool), Arc<Paragraph>>,
    ctx: RenderContext,
    pixmap: Pixmap,
    resources: Resources,
    glyphs: Vec<Glyph>,
    drawn: Option<Drawn>,
    images: ImageCache,
    // --- Editing (see `editing`) ---
    selection: Selection,
    /// A composition not committed yet, and its caret.
    preedit: Option<(String, Option<(usize, usize)>)>,
    drag: Option<editing::Drag>,
    caret_on: bool,
    ime_area: (f64, f64, f64, f64),
    history: editing::History,
    /// Counts every change to what is drawn.
    revision: u64,
    /// In a slide show.
    presenting: bool,
    /// The thumbnails have the keyboard: keys act on slides, not shapes.
    strip_focus: bool,
    /// Where the caret goes up and down from, physical pixels.
    goal_x: Option<f64>,
}

impl SlideView {
    pub fn new(presentation: Arc<Presentation>) -> Self {
        let mut shaper = Shaper::new();
        shaper.set_han_fallbacks(&HAN_FALLBACK_FAMILIES);
        Self {
            presentation,
            current: 0,
            strip_scroll: 0.0,
            wheel: 0.0,
            width: 0,
            height: 0,
            scale: 1.0,
            style: SlideStyle::default(),
            shaper,
            laid: HashMap::new(),
            numbers: HashMap::new(),
            ctx: RenderContext::new(1, 1),
            pixmap: Pixmap::new(1, 1),
            resources: Resources::new(),
            glyphs: Vec::new(),
            drawn: None,
            images: ImageCache::default(),
            selection: Selection::None,
            preedit: None,
            drag: None,
            caret_on: true,
            ime_area: (0.0, 0.0, 1.0, 1.0),
            history: editing::History::default(),
            revision: 0,
            presenting: false,
            strip_focus: false,
            goal_x: None,
        }
    }

    pub fn presentation(&self) -> &Arc<Presentation> {
        &self.presentation
    }

    /// Something drawn changed.
    fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn set_caret_on(&mut self, on: bool) {
        self.caret_on = on;
    }

    pub fn is_composing(&self) -> bool {
        self.preedit.is_some()
    }

    /// Where the input method's candidate window goes: at the caret.
    pub fn ime_cursor_area(&mut self) -> (f64, f64, f64, f64) {
        if self.needs_render() {
            self.render();
        }
        self.ime_area
    }

    pub fn current(&self) -> usize {
        self.current
    }

    pub fn count(&self) -> usize {
        self.presentation.slides.len()
    }

    pub fn set_style(&mut self, style: SlideStyle) {
        if style != self.style {
            self.style = style;
            self.laid.clear();
            self.numbers.clear();
            self.drawn = None;
        }
    }

    pub fn resize(&mut self, width: u16, height: u16, scale: f32) {
        if (width, height) == (self.width, self.height) && scale == self.scale {
            return;
        }
        if scale != self.scale {
            self.numbers.clear();
        }
        self.width = width;
        self.height = height;
        self.scale = scale;
        self.ctx = RenderContext::new(width.max(1), height.max(1));
        self.pixmap = Pixmap::new(width.max(1), height.max(1));
        self.drawn = None;
        self.clamp_strip();
    }

    pub fn size(&self) -> (u16, u16, f32) {
        (self.width, self.height, self.scale)
    }

    fn s(&self) -> f64 {
        f64::from(self.scale)
    }

    fn shows_strip(&self) -> bool {
        !self.presenting && f64::from(self.width) / self.s() >= STRIP_MIN_WINDOW
    }

    fn strip_width(&self) -> f64 {
        if self.shows_strip() {
            STRIP_WIDTH * self.s()
        } else {
            0.0
        }
    }

    /// Height of a thumbnail plus its gap, physical pixels.
    fn thumb_pitch(&self) -> f64 {
        let (w, h) = self.presentation.size;
        (THUMB_WIDTH * h as f64 / w.max(1) as f64 + THUMB_GAP) * self.s()
    }

    fn strip_height(&self) -> f64 {
        self.thumb_pitch() * self.count() as f64 + THUMB_GAP * self.s()
    }

    fn clamp_strip(&mut self) {
        let max = (self.strip_height() - f64::from(self.height)).max(0.0);
        self.strip_scroll = self.strip_scroll.clamp(0.0, max);
    }

    /// Show slide `index`. Returns whether it changed.
    pub fn go_to(&mut self, index: usize) -> bool {
        if self.count() == 0 {
            return false;
        }
        let index = index.min(self.count() - 1);
        if index == self.current {
            return false;
        }
        self.leave_text();
        self.selection = Selection::None;
        self.current = index;
        self.wheel = 0.0;
        self.touch();
        // Keep its thumbnail in view.
        let pitch = self.thumb_pitch();
        let top = THUMB_GAP * self.s() + pitch * index as f64;
        let bottom = top + pitch;
        if top < self.strip_scroll {
            self.strip_scroll = top - THUMB_GAP * self.s();
        } else if bottom > self.strip_scroll + f64::from(self.height) {
            self.strip_scroll = bottom - f64::from(self.height);
        }
        self.clamp_strip();
        true
    }

    pub fn next_slide(&mut self) -> bool {
        self.go_to(self.current + 1)
    }

    pub fn previous_slide(&mut self) -> bool {
        self.current > 0 && self.go_to(self.current - 1)
    }

    pub fn last_slide(&mut self) -> bool {
        self.go_to(self.count().saturating_sub(1))
    }

    /// The thumbnail under a point of the strip (physical pixels).
    fn thumb_at(&self, x: f64, y: f64) -> Option<usize> {
        if x >= self.strip_width() || self.count() == 0 {
            return None;
        }
        let y = y + self.strip_scroll - THUMB_GAP * self.s() / 2.0;
        let index = (y / self.thumb_pitch()).floor();
        (index >= 0.0).then(|| (index as usize).min(self.count() - 1))
    }

    /// Where a slide dragged in the strip to height `y` would go: the gap
    /// before slide `n`.
    fn drop_slot(&self, y: f64) -> usize {
        let y = y + self.strip_scroll - THUMB_GAP * self.s() / 2.0;
        let slot = (y / self.thumb_pitch() + 0.5).floor();
        (slot.max(0.0) as usize).min(self.count())
    }

    /// The wheel: over the strip it scrolls the strip, over the slide it
    /// turns pages. `x` is where the pointer is, physical pixels.
    pub fn wheel(&mut self, x: f64, delta: f64) -> bool {
        if x < self.strip_width() {
            let before = self.strip_scroll;
            self.strip_scroll += delta;
            self.clamp_strip();
            return self.strip_scroll != before;
        }
        self.wheel += delta;
        let step = 90.0 * self.s();
        if self.wheel >= step {
            self.wheel = 0.0;
            return self.next_slide();
        }
        if self.wheel <= -step {
            self.wheel = 0.0;
            return self.previous_slide();
        }
        false
    }

    /// Strip scroll offset and its bound, physical pixels.
    pub fn scroll_range(&self) -> (f64, f64) {
        (
            self.strip_scroll,
            (self.strip_height() - f64::from(self.height)).max(0.0),
        )
    }

    /// The text of the current slide.
    pub fn text(&self) -> String {
        self.presentation
            .slides
            .get(self.current)
            .map(Slide::plain_text)
            .unwrap_or_default()
    }

    /// The current slide's frame on screen, physical pixels.
    fn page_rect(&self) -> Rect {
        let (w, h) = self.presentation.size;
        let (w, h) = (w.max(1) as f64, h.max(1) as f64);
        let margin = if self.presenting {
            0.0
        } else {
            MARGIN * self.s()
        };
        let left = self.strip_width() + margin;
        let avail_w = (f64::from(self.width) - left - margin).max(1.0);
        let avail_h = (f64::from(self.height) - 2.0 * margin).max(1.0);
        let fit = (avail_w / w).min(avail_h / h);
        let (pw, ph) = (w * fit, h * fit);
        let x = left + (avail_w - pw) / 2.0;
        let y = margin + (avail_h - ph) / 2.0;
        Rect::new(x.round(), y.round(), (x + pw).round(), (y + ph).round())
    }

    pub fn needs_render(&self) -> bool {
        self.drawn.as_ref() != Some(&self.state())
    }

    fn state(&self) -> Drawn {
        Drawn {
            current: self.current,
            strip_scroll: self.strip_scroll,
            size: (self.width, self.height),
            scale: self.scale,
            revision: self.revision,
            caret_on: self.caret_on && matches!(self.selection, Selection::Text { .. }),
        }
    }

    /// Slide `index`'s laid-out text is out of date; `None` for all.
    fn forget(&mut self, index: Option<usize>) {
        match index {
            Some(index) => self.laid.retain(|(i, _), _| *i != index),
            None => self.laid.clear(),
        }
        self.touch();
    }

    pub fn render(&mut self) -> &Pixmap {
        if !self.needs_render() {
            return &self.pixmap;
        }
        self.drawn = Some(self.state());
        let (w, h) = (f64::from(self.width), f64::from(self.height));
        let s = self.s();
        let style = self.style.clone();
        self.ctx.reset();
        self.ctx.set_fill_rule(FillRule::NonZero);
        self.ctx.set_transform(Affine::IDENTITY);
        self.ctx.set_paint(color(style.background));
        self.ctx.fill_rect(&Rect::new(0.0, 0.0, w, h));

        if self.presenting {
            self.ctx.set_paint(color([0, 0, 0, 0xff]));
            self.ctx.fill_rect(&Rect::new(0.0, 0.0, w, h));
        }
        if self.count() > 0 {
            let page = self.page_rect();
            self.draw_slide(self.current, page, true);
            if !self.presenting {
                self.draw_editing(page);
            }
        }

        if self.shows_strip() {
            let strip = self.strip_width();
            self.ctx.set_paint(color(style.strip));
            self.ctx.fill_rect(&Rect::new(0.0, 0.0, strip, h));
            self.ctx.set_paint(color(style.hairline));
            self.ctx
                .fill_rect(&Rect::new(strip - s.round().max(1.0), 0.0, strip, h));
            self.ctx
                .push_clip_layer(&Rect::new(0.0, 0.0, strip, h).to_path(0.1));
            let pitch = self.thumb_pitch();
            let first = ((self.strip_scroll / pitch).floor().max(0.0)) as usize;
            let (sw, sh) = self.presentation.size;
            let thumb_h = THUMB_WIDTH * sh as f64 / sw.max(1) as f64 * s;
            let thumb_x = (STRIP_WIDTH - THUMB_WIDTH - 12.0) * s;
            for index in first..self.count() {
                let y = THUMB_GAP * s + pitch * index as f64 - self.strip_scroll;
                if y > h {
                    break;
                }
                let rect = Rect::new(
                    thumb_x.round(),
                    y.round(),
                    (thumb_x + THUMB_WIDTH * s).round(),
                    (y + thumb_h).round(),
                );
                self.draw_slide(index, rect, false);
                let selected = index == self.current;
                if selected {
                    let border = (2.0 * s).round();
                    self.ctx.set_paint(color(style.accent));
                    self.ctx.set_stroke(Stroke::new(border));
                    self.ctx.stroke_rect(&rect.inset(border * 1.5));
                } else {
                    self.ctx.set_paint(color(style.hairline));
                    self.ctx.set_stroke(Stroke::new(s.round().max(1.0)));
                    self.ctx.stroke_rect(&rect.inset(s / 2.0));
                }
                if selected && self.strip_focus {
                    let border = (2.0 * s).round();
                    self.ctx.set_paint(color(style.accent));
                    self.ctx.set_stroke(Stroke::new(border));
                    self.ctx.stroke_rect(&rect.inset(-border * 1.5));
                }
                let hidden = self.presentation.slides[index].hidden;
                let number = self.number(index, selected);
                let x = thumb_x - 8.0 * s - f64::from(number.width());
                self.paint(&number, x, y);
                if hidden {
                    // A diagonal across a skipped slide's thumbnail.
                    self.ctx.set_paint(color(style.secondary));
                    self.ctx.set_stroke(Stroke::new(s.max(1.0)));
                    self.ctx.stroke_path(
                        &Line::new((rect.x0, rect.y1), (rect.x1, rect.y0)).to_path(0.1),
                    );
                }
            }
            if let Some(slot) = self.drop_indicator() {
                // Where a dragged slide would go.
                let y = THUMB_GAP * s / 2.0 + pitch * slot as f64 - self.strip_scroll;
                self.ctx.set_paint(color(style.accent));
                self.ctx.fill_rect(&Rect::new(
                    thumb_x - 4.0 * s,
                    y - s,
                    thumb_x + (THUMB_WIDTH + 4.0) * s,
                    y + s,
                ));
            }
            self.ctx.pop_layer();
        }
        self.ctx.flush();
        self.ctx.render(&mut self.pixmap, &mut self.resources);
        &self.pixmap
    }

    fn number(&mut self, index: usize, selected: bool) -> Arc<Paragraph> {
        if let Some(found) = self.numbers.get(&(index, selected)) {
            return found.clone();
        }
        let style = &self.style;
        let text = (index + 1).to_string();
        let paragraph = Arc::new(simple(
            &mut self.shaper,
            &text,
            11.0,
            if selected {
                style.accent
            } else {
                style.secondary
            },
            self.scale,
            Align::End,
        ));
        self.numbers.insert((index, selected), paragraph.clone());
        paragraph
    }

    fn laid_slide(&mut self, index: usize, page: Rect) -> Arc<LaidSlide> {
        let key = (index, page.width().round() as u32);
        if let Some(found) = self.laid.get(&key) {
            return found.clone();
        }
        if self.laid.len() >= CACHE_LIMIT {
            self.laid.clear();
        }
        let px_per_emu = page.width() / self.presentation.size.0.max(1) as f64;
        let presentation = self.presentation.clone();
        let slide = &presentation.slides[index];
        let mut shapes = Vec::new();
        for (i, shape) in slide.shapes.iter().enumerate() {
            let (width, height) = (
                shape.frame.width as f64 * px_per_emu,
                shape.frame.height as f64 * px_per_emu,
            );
            let (texts, paras) = match &shape.text {
                Some(body) => lay_out_text(&mut self.shaper, body, width, height, px_per_emu),
                None => (Vec::new(), Vec::new()),
            };
            let cells = match &shape.table {
                Some(table) => lay_out_table(&mut self.shaper, table, px_per_emu),
                None => Vec::new(),
            };
            let label = match shape.role {
                Role::Picture => Some(self.style.picture.clone()),
                Role::Table if shape.table.is_none() => Some(self.style.table.clone()),
                Role::Chart => Some(self.style.chart.clone()),
                Role::Graphic => Some(self.style.graphic.clone()),
                _ => None,
            }
            .map(|label| {
                // Scaled with the page, so thumbnails keep their proportions.
                let size = (12.0 * EMU_PER_POINT * px_per_emu / f64::from(self.scale)) as f32;
                simple(
                    &mut self.shaper,
                    &label,
                    size.max(1.0),
                    PAGE_LABEL,
                    self.scale,
                    Align::Start,
                )
            });
            let visible = !texts.is_empty()
                || !cells.is_empty()
                || label.is_some()
                || shape.fill.is_some()
                || shape.line.is_some()
                || shape.picture.is_some();
            // Shapes of the layout and master are only drawn; the slide's
            // own are kept for clicks even when there is nothing to draw.
            if !visible && shape.inherited {
                continue;
            }
            shapes.push(LaidShape {
                index: i,
                texts,
                paras,
                label,
                cells,
                visible,
            });
        }
        let laid = Arc::new(LaidSlide { shapes });
        self.laid.insert(key, laid.clone());
        laid
    }

    /// Paint `fill` over `area` (a path in `frame`, before `transform`).
    fn fill_area(&mut self, fill: &Fill, area: &BezPath, frame: Rect, transform: Affine) {
        self.ctx.set_transform(transform);
        match fill {
            Fill::Solid(rgba) => {
                self.ctx.set_paint(color(*rgba));
                self.ctx.fill_path(area);
            }
            Fill::Gradient { stops, angle } => {
                let stops: Vec<(f32, Color)> =
                    stops.iter().map(|(at, rgba)| (*at, color(*rgba))).collect();
                if stops.len() < 2 {
                    if let Some((_, only)) = stops.first() {
                        self.ctx.set_paint(*only);
                        self.ctx.fill_path(area);
                    }
                } else {
                    // Clockwise from left to right, across the whole frame.
                    let (sin, cos) = angle.to_radians().sin_cos();
                    let half = (frame.width() * cos.abs() + frame.height() * sin.abs()) / 2.0;
                    let center = frame.center();
                    let start = (center.x - cos * half, center.y - sin * half);
                    let end = (center.x + cos * half, center.y + sin * half);
                    self.ctx.set_paint(
                        vello_cpu::peniko::Gradient::new_linear(start, end).with_stops(&stops[..]),
                    );
                    self.ctx.fill_path(area);
                }
            }
            Fill::Picture(picture) => {
                if let Some(image) = self.images.get(&picture.image) {
                    self.ctx.push_clip_layer(area);
                    draw_image(&mut self.ctx, &image, frame, Some(picture.crop), transform);
                    self.ctx.pop_layer();
                }
            }
        }
        self.ctx.set_transform(Affine::IDENTITY);
    }

    fn draw_slide(&mut self, index: usize, page: Rect, main: bool) {
        let mut laid = self.laid_slide(index, page);
        // The text being typed into, with its composition.
        if main
            && index == self.current
            && let Some(edited) = self.edited_layout(page)
        {
            let mut copy = LaidSlide {
                shapes: laid.shapes.clone(),
            };
            if let Some(slot) = copy.shapes.iter_mut().find(|s| s.index == edited.index) {
                *slot = edited;
            }
            laid = Arc::new(copy);
        }
        let presentation = self.presentation.clone();
        let slide = &presentation.slides[index];
        let px_per_emu = page.width() / presentation.size.0.max(1) as f64;
        let s = self.s();
        // The page, with a soft edge on the big one.
        if main {
            self.ctx.set_paint(color(self.style.hairline));
            self.ctx.fill_rect(&page.inset(s));
        }
        self.ctx.set_paint(color(PAGE));
        self.ctx.fill_rect(&page);
        self.ctx.push_clip_layer(&page.to_path(0.1));
        if let Some(background) = &slide.background {
            self.fill_area(background, &page.to_path(0.1), page, Affine::IDENTITY);
        }
        // Hairlines stay visible however small the page.
        let min_line = if main { s.max(1.0) * 0.75 } else { 0.5 };
        for laid_shape in laid.shapes.iter().filter(|s| s.visible) {
            let shape = &slide.shapes[laid_shape.index];
            let frame = Rect::new(
                page.x0 + shape.frame.x as f64 * px_per_emu,
                page.y0 + shape.frame.y as f64 * px_per_emu,
                page.x0 + (shape.frame.x + shape.frame.width) as f64 * px_per_emu,
                page.y0 + (shape.frame.y + shape.frame.height) as f64 * px_per_emu,
            );
            let transform = shape_transform(shape, frame);
            let path = outline(&shape.geometry, frame, shape);
            if let Some(fill) = &shape.fill
                && shape.geometry != Geometry::Line
            {
                self.fill_area(fill, &path, frame, transform);
            }
            let mut boxed = laid_shape.label.is_some() && shape.role != Role::Picture;
            if let Some(picture) = &shape.picture {
                match self.images.get(&picture.image) {
                    Some(image) => {
                        draw_image(&mut self.ctx, &image, frame, Some(picture.crop), transform)
                    }
                    // A format we do not decode (EMF, TIFF ...): its box.
                    None => boxed = true,
                }
            } else if shape.role == Role::Picture {
                boxed = true;
            }
            // Boxes for what is not drawn: charts, diagrams, pictures that
            // cannot be shown.
            if boxed && let Some(label) = &laid_shape.label {
                self.ctx.set_transform(transform);
                self.ctx.set_paint(color(PAGE_BOX));
                self.ctx.fill_rect(&frame);
                self.ctx.set_paint(color(PAGE_LINE));
                self.ctx.set_stroke(Stroke::new(min_line));
                self.ctx.stroke_rect(&frame);
                // A small picture, such as an icon in front of a line of
                // text, is only a box: its name would spill over the text.
                let fits = f64::from(label.width()) <= frame.width() * 0.9
                    && f64::from(label.height()) <= frame.height() * 0.9;
                if fits {
                    let x = (frame.x0 + frame.x1 - f64::from(label.width())) / 2.0;
                    let y = (frame.y0 + frame.y1 - f64::from(label.height())) / 2.0;
                    self.ctx
                        .set_transform(transform * Affine::translate((x, y)));
                    draw_text(
                        &mut self.ctx,
                        &mut self.resources,
                        label,
                        0.0,
                        label.height(),
                        self.scale,
                        &mut self.glyphs,
                    );
                }
            }
            if !laid_shape.cells.is_empty() {
                self.draw_table(&laid_shape.cells, frame, transform, px_per_emu, min_line);
            }
            if let Some(line) = shape.line {
                self.ctx.set_transform(transform);
                self.ctx.set_paint(color(line.color));
                self.ctx
                    .set_stroke(Stroke::new((line.width as f64 * px_per_emu).max(min_line)));
                self.ctx.stroke_path(&path);
            }
            for (x, y, paragraph) in &laid_shape.texts {
                self.ctx
                    .set_transform(transform * Affine::translate((frame.x0 + x, frame.y0 + y)));
                draw_text(
                    &mut self.ctx,
                    &mut self.resources,
                    paragraph,
                    f32::MIN,
                    f32::MAX,
                    self.scale,
                    &mut self.glyphs,
                );
            }
        }
        self.ctx.set_transform(Affine::IDENTITY);
        self.ctx.pop_layer();
    }

    /// A table's cells: fills, then texts, then borders over both.
    fn draw_table(
        &mut self,
        cells: &[LaidCell],
        frame: Rect,
        transform: Affine,
        px_per_emu: f64,
        min_line: f64,
    ) {
        let at = |cell: &LaidCell| cell.rect + frame.origin().to_vec2();
        for cell in cells {
            if let Some(fill) = &cell.fill {
                let rect = at(cell);
                self.fill_area(fill, &rect.to_path(0.1), rect, transform);
            }
        }
        for cell in cells {
            let rect = at(cell);
            self.ctx.push_clip_layer(&transform_path(transform, rect));
            for (x, y, paragraph) in &cell.texts {
                self.ctx
                    .set_transform(transform * Affine::translate((rect.x0 + x, rect.y0 + y)));
                draw_text(
                    &mut self.ctx,
                    &mut self.resources,
                    paragraph,
                    f32::MIN,
                    f32::MAX,
                    self.scale,
                    &mut self.glyphs,
                );
            }
            self.ctx.set_transform(Affine::IDENTITY);
            self.ctx.pop_layer();
        }
        self.ctx.set_transform(transform);
        for cell in cells {
            let rect = at(cell);
            let edges = [
                ((rect.x0, rect.y0), (rect.x0, rect.y1)),
                ((rect.x0, rect.y0), (rect.x1, rect.y0)),
                ((rect.x1, rect.y0), (rect.x1, rect.y1)),
                ((rect.x0, rect.y1), (rect.x1, rect.y1)),
            ];
            for (border, (a, b)) in cell.borders.iter().zip(edges) {
                if let Some(border) = border {
                    self.ctx.set_paint(color(border.color));
                    self.ctx.set_stroke(Stroke::new(
                        (border.width as f64 * px_per_emu).max(min_line),
                    ));
                    self.ctx.stroke_path(&Line::new(a, b).to_path(0.1));
                }
            }
        }
        self.ctx.set_transform(Affine::IDENTITY);
    }

    fn paint(&mut self, paragraph: &Paragraph, x: f64, y: f64) {
        self.ctx.set_transform(Affine::translate((x, y)));
        draw_text(
            &mut self.ctx,
            &mut self.resources,
            paragraph,
            0.0,
            paragraph.height(),
            self.scale,
            &mut self.glyphs,
        );
        self.ctx.set_transform(Affine::IDENTITY);
    }

    /// Render and encode as PNG, for tests and `--dump`.
    pub fn encode_png(&mut self) -> Result<Vec<u8>, String> {
        let (width, height) = (self.width.max(1), self.height.max(1));
        self.drawn = None;
        let data = self.render().data().to_vec();
        Pixmap::from_parts(data, width, height)
            .into_png()
            .map_err(|err| err.to_string())
    }
}

/// Rotation and flips about the frame's centre.
fn shape_transform(shape: &Shape, frame: Rect) -> Affine {
    let center = frame.center();
    let mut transform = Affine::IDENTITY;
    if shape.flip_h || shape.flip_v {
        let (sx, sy) = (
            if shape.flip_h { -1.0 } else { 1.0 },
            if shape.flip_v { -1.0 } else { 1.0 },
        );
        transform = Affine::translate(center.to_vec2())
            * Affine::scale_non_uniform(sx, sy)
            * Affine::translate(-center.to_vec2());
    }
    if shape.rotation != 0.0 {
        transform = Affine::rotate_about(shape.rotation.to_radians(), center) * transform;
    }
    // Text is not mirrored by a flip, only moved with the shape: keep it
    // readable by flipping back only the line geometry's flips for text is
    // beyond this view; flips are rare on text shapes.
    transform
}

fn outline(geometry: &Geometry, frame: Rect, _shape: &Shape) -> BezPath {
    match geometry {
        Geometry::Ellipse => Ellipse::from_rect(frame).to_path(0.1),
        Geometry::RoundRect => {
            let radius = frame.width().min(frame.height()) * 0.1667;
            RoundedRect::from_rect(frame, radius).to_path(0.1)
        }
        Geometry::Line => Line::new((frame.x0, frame.y0), (frame.x1, frame.y1)).to_path(0.1),
        Geometry::Rect | Geometry::Other(_) => frame.to_path(0.1),
    }
}

/// Lay out a text body inside a frame `width` × `height` physical pixels:
/// positions relative to the frame's top-left.
fn lay_out_text(
    shaper: &mut Shaper,
    body: &TextBody,
    width: f64,
    height: f64,
    px_per_emu: f64,
) -> (Vec<(f64, f64, Paragraph)>, Vec<LaidPara>) {
    let (mut out, mut paras, used) = lay_out_paragraphs(shaper, body, width, px_per_emu);
    let shift = anchor_shift(body, height, used, px_per_emu);
    if shift != 0.0 {
        for (_, y, _) in &mut out {
            *y += shift;
        }
        for para in &mut paras {
            para.y += shift;
        }
    }
    (out, paras)
}

/// How far down a body's text goes for its vertical anchor in a frame of
/// `height`, `used` being the height of the text.
fn anchor_shift(body: &TextBody, height: f64, used: f64, px_per_emu: f64) -> f64 {
    let top = body.insets[1] as f64 * px_per_emu;
    let bottom = body.insets[3] as f64 * px_per_emu;
    let room = height - top - bottom;
    match body.anchor {
        Anchor::Top => 0.0,
        Anchor::Middle => (room - used) / 2.0,
        Anchor::Bottom => room - used,
    }
}

/// Move laid-out paragraphs to the body's vertical anchor.
fn anchor(
    out: &mut [(f64, f64, Paragraph)],
    body: &TextBody,
    height: f64,
    used: f64,
    px_per_emu: f64,
) {
    let shift = anchor_shift(body, height, used, px_per_emu);
    if shift != 0.0 {
        for (_, y, _) in out {
            *y += shift;
        }
    }
}

/// The font a run asks for, then its East Asian font, then the system's.
fn family(run: &aster_slides::Run) -> Family {
    let names: Vec<String> = [run.font.as_deref(), run.east_asian_font.as_deref()]
        .into_iter()
        .flatten()
        .map(|name| {
            name.chars()
                .filter(|c| !matches!(c, '"' | ',' | ';'))
                .collect::<String>()
        })
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!("\"{}\"", name.trim()))
        .collect();
    if names.is_empty() {
        Family::SystemUi
    } else {
        Family::List(format!("{}, system-ui", names.join(", ")).into())
    }
}

fn run_style(run: &aster_slides::Run, size: f32) -> TextStyle {
    TextStyle {
        family: family(run),
        size: size.max(0.5),
        color: run.color.unwrap_or(PAGE_TEXT),
        bold: run.bold,
        italic: run.italic,
        underline: run.underline,
        strike: run.strike,
        rise: 0.0,
    }
}

/// Lay out a body's paragraphs from the top of its text area; returns them
/// (relative to the frame's top-left, physical pixels) and the height the
/// text takes.
fn lay_out_body(
    shaper: &mut Shaper,
    body: &TextBody,
    frame_width: f64,
    px_per_emu: f64,
) -> (Vec<(f64, f64, Paragraph)>, f64) {
    let (out, _, used) = lay_out_paragraphs(shaper, body, frame_width, px_per_emu);
    (out, used)
}

/// Lay out a body's paragraphs from the top of its text area: the boxes to
/// draw (bullets and text), where each paragraph is, and the height the
/// text takes.
fn lay_out_paragraphs(
    shaper: &mut Shaper,
    body: &TextBody,
    frame_width: f64,
    px_per_emu: f64,
) -> (Vec<(f64, f64, Paragraph)>, Vec<LaidPara>, f64) {
    let [left, top, right, _] = body.insets.map(|inset| inset as f64 * px_per_emu);
    let width = frame_width - left - right;
    let points_to_px = EMU_PER_POINT * px_per_emu * f64::from(body.font_scale);
    let mut out: Vec<(f64, f64, Paragraph)> = Vec::new();
    let mut paras: Vec<LaidPara> = Vec::new();
    let mut y = 0.0;
    for paragraph in &body.paragraphs {
        let margin = paragraph.margin as f64 * px_per_emu;
        let indent = paragraph.indent as f64 * px_per_emu;
        let text: String = paragraph.runs.iter().map(|run| run.text.as_str()).collect();
        let size = (f64::from(paragraph.size()) * points_to_px) as f32;
        if text.is_empty() {
            // An empty line still takes its height.
            let height = f64::from(size * LINE_HEIGHT);
            let x = left + (margin + indent.min(0.0)).max(0.0).max(margin);
            paras.push(LaidPara {
                x,
                y: top + y,
                width: (width - (x - left)).max(1.0),
                height,
                align: paragraph.align,
                laid: None,
            });
            y += height;
            continue;
        }
        let mut spans = Vec::with_capacity(paragraph.runs.len());
        let mut start = 0;
        for run in &paragraph.runs {
            let end = start + run.text.len();
            spans.push(Span {
                range: start..end,
                style: run_style(run, (f64::from(run.size) * points_to_px) as f32),
            });
            start = end;
        }
        let base = spans
            .first()
            .map(|span| span.style.clone())
            .unwrap_or_else(|| style(size, false, false, false));
        let language = aster_text::segment::guess_language(&text, DEFAULT_LANGUAGE);
        // The bullet hangs in the indent; text starts after it when the
        // bullet is wider than the indent leaves room for.
        let mut margin = margin;
        if let Some(bullet) = &paragraph.bullet {
            let first_size = paragraph
                .runs
                .first()
                .map_or(size, |run| (f64::from(run.size) * points_to_px) as f32);
            let mut mark_style = style(first_size, false, false, false);
            if let Some(first) = paragraph.runs.first() {
                mark_style.color = first.color.unwrap_or(PAGE_TEXT);
            }
            let mark = shaper.lay_out(&Spec {
                text: bullet,
                base: mark_style,
                spans: &[],
                language,
                width: None,
                scale: 1.0,
                line_height: LINE_HEIGHT,
                align: Align::Start,
                rules: Rules::for_language(language),
            });
            let bullet_x = (margin + indent).max(0.0);
            let after = bullet_x + f64::from(mark.width()) + f64::from(first_size) * 0.3;
            margin = margin.max(after);
            out.push((left + bullet_x, top + y, mark));
        }
        let text_width = (width - margin).max(1.0);
        // Sizes in `Style` are logical; the layout here is already in
        // physical pixels, so scale is 1.
        let laid = shaper.lay_out(&Spec {
            text: &text,
            base,
            spans: &spans,
            language,
            width: body.wrap.then_some(text_width as f32),
            scale: 1.0,
            line_height: LINE_HEIGHT,
            align: match paragraph.align {
                SlideAlign::Left | SlideAlign::Justify => Align::Start,
                SlideAlign::Center => Align::Center,
                SlideAlign::Right => Align::End,
            },
            rules: Rules::for_language(language),
        });
        let x = left + margin;
        let h = f64::from(laid.height());
        paras.push(LaidPara {
            x,
            y: top + y,
            width: text_width,
            height: h,
            align: paragraph.align,
            laid: Some(laid.clone()),
        });
        out.push((x, top + y, laid));
        y += h;
    }
    (out, paras, y)
}

/// Paragraphs laid out from the top of a text area, and the height they
/// take.
type LaidBody = (Vec<(f64, f64, Paragraph)>, f64);

/// A table's cells placed in its frame (physical pixels from the frame's
/// top-left), rows grown to fit their text as PowerPoint does.
fn lay_out_table(shaper: &mut Shaper, table: &Table, px_per_emu: f64) -> Vec<LaidCell> {
    let mut xs = vec![0.0];
    for width in &table.columns {
        xs.push(xs.last().copied().unwrap_or(0.0) + *width as f64 * px_per_emu);
    }
    let cols = table.columns.len();
    let width_of = |c: usize, span: u32| {
        let end = (c + span.max(1) as usize).min(cols);
        xs[end] - xs[c.min(cols)]
    };
    // Lay out every shown cell's text at its width first, to know how tall
    // each row must be.
    let mut texts: Vec<Vec<Option<LaidBody>>> = Vec::new();
    let mut heights: Vec<f64> = Vec::new();
    for row in &table.rows {
        let mut row_texts = Vec::with_capacity(row.cells.len());
        let mut height = row.height as f64 * px_per_emu;
        for (c, cell) in row.cells.iter().enumerate().take(cols) {
            let laid = match (&cell.text, cell.merged) {
                (Some(body), false) => {
                    let (out, used) =
                        lay_out_body(shaper, body, width_of(c, cell.span.0), px_per_emu);
                    if cell.span.1 <= 1 {
                        let insets = (body.insets[1] + body.insets[3]) as f64 * px_per_emu;
                        height = height.max(used + insets);
                    }
                    Some((out, used))
                }
                _ => None,
            };
            row_texts.push(laid);
        }
        texts.push(row_texts);
        heights.push(height);
    }
    let mut ys = vec![0.0];
    for height in &heights {
        ys.push(ys.last().copied().unwrap_or(0.0) + height);
    }
    let rows = table.rows.len();
    let mut out = Vec::new();
    for (r, (row, row_texts)) in table.rows.iter().zip(texts).enumerate() {
        for (c, (cell, laid)) in row.cells.iter().zip(row_texts).enumerate().take(cols) {
            if cell.merged {
                continue;
            }
            let x1 = xs[(c + cell.span.0.max(1) as usize).min(cols)];
            let y1 = ys[(r + cell.span.1.max(1) as usize).min(rows)];
            let rect = Rect::new(xs[c], ys[r], x1, y1);
            let texts = match (laid, &cell.text) {
                (Some((mut items, used)), Some(body)) => {
                    anchor(&mut items, body, rect.height(), used, px_per_emu);
                    items
                }
                _ => Vec::new(),
            };
            out.push(LaidCell {
                rect,
                fill: cell.fill.clone(),
                borders: cell.borders,
                texts,
            });
        }
    }
    out
}

/// A rectangle through a transform, as a path for clipping.
fn transform_path(transform: Affine, rect: Rect) -> BezPath {
    transform * rect.to_path(0.1)
}

fn style(size: f32, bold: bool, italic: bool, underline: bool) -> TextStyle {
    TextStyle {
        family: Family::SystemUi,
        size: size.max(0.5),
        color: PAGE_TEXT,
        bold,
        italic,
        underline,
        strike: false,
        rise: 0.0,
    }
}

fn simple(
    shaper: &mut Shaper,
    text: &str,
    size: f32,
    color: Rgba8,
    scale: f32,
    align: Align,
) -> Paragraph {
    let language = aster_text::segment::guess_language(text, DEFAULT_LANGUAGE);
    shaper.lay_out(&Spec {
        text,
        base: TextStyle {
            family: Family::SystemUi,
            size,
            color,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            rise: 0.0,
        },
        spans: &[],
        language,
        width: None,
        scale,
        line_height: 1.25,
        align,
        rules: Rules::for_language(language),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aster_slides::{Frame, Paragraph as SlideParagraph, Run, TextBody};

    fn text_shape(frame: Frame, role: Role, text: &str, size: f32) -> Shape {
        let mut shape = Shape::new(frame, Geometry::Rect, role);
        shape.text = Some(TextBody {
            paragraphs: vec![SlideParagraph {
                runs: vec![Run::new(text, size)],
                bullet: Some("•".to_owned()),
                ..SlideParagraph::default()
            }],
            ..TextBody::default()
        });
        shape
    }

    fn presentation(count: usize) -> Arc<Presentation> {
        let slides = (0..count)
            .map(|n| {
                let mut ellipse = Shape::new(
                    Frame::new(6_000_000, 3_000_000, 2_000_000, 1_000_000),
                    Geometry::Ellipse,
                    Role::Shape,
                );
                ellipse.line = Some(Outline {
                    color: [0x20, 0x20, 0x20, 0xff],
                    width: 12_700,
                });
                ellipse.rotation = 30.0;
                let picture = Shape::new(
                    Frame::new(500_000, 4_000_000, 1_000_000, 1_000_000),
                    Geometry::Rect,
                    Role::Picture,
                );
                Slide {
                    shapes: vec![
                        text_shape(
                            Frame::new(500_000, 300_000, 11_000_000, 1_200_000),
                            Role::Title,
                            &format!("第{}页 スライド", n + 1),
                            40.0,
                        ),
                        ellipse,
                        picture,
                    ],
                    hidden: n == 1,
                    ..Slide::default()
                }
            })
            .collect();
        Arc::new(Presentation {
            size: aster_slides::DEFAULT_SIZE,
            slides,
            ..Presentation::default()
        })
    }

    #[test]
    fn draws_slides_and_thumbnails() {
        let mut view = SlideView::new(presentation(40));
        view.resize(1600, 1000, 2.0);
        let pixels = view.render().data_as_u8_slice().to_vec();
        assert!(
            pixels
                .chunks(4)
                .any(|p| p == [0x1f, 0x1f, 0x1f, 0xff] || p[0] < 0x40)
        );
        assert!(!view.needs_render());
        assert!(view.next_slide());
        assert!(view.needs_render());
        view.render();
        assert_eq!(view.text(), "第2页 スライド");
    }

    #[test]
    fn navigation_keeps_the_thumbnail_in_view() {
        let mut view = SlideView::new(presentation(40));
        view.resize(1600, 1000, 2.0);
        assert!(!view.previous_slide());
        assert!(view.last_slide());
        assert_eq!(view.current(), 39);
        let (offset, max) = view.scroll_range();
        assert!(offset > 0.0 && (offset - max).abs() < 1.0);
        assert!(view.go_to(0));
        assert_eq!(view.scroll_range().0, 0.0);
        // A click on the second thumbnail.
        let pitch = view.thumb_pitch();
        assert!(view.press(100.0, THUMB_GAP * 2.0 + pitch * 1.5, false, 1));
        view.release();
        assert_eq!(view.current(), 1);
        // A click on the slide takes the keyboard from the thumbnails; on an
        // empty spot it changes nothing more.
        assert!(view.press(1500.0, 950.0, false, 1));
        assert!(!view.press(1500.0, 950.0, false, 1));
        // The wheel over the slide turns pages once it has moved enough.
        assert!(!view.wheel(1500.0, 50.0));
        assert!(view.wheel(1500.0, 200.0));
        assert_eq!(view.current(), 2);
        // Over the strip it scrolls the strip.
        assert!(view.wheel(10.0, 300.0));
    }

    #[test]
    fn an_empty_presentation_and_a_narrow_window() {
        let mut view = SlideView::new(Arc::new(Presentation::default()));
        view.resize(300, 200, 1.0);
        view.render();
        assert!(!view.next_slide());
        assert!(!view.press(10.0, 10.0, false, 1));
        assert_eq!(view.text(), "");
        let mut view = SlideView::new(presentation(3));
        // Narrower than the strip needs: only the slide.
        view.resize(400, 300, 1.0);
        assert!(!view.shows_strip());
        view.render();
        view.resize(1, 1, 3.0);
        view.render();
    }

    fn pixel(view: &mut SlideView, x: f64, y: f64) -> [u8; 4] {
        let width = usize::from(view.size().0);
        let pixmap = view.render();
        let data = pixmap.data_as_u8_slice();
        let at = (y as usize * width + x as usize) * 4;
        [data[at], data[at + 1], data[at + 2], data[at + 3]]
    }

    fn png(rgba: [u8; 4], width: u16, height: u16) -> Arc<[u8]> {
        let mut pixmap = Pixmap::new(width, height);
        for pixel in pixmap.data_mut() {
            *pixel = vello_cpu::peniko::color::PremulRgba8 {
                r: rgba[0],
                g: rgba[1],
                b: rgba[2],
                a: rgba[3],
            };
        }
        Arc::from(pixmap.into_png().unwrap())
    }

    /// Where a point of the slide (EMU) is on screen.
    fn on_page(view: &SlideView, x: i64, y: i64) -> (f64, f64) {
        let page = view.page_rect();
        let per = page.width() / view.presentation.size.0 as f64;
        (page.x0 + x as f64 * per, page.y0 + y as f64 * per)
    }

    #[test]
    fn backgrounds_fills_pictures_and_tables_are_drawn() {
        use aster_slides::{Picture, TableCell, TableRow};

        let mut card = Shape::new(
            Frame::new(1_000_000, 1_000_000, 2_000_000, 1_000_000),
            Geometry::Rect,
            Role::Shape,
        );
        card.fill = Some(Fill::Solid([0x10, 0x80, 0x40, 0xff]));
        let mut photo = Shape::new(
            Frame::new(4_000_000, 1_000_000, 2_000_000, 1_000_000),
            Geometry::Rect,
            Role::Picture,
        );
        // A one-colour picture with its left quarter cropped off still
        // fills the frame.
        photo.picture = Some(Picture {
            image: png([0, 0, 0xff, 0xff], 8, 4),
            crop: [0.25, 0.0, 0.0, 0.0],
        });
        let mut broken = Shape::new(
            Frame::new(7_000_000, 1_000_000, 2_000_000, 1_000_000),
            Geometry::Rect,
            Role::Picture,
        );
        broken.picture = Some(Picture {
            image: Arc::from(&b"not a picture"[..]),
            crop: [0.0; 4],
        });
        let mut table = Shape::new(
            Frame::new(1_000_000, 3_000_000, 4_000_000, 1_000_000),
            Geometry::Rect,
            Role::Table,
        );
        let cell = |fill: [u8; 4], text: &str| TableCell {
            fill: Some(Fill::Solid(fill)),
            text: Some(TextBody {
                paragraphs: vec![SlideParagraph {
                    runs: vec![Run {
                        color: Some([0xff, 0xff, 0xff, 0xff]),
                        ..Run::new(text, 14.0)
                    }],
                    ..SlideParagraph::default()
                }],
                ..TextBody::default()
            }),
            borders: [Some(Outline {
                color: [0, 0, 0, 0xff],
                width: 12_700,
            }); 4],
            ..TableCell::default()
        };
        table.table = Some(Table {
            columns: vec![2_000_000, 2_000_000],
            rows: vec![TableRow {
                height: 500_000,
                cells: vec![
                    cell([0xC0, 0x20, 0x20, 0xff], "表"),
                    cell([0x20, 0x20, 0xC0, 0xff], "表"),
                ],
            }],
        });
        let slide = Slide {
            shapes: vec![card, photo, broken, table],
            background: Some(Fill::Solid([0x30, 0x30, 0x30, 0xff])),
            notes: Some("メモ".into()),
            ..Slide::default()
        };
        let mut view = SlideView::new(Arc::new(Presentation {
            size: aster_slides::DEFAULT_SIZE,
            slides: vec![slide],
            ..Presentation::default()
        }));
        view.resize(1600, 1000, 1.0);
        let (x, y) = on_page(&view, 100_000, 6_000_000);
        assert_eq!(pixel(&mut view, x, y), [0x30, 0x30, 0x30, 0xff]);
        let (x, y) = on_page(&view, 2_000_000, 1_500_000);
        assert_eq!(pixel(&mut view, x, y), [0x10, 0x80, 0x40, 0xff]);
        let (x, y) = on_page(&view, 4_200_000, 1_500_000);
        assert_eq!(pixel(&mut view, x, y), [0, 0, 0xff, 0xff]);
        // A picture that cannot be decoded is a light box.
        let (x, y) = on_page(&view, 7_100_000, 1_100_000);
        assert_eq!(pixel(&mut view, x, y), PAGE_BOX);
        // Each table cell in its own fill, away from its text.
        let (x, y) = on_page(&view, 1_050_000, 3_450_000);
        assert_eq!(pixel(&mut view, x, y), [0xC0, 0x20, 0x20, 0xff]);
        let (x, y) = on_page(&view, 3_050_000, 3_450_000);
        assert_eq!(pixel(&mut view, x, y), [0x20, 0x20, 0xC0, 0xff]);
        // The thumbnail is drawn the same way, smaller.
        assert!(view.shows_strip());
    }

    #[test]
    fn a_gradient_background_runs_from_its_first_colour_to_its_last() {
        let slide = Slide {
            background: Some(Fill::Gradient {
                stops: vec![(0.0, [0xff, 0, 0, 0xff]), (1.0, [0, 0, 0xff, 0xff])],
                angle: 0.0,
            }),
            ..Slide::default()
        };
        let mut view = SlideView::new(Arc::new(Presentation {
            size: aster_slides::DEFAULT_SIZE,
            slides: vec![slide],
            ..Presentation::default()
        }));
        view.resize(1600, 1000, 1.0);
        let (x, y) = on_page(&view, 50_000, 3_000_000);
        let left = pixel(&mut view, x, y);
        let (x, y) = on_page(&view, 12_100_000, 3_000_000);
        let right = pixel(&mut view, x, y);
        assert!(left[0] > 0xF0 && left[2] < 0x10, "{left:?}");
        assert!(right[2] > 0xF0 && right[0] < 0x10, "{right:?}");
    }
}
