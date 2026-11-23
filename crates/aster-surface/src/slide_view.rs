//! A presentation on screen: a strip of slide thumbnails on the left and the
//! current slide, fitted, on the right. Read-only.
//!
//! A slide is drawn as a white page whatever the theme, since its colours
//! are not read yet: the text of its shapes, the outlines of autoshapes and
//! connectors, and labelled boxes where pictures, tables and charts are.
//! Laid-out slides are kept per size, so moving through the slides and
//! scrolling the strip do not lay text out again.

use std::collections::HashMap;
use std::sync::Arc;

use aster_slides::{
    Align as SlideAlign, Anchor, EMU_PER_POINT, Geometry, Presentation, Role, Shape, Slide,
};
use aster_text::{Align, Family, Paragraph, Rules, Shaper, Span, Spec, Style as TextStyle};
use vello_cpu::kurbo::{Affine, BezPath, Ellipse, Line, Rect, RoundedRect, Shape as _, Stroke};
use vello_cpu::peniko::{Color, Fill};
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

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
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Drawn {
    current: usize,
    strip_scroll: f64,
    size: (u16, u16),
    scale: f32,
}

/// A slide's texts laid out for one page width.
struct LaidSlide {
    shapes: Vec<LaidShape>,
}

struct LaidShape {
    /// Index into the slide's shapes.
    index: usize,
    /// Paragraph boxes: position inside the text area (physical pixels from
    /// the shape's top-left) and the laid-out text.
    texts: Vec<(f64, f64, Paragraph)>,
    label: Option<Paragraph>,
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
        }
    }

    pub fn presentation(&self) -> &Arc<Presentation> {
        &self.presentation
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
        f64::from(self.width) / self.s() >= STRIP_MIN_WINDOW
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
        self.current = index;
        self.wheel = 0.0;
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

    /// A click (physical pixels): on a thumbnail, shows that slide.
    pub fn press(&mut self, x: f64, y: f64) -> bool {
        if x >= self.strip_width() || self.count() == 0 {
            return false;
        }
        let y = y + self.strip_scroll - THUMB_GAP * self.s() / 2.0;
        let index = (y / self.thumb_pitch()).floor();
        if index < 0.0 {
            return false;
        }
        self.go_to((index as usize).min(self.count() - 1))
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
        let margin = MARGIN * self.s();
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
        }
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
        self.ctx.set_fill_rule(Fill::NonZero);
        self.ctx.set_transform(Affine::IDENTITY);
        self.ctx.set_paint(color(style.background));
        self.ctx.fill_rect(&Rect::new(0.0, 0.0, w, h));

        if self.count() > 0 {
            let page = self.page_rect();
            self.draw_slide(self.current, page, true);
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
            let texts = match &shape.text {
                Some(_) => lay_out_text(&mut self.shaper, shape, px_per_emu),
                None => Vec::new(),
            };
            let label = match shape.role {
                Role::Picture => Some(self.style.picture.clone()),
                Role::Table => Some(self.style.table.clone()),
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
            if texts.is_empty() && label.is_none() && !shape.outlined {
                continue;
            }
            shapes.push(LaidShape {
                index: i,
                texts,
                label,
            });
        }
        let laid = Arc::new(LaidSlide { shapes });
        self.laid.insert(key, laid.clone());
        laid
    }

    fn draw_slide(&mut self, index: usize, page: Rect, main: bool) {
        let laid = self.laid_slide(index, page);
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
        let line_width = if main {
            (1.25 * s).max(1.0)
        } else {
            (px_per_emu * 9_525.0).max(0.5)
        };
        for laid_shape in &laid.shapes {
            let shape = &slide.shapes[laid_shape.index];
            let frame = Rect::new(
                page.x0 + shape.frame.x as f64 * px_per_emu,
                page.y0 + shape.frame.y as f64 * px_per_emu,
                page.x0 + (shape.frame.x + shape.frame.width) as f64 * px_per_emu,
                page.y0 + (shape.frame.y + shape.frame.height) as f64 * px_per_emu,
            );
            let transform = shape_transform(shape, frame);
            // Placeholder boxes for what is not drawn yet.
            if let Some(label) = &laid_shape.label {
                self.ctx.set_transform(transform);
                self.ctx.set_paint(color(PAGE_BOX));
                self.ctx.fill_rect(&frame);
                self.ctx.set_paint(color(PAGE_LINE));
                self.ctx.set_stroke(Stroke::new(line_width));
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
            if shape.outlined {
                self.ctx.set_transform(transform);
                self.ctx.set_paint(color(PAGE_LINE));
                self.ctx.set_stroke(Stroke::new(line_width));
                self.ctx
                    .stroke_path(&outline(&shape.geometry, frame, shape));
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

/// Lay out a shape's paragraphs inside its frame: positions relative to the
/// frame's top-left, physical pixels.
fn lay_out_text(shaper: &mut Shaper, shape: &Shape, px_per_emu: f64) -> Vec<(f64, f64, Paragraph)> {
    let Some(body) = &shape.text else {
        return Vec::new();
    };
    let [left, top, right, bottom] = body.insets.map(|inset| inset as f64 * px_per_emu);
    let width = shape.frame.width as f64 * px_per_emu - left - right;
    let height = shape.frame.height as f64 * px_per_emu - top - bottom;
    let points_to_px = EMU_PER_POINT * px_per_emu * f64::from(body.font_scale);
    let mut out: Vec<(f64, f64, Paragraph)> = Vec::new();
    let mut y = 0.0;
    for paragraph in &body.paragraphs {
        let margin = paragraph.margin as f64 * px_per_emu;
        let indent = paragraph.indent as f64 * px_per_emu;
        let text: String = paragraph.runs.iter().map(|run| run.text.as_str()).collect();
        let size = (f64::from(paragraph.size()) * points_to_px) as f32;
        if text.is_empty() {
            // An empty line still takes its height.
            y += f64::from(size * LINE_HEIGHT);
            continue;
        }
        let mut spans = Vec::with_capacity(paragraph.runs.len());
        let mut start = 0;
        for run in &paragraph.runs {
            let end = start + run.text.len();
            spans.push(Span {
                range: start..end,
                style: style(
                    (f64::from(run.size) * points_to_px) as f32,
                    run.bold,
                    run.italic,
                    run.underline,
                ),
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
            let mark = shaper.lay_out(&Spec {
                text: bullet,
                base: style(first_size, false, false, false),
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
        out.push((x, top + y, laid));
        y += h;
    }
    // Vertical anchoring of the whole block.
    let shift = match body.anchor {
        Anchor::Top => 0.0,
        Anchor::Middle => ((height - y) / 2.0).max(f64::MIN),
        Anchor::Bottom => height - y,
    };
    if shift != 0.0 {
        for (_, py, _) in &mut out {
            *py += shift;
        }
    }
    out
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
                ellipse.outlined = true;
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
                }
            })
            .collect();
        Arc::new(Presentation {
            size: aster_slides::DEFAULT_SIZE,
            slides,
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
        assert!(view.press(100.0, THUMB_GAP * 2.0 + pitch * 1.5));
        assert_eq!(view.current(), 1);
        // Clicks on the slide itself do nothing.
        assert!(!view.press(1500.0, 500.0));
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
        assert!(!view.press(10.0, 10.0));
        assert_eq!(view.text(), "");
        let mut view = SlideView::new(presentation(3));
        // Narrower than the strip needs: only the slide.
        view.resize(400, 300, 1.0);
        assert!(!view.shows_strip());
        view.render();
        view.resize(1, 1, 3.0);
        view.render();
    }
}
