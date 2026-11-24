//! A PDF on screen: its pages one under the other, scrolled continuously,
//! with an optional strip of page thumbnails on the left. Read-only.
//!
//! The view does not render pages itself. It says which pages it needs and at
//! what scale ([`PdfView::wanted`]); a worker renders them and hands the
//! pixels back ([`PdfView::insert`]). Until a page arrives its place is a
//! blank page; while a page at a new zoom is on its way, the old rendering is
//! drawn stretched. Only pages near the window are kept.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use aster_text::{Align, Paragraph, Shaper};
use vello_cpu::kurbo::{Affine, Rect, Shape as _, Stroke};
use vello_cpu::peniko::color::PremulRgba8;
use vello_cpu::peniko::{Color, Fill as FillRule};
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

use crate::picture::draw_image;
use crate::preview::draw_text;
use crate::slide_view::simple;
use crate::text_surface::{HAN_FALLBACK_FAMILIES, Rgba8};

/// Width of the thumbnail strip and of a thumbnail, logical pixels.
const STRIP_WIDTH: f64 = 196.0;
const THUMB_WIDTH: f64 = 132.0;
const THUMB_GAP: f64 = 28.0;
/// Below this window width the strip is not shown.
const STRIP_MIN_WINDOW: f64 = 560.0;
/// Around and between pages, logical pixels.
const MARGIN: f64 = 24.0;
const GAP: f64 = 16.0;
/// Zoom steps, in percent.
const ZOOM_STEPS: [f32; 15] = [
    10.0, 25.0, 33.0, 50.0, 67.0, 75.0, 90.0, 100.0, 125.0, 150.0, 200.0, 300.0, 400.0, 600.0,
    800.0,
];
pub const ZOOM_MIN: f32 = 10.0;
pub const ZOOM_MAX: f32 = 800.0;
/// Pages kept rendered beyond those on screen, before and after.
const KEEP_AROUND: usize = 2;
const THUMBS_AROUND: usize = 12;
/// How far a rendering's scale may differ from the wanted one and still be
/// used as it is.
const SCALE_TOLERANCE: f32 = 0.02;
/// Scroll by this much (logical pixels) when the arrow keys are used.
pub const LINE_STEP: f64 = 48.0;

fn color(rgba: Rgba8) -> Color {
    Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3])
}

/// How large pages are drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    /// The widest page fills the width of the window.
    FitWidth,
    /// The largest page fits in the window whole.
    FitPage,
    /// Percent of actual size (100 % = one point per logical pixel).
    Percent(f32),
}

/// Colours around the pages.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfStyle {
    pub background: Rgba8,
    pub strip: Rgba8,
    pub hairline: Rgba8,
    pub secondary: Rgba8,
    pub accent: Rgba8,
}

impl Default for PdfStyle {
    fn default() -> Self {
        Self {
            background: [0xe9, 0xe9, 0xe9, 0xff],
            strip: [0xfa, 0xfa, 0xfa, 0xff],
            hairline: [0xd0, 0xd0, 0xd0, 0xff],
            secondary: [0x6b, 0x6b, 0x6b, 0xff],
            accent: [0x7c, 0x5c, 0xe0, 0xff],
        }
    }
}

/// A rendering of a page, and the scale (pixels per point) it was made at.
#[derive(Clone)]
struct Shot {
    scale: f32,
    pixmap: Arc<Pixmap>,
}

#[derive(Clone, Debug, PartialEq)]
struct Drawn {
    scroll: (f64, f64),
    strip_scroll: f64,
    size: (u16, u16),
    scale: f32,
    zoom: Zoom,
    strip: bool,
    current: usize,
    version: u64,
}

pub struct PdfView {
    /// Page sizes in points, as shown.
    pages: Vec<(f32, f32)>,
    /// Top of each page in layout units (points at zoom 1), and the total.
    tops: Vec<f64>,
    zoom: Zoom,
    /// Physical pixels.
    scroll_x: f64,
    scroll_y: f64,
    strip_scroll: f64,
    strip: bool,
    width: u16,
    height: u16,
    scale: f32,
    style: PdfStyle,
    shots: HashMap<usize, Shot>,
    thumbs: HashMap<usize, Shot>,
    failed: HashSet<usize>,
    shaper: Shaper,
    numbers: HashMap<(usize, bool), Arc<Paragraph>>,
    ctx: RenderContext,
    pixmap: Pixmap,
    resources: Resources,
    glyphs: Vec<Glyph>,
    drawn: Option<Drawn>,
    /// Bumped whenever a page arrives, so the next frame is drawn.
    version: u64,
}

impl PdfView {
    /// A view of pages of these sizes (points, as shown).
    pub fn new(pages: Vec<(f32, f32)>) -> Self {
        let mut shaper = Shaper::new();
        shaper.set_han_fallbacks(&HAN_FALLBACK_FAMILIES);
        let pages: Vec<(f32, f32)> = pages
            .into_iter()
            .map(|(w, h)| (w.max(1.0), h.max(1.0)))
            .collect();
        let mut view = Self {
            pages,
            tops: Vec::new(),
            zoom: Zoom::FitWidth,
            scroll_x: 0.0,
            scroll_y: 0.0,
            strip_scroll: 0.0,
            strip: true,
            width: 0,
            height: 0,
            scale: 1.0,
            style: PdfStyle::default(),
            shots: HashMap::new(),
            thumbs: HashMap::new(),
            failed: HashSet::new(),
            shaper,
            numbers: HashMap::new(),
            ctx: RenderContext::new(1, 1),
            pixmap: Pixmap::new(1, 1),
            resources: Resources::new(),
            glyphs: Vec::new(),
            drawn: None,
            version: 0,
        };
        view.lay_out();
        view
    }

    fn lay_out(&mut self) {
        let mut top = 0.0;
        self.tops = self
            .pages
            .iter()
            .map(|(_, h)| {
                let at = top;
                top += f64::from(*h);
                at
            })
            .collect();
        self.tops.push(top);
    }

    pub fn count(&self) -> usize {
        self.pages.len()
    }

    /// The text on the pages: not read yet, so empty.
    pub fn text(&self) -> String {
        String::new()
    }

    pub fn set_style(&mut self, style: PdfStyle) {
        if style != self.style {
            self.style = style;
            self.numbers.clear();
            self.drawn = None;
        }
    }

    pub fn resize(&mut self, width: u16, height: u16, scale: f32) {
        if (width, height) == (self.width, self.height) && scale == self.scale {
            return;
        }
        let anchor = self.anchor();
        if scale != self.scale {
            self.numbers.clear();
        }
        self.width = width;
        self.height = height;
        self.scale = scale;
        self.ctx = RenderContext::new(width.max(1), height.max(1));
        self.pixmap = Pixmap::new(width.max(1), height.max(1));
        self.drawn = None;
        self.restore(anchor);
        self.clamp_strip();
    }

    pub fn size(&self) -> (u16, u16, f32) {
        (self.width, self.height, self.scale)
    }

    fn s(&self) -> f64 {
        f64::from(self.scale)
    }

    fn shows_strip(&self) -> bool {
        self.strip && f64::from(self.width) / self.s() >= STRIP_MIN_WINDOW && self.count() > 0
    }

    /// Whether the thumbnail strip is asked for (it is still hidden when the
    /// window is narrow).
    pub fn strip_on(&self) -> bool {
        self.strip
    }

    pub fn set_strip(&mut self, on: bool) {
        if on != self.strip {
            let anchor = self.anchor();
            self.strip = on;
            self.restore(anchor);
        }
    }

    fn strip_width(&self) -> f64 {
        if self.shows_strip() {
            STRIP_WIDTH * self.s()
        } else {
            0.0
        }
    }

    /// The width pages are laid out in, physical pixels.
    fn column(&self) -> (f64, f64) {
        let left = self.strip_width();
        (left, (f64::from(self.width) - left).max(1.0))
    }

    fn widest(&self) -> f64 {
        self.pages
            .iter()
            .map(|(w, _)| f64::from(*w))
            .fold(1.0, f64::max)
    }

    fn tallest(&self) -> f64 {
        self.pages
            .iter()
            .map(|(_, h)| f64::from(*h))
            .fold(1.0, f64::max)
    }

    /// Physical pixels per point.
    pub fn pixels_per_point(&self) -> f64 {
        let s = self.s();
        let (_, column) = self.column();
        let margin = MARGIN * s;
        let fit_width = ((column - 2.0 * margin) / self.widest()).max(0.01);
        let fit_height = ((f64::from(self.height) - 2.0 * margin) / self.tallest()).max(0.01);
        let ppp = match self.zoom {
            Zoom::FitWidth => fit_width,
            Zoom::FitPage => fit_width.min(fit_height),
            Zoom::Percent(p) => f64::from(p) / 100.0 * s,
        };
        ppp.clamp(
            f64::from(ZOOM_MIN) / 100.0 * s,
            f64::from(ZOOM_MAX) / 100.0 * s,
        )
    }

    /// The zoom as a percentage of actual size, whatever mode it is in.
    pub fn zoom_percent(&self) -> f32 {
        (self.pixels_per_point() / self.s() * 100.0).round() as f32
    }

    pub fn zoom(&self) -> Zoom {
        self.zoom
    }

    pub fn set_zoom(&mut self, zoom: Zoom) -> bool {
        let zoom = match zoom {
            Zoom::Percent(p) => Zoom::Percent(p.clamp(ZOOM_MIN, ZOOM_MAX)),
            other => other,
        };
        if zoom == self.zoom {
            return false;
        }
        let anchor = self.anchor();
        self.zoom = zoom;
        self.restore(anchor);
        true
    }

    /// The next step up from the current zoom.
    pub fn zoom_in(&mut self) -> bool {
        let now = self.zoom_percent();
        let next = ZOOM_STEPS
            .iter()
            .copied()
            .find(|p| *p > now + 0.5)
            .unwrap_or(ZOOM_MAX);
        self.set_zoom(Zoom::Percent(next))
    }

    pub fn zoom_out(&mut self) -> bool {
        let now = self.zoom_percent();
        let next = ZOOM_STEPS
            .iter()
            .rev()
            .copied()
            .find(|p| *p < now - 0.5)
            .unwrap_or(ZOOM_MIN);
        self.set_zoom(Zoom::Percent(next))
    }

    /// Top of page `index` on the scrolled-through canvas, physical pixels.
    fn page_top(&self, index: usize) -> f64 {
        let s = self.s();
        MARGIN * s + self.tops[index] * self.pixels_per_point() + GAP * s * index as f64
    }

    fn content_height(&self) -> f64 {
        let s = self.s();
        let n = self.count();
        2.0 * MARGIN * s
            + self.tops[n] * self.pixels_per_point()
            + GAP * s * n.saturating_sub(1) as f64
    }

    fn content_width(&self) -> f64 {
        2.0 * MARGIN * self.s() + self.widest() * self.pixels_per_point()
    }

    /// Where page `index` is on screen, physical pixels.
    fn page_rect(&self, index: usize) -> Rect {
        let ppp = self.pixels_per_point();
        let (left, column) = self.column();
        let (w, h) = self.pages[index];
        let (w, h) = (f64::from(w) * ppp, f64::from(h) * ppp);
        let content = self.content_width();
        let x = if content <= column {
            left + (column - w) / 2.0
        } else {
            left + (content - w) / 2.0 - self.scroll_x
        };
        let y = self.page_top(index) - self.scroll_y;
        Rect::new(x.round(), y.round(), (x + w).round(), (y + h).round())
    }

    /// The first page whose bottom is below `y` (canvas pixels).
    fn page_at(&self, y: f64) -> usize {
        let n = self.count();
        if n == 0 {
            return 0;
        }
        let ppp = self.pixels_per_point();
        let s = self.s();
        // Binary search over page bottoms.
        let mut low = 0;
        let mut high = n - 1;
        while low < high {
            let mid = (low + high) / 2;
            let bottom = self.page_top(mid) + f64::from(self.pages[mid].1) * ppp + GAP * s / 2.0;
            if bottom <= y {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low
    }

    /// Pages on screen, first to last.
    fn visible(&self) -> std::ops::Range<usize> {
        if self.count() == 0 {
            return 0..0;
        }
        let first = self.page_at(self.scroll_y);
        let last = self.page_at(self.scroll_y + f64::from(self.height));
        first..last + 1
    }

    /// The page the reader is on: the one covering the middle of the window,
    /// or the last page once the end is reached.
    pub fn current(&self) -> usize {
        if self.count() == 0 {
            return 0;
        }
        let (offset, max) = self.scroll_range();
        if max > 0.0 && offset >= max - 1.0 {
            return self.count() - 1;
        }
        self.page_at(self.scroll_y + f64::from(self.height) / 3.0)
    }

    /// Page and how far down it (0–1) the top of the window is, to keep the
    /// place across zooms and resizes.
    fn anchor(&self) -> Option<(usize, f64)> {
        if self.count() == 0 || self.width == 0 || self.scroll_y <= 0.0 {
            return None;
        }
        let page = self.page_at(self.scroll_y);
        let height = f64::from(self.pages[page].1) * self.pixels_per_point();
        let into = (self.scroll_y - self.page_top(page)) / height.max(1.0);
        Some((page, into))
    }

    fn restore(&mut self, anchor: Option<(usize, f64)>) {
        let Some((page, into)) = anchor else {
            self.clamp();
            return;
        };
        let height = f64::from(self.pages[page].1) * self.pixels_per_point();
        self.scroll_y = self.page_top(page) + into * height;
        self.clamp();
    }

    fn clamp(&mut self) {
        let max_y = (self.content_height() - f64::from(self.height)).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
        let (_, column) = self.column();
        let max_x = (self.content_width() - column).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
    }

    /// Height of a thumbnail plus its gap, for page `index`, physical pixels.
    fn thumb_size(&self, index: usize) -> (f64, f64) {
        let (w, h) = self.pages[index];
        let (w, h) = (f64::from(w), f64::from(h));
        let s = self.s();
        // Fit in a square THUMB_WIDTH a side, so landscape pages stay small.
        let fit = (THUMB_WIDTH / w).min(THUMB_WIDTH * 1.42 / h) * s;
        (w * fit, h * fit)
    }

    fn thumb_pitch(&self) -> f64 {
        (THUMB_WIDTH * 1.42 + THUMB_GAP) * self.s()
    }

    fn strip_height(&self) -> f64 {
        self.thumb_pitch() * self.count() as f64 + THUMB_GAP * self.s() / 2.0
    }

    fn clamp_strip(&mut self) {
        let max = (self.strip_height() - f64::from(self.height)).max(0.0);
        self.strip_scroll = self.strip_scroll.clamp(0.0, max);
    }

    fn thumb_rect(&self, index: usize) -> Rect {
        let s = self.s();
        let (w, h) = self.thumb_size(index);
        let cell_top = THUMB_GAP * s / 2.0 + self.thumb_pitch() * index as f64 - self.strip_scroll;
        let cell_h = THUMB_WIDTH * 1.42 * s;
        let x = (STRIP_WIDTH * s - w) / 2.0;
        let y = cell_top + (cell_h - h) / 2.0;
        Rect::new(x.round(), y.round(), (x + w).round(), (y + h).round())
    }

    /// Show page `index` at the top of the window. Returns whether the view
    /// moved.
    pub fn go_to(&mut self, index: usize) -> bool {
        if self.count() == 0 {
            return false;
        }
        let index = index.min(self.count() - 1);
        let before = self.scroll_y;
        self.scroll_y = self.page_top(index) - MARGIN * self.s() / 2.0;
        self.clamp();
        self.reveal_thumb(index);
        self.scroll_y != before
    }

    /// Show a point of page `index` (in points from its top-left corner)
    /// near the top of the window.
    pub fn go_to_point(&mut self, index: usize, y: f32) -> bool {
        if index >= self.count() {
            return false;
        }
        let before = self.scroll_y;
        let ppp = self.pixels_per_point();
        self.scroll_y = self.page_top(index) + f64::from(y) * ppp - MARGIN * self.s();
        self.clamp();
        self.reveal_thumb(index);
        self.scroll_y != before
    }

    fn reveal_thumb(&mut self, index: usize) {
        let pitch = self.thumb_pitch();
        let top = pitch * index as f64;
        let bottom = top + pitch;
        if top < self.strip_scroll {
            self.strip_scroll = top;
        } else if bottom > self.strip_scroll + f64::from(self.height) {
            self.strip_scroll = bottom - f64::from(self.height) + THUMB_GAP * self.s();
        }
        self.clamp_strip();
    }

    pub fn next_page(&mut self) -> bool {
        self.go_to(self.current() + 1)
    }

    pub fn previous_page(&mut self) -> bool {
        let current = self.current();
        // On a page whose top is above the window, go to its top first.
        if self.page_top(current) < self.scroll_y - 1.0 {
            return self.go_to(current);
        }
        current > 0 && self.go_to(current - 1)
    }

    pub fn first_page(&mut self) -> bool {
        self.go_to(0)
    }

    pub fn last_page(&mut self) -> bool {
        self.go_to(self.count().saturating_sub(1))
    }

    /// Scroll by a window's height, less a little to keep context.
    pub fn page_down(&mut self, down: bool) -> bool {
        let step = (f64::from(self.height) - 2.0 * LINE_STEP * self.s()).max(LINE_STEP * self.s());
        self.scroll_by(if down { step } else { -step })
    }

    /// A click (physical pixels): on a thumbnail, goes to that page.
    pub fn press(&mut self, x: f64, y: f64) -> bool {
        if x >= self.strip_width() || self.count() == 0 {
            return false;
        }
        let index =
            ((y + self.strip_scroll - THUMB_GAP * self.s() / 4.0) / self.thumb_pitch()).floor();
        if index < 0.0 {
            return false;
        }
        self.go_to((index as usize).min(self.count() - 1)) || true
    }

    /// Scroll the pages by physical pixels.
    pub fn scroll_by(&mut self, delta: f64) -> bool {
        let before = self.scroll_y;
        self.scroll_y += delta;
        self.clamp();
        self.scroll_y != before
    }

    /// The wheel or trackpad at `x` (physical pixels): over the strip it
    /// scrolls the strip, elsewhere the pages, both ways.
    pub fn wheel(&mut self, x: f64, dx: f64, dy: f64) -> bool {
        if x < self.strip_width() {
            let before = self.strip_scroll;
            self.strip_scroll += dy;
            self.clamp_strip();
            return self.strip_scroll != before;
        }
        let before = (self.scroll_x, self.scroll_y);
        self.scroll_x += dx;
        self.scroll_y += dy;
        self.clamp();
        (self.scroll_x, self.scroll_y) != before
    }

    /// Scroll offset and its bound, physical pixels.
    pub fn scroll_range(&self) -> (f64, f64) {
        (
            self.scroll_y,
            (self.content_height() - f64::from(self.height)).max(0.0),
        )
    }

    pub fn set_scroll(&mut self, offset: f64) -> bool {
        let before = self.scroll_y;
        self.scroll_y = offset;
        self.clamp();
        self.scroll_y != before
    }

    /// Thumbnail scale for page `index`: pixels per point.
    fn thumb_scale(&self, index: usize) -> f32 {
        let (w, _) = self.thumb_size(index);
        (w / f64::from(self.pages[index].0)) as f32
    }

    /// Pages to render, most needed first, as (page, pixels per point): those
    /// on screen at the current zoom, one more each side, then the visible
    /// thumbnails. Pages already rendered at that scale are left out.
    pub fn wanted(&self) -> Vec<(usize, f32)> {
        let mut wanted = Vec::new();
        if self.count() == 0 || self.width == 0 {
            return wanted;
        }
        let ppp = self.pixels_per_point() as f32;
        let visible = self.visible();
        let around = visible.start.saturating_sub(1)..(visible.end + 1).min(self.count());
        let current = self.current();
        let mut pages: Vec<usize> = around.collect();
        pages.sort_by_key(|p| p.abs_diff(current));
        for page in pages {
            if self.failed.contains(&page) {
                continue;
            }
            let fresh = self
                .shots
                .get(&page)
                .is_some_and(|shot| (shot.scale / ppp - 1.0).abs() <= SCALE_TOLERANCE);
            if !fresh {
                wanted.push((page, ppp));
            }
        }
        if self.shows_strip() {
            for page in self.visible_thumbs() {
                if self.failed.contains(&page) || self.thumbs.contains_key(&page) {
                    continue;
                }
                wanted.push((page, self.thumb_scale(page)));
            }
        }
        wanted
    }

    fn visible_thumbs(&self) -> std::ops::Range<usize> {
        let pitch = self.thumb_pitch();
        let first = (self.strip_scroll / pitch).floor().max(0.0) as usize;
        let last = ((self.strip_scroll + f64::from(self.height)) / pitch).ceil() as usize;
        first.min(self.count())..(last + 1).min(self.count())
    }

    /// A rendered page arrives: premultiplied RGBA, `width * height * 4`
    /// bytes, rendered at `scale` pixels per point. Returns whether it shows.
    pub fn insert(
        &mut self,
        page: usize,
        scale: f32,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> bool {
        if page >= self.count() {
            return false;
        }
        let (Ok(w), Ok(h)) = (u16::try_from(width), u16::try_from(height)) else {
            return false;
        };
        if pixels.len() != usize::from(w) * usize::from(h) * 4 || w == 0 || h == 0 {
            return false;
        }
        let pixels: Vec<PremulRgba8> = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| PremulRgba8 {
                r: p[0],
                g: p[1],
                b: p[2],
                a: p[3],
            })
            .collect();
        let shot = Shot {
            scale,
            pixmap: Arc::new(Pixmap::from_parts_with_opacity(pixels, w, h, false)),
        };
        let is_thumb =
            self.shows_strip() && (scale / self.thumb_scale(page) - 1.0).abs() <= SCALE_TOLERANCE;
        let main = (scale / self.pixels_per_point() as f32 - 1.0).abs() <= SCALE_TOLERANCE;
        if is_thumb && !main {
            self.thumbs.insert(page, shot);
        } else {
            // A page at another scale than now (the zoom changed since it
            // was asked for) is still better than a blank page.
            let better = self.shots.get(&page).is_none_or(|old| {
                let want = self.pixels_per_point() as f32;
                (scale - want).abs() <= (old.scale - want).abs()
            });
            if better {
                self.shots.insert(page, shot);
            }
        }
        self.forget_far();
        self.version += 1;
        true
    }

    /// A page that cannot be drawn: shown blank, not asked for again.
    pub fn mark_failed(&mut self, page: usize) {
        if self.failed.insert(page) {
            self.version += 1;
        }
    }

    /// Drop renderings far from what is on screen.
    fn forget_far(&mut self) {
        let visible = self.visible();
        let keep = visible.start.saturating_sub(KEEP_AROUND)..visible.end + KEEP_AROUND;
        self.shots.retain(|page, _| keep.contains(page));
        let thumbs = self.visible_thumbs();
        let keep = thumbs.start.saturating_sub(THUMBS_AROUND)..thumbs.end + THUMBS_AROUND;
        self.thumbs.retain(|page, _| keep.contains(page));
    }

    pub fn needs_render(&self) -> bool {
        self.drawn.as_ref() != Some(&self.state())
    }

    fn state(&self) -> Drawn {
        Drawn {
            scroll: (self.scroll_x, self.scroll_y),
            strip_scroll: self.strip_scroll,
            size: (self.width, self.height),
            scale: self.scale,
            zoom: self.zoom,
            strip: self.strip,
            current: self.current(),
            version: self.version,
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
        self.ctx.set_fill_rule(FillRule::NonZero);
        self.ctx.set_transform(Affine::IDENTITY);
        self.ctx.set_paint(color(style.background));
        self.ctx.fill_rect(&Rect::new(0.0, 0.0, w, h));

        let hairline = s.round().max(1.0);
        for index in self.visible() {
            let rect = self.page_rect(index);
            // A thin edge, so white pages stand out from a light background.
            self.ctx.set_paint(color(style.hairline));
            self.ctx.fill_rect(&rect.inflate(hairline, hairline));
            self.ctx.set_paint(Color::WHITE);
            self.ctx.fill_rect(&rect);
            if let Some(shot) = self.shots.get(&index).cloned() {
                draw_image(&mut self.ctx, &shot.pixmap, rect, None, Affine::IDENTITY);
            } else if let Some(thumb) = self.thumbs.get(&index).cloned() {
                draw_image(&mut self.ctx, &thumb.pixmap, rect, None, Affine::IDENTITY);
            }
        }

        if self.shows_strip() {
            let strip = self.strip_width();
            self.ctx.set_paint(color(style.strip));
            self.ctx.fill_rect(&Rect::new(0.0, 0.0, strip, h));
            self.ctx.set_paint(color(style.hairline));
            self.ctx
                .fill_rect(&Rect::new(strip - hairline, 0.0, strip, h));
            self.ctx
                .push_clip_layer(&Rect::new(0.0, 0.0, strip, h).to_path(0.1));
            let current = self.current();
            for index in self.visible_thumbs() {
                let rect = self.thumb_rect(index);
                self.ctx.set_paint(Color::WHITE);
                self.ctx.fill_rect(&rect);
                let shot = self
                    .thumbs
                    .get(&index)
                    .or_else(|| self.shots.get(&index))
                    .cloned();
                if let Some(shot) = shot {
                    draw_image(&mut self.ctx, &shot.pixmap, rect, None, Affine::IDENTITY);
                }
                let selected = index == current;
                if selected {
                    let border = (2.0 * s).round();
                    self.ctx.set_paint(color(style.accent));
                    self.ctx.set_stroke(Stroke::new(border));
                    self.ctx.stroke_rect(&rect.inflate(border, border));
                } else {
                    self.ctx.set_paint(color(style.hairline));
                    self.ctx.set_stroke(Stroke::new(hairline));
                    self.ctx
                        .stroke_rect(&rect.inflate(hairline / 2.0, hairline / 2.0));
                }
                let number = self.number(index, selected);
                let x = (STRIP_WIDTH * s - f64::from(number.width())) / 2.0;
                self.paint(&number, x, rect.y1 + 4.0 * s);
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
        if self.numbers.len() > 512 {
            self.numbers.clear();
        }
        let color = if selected {
            self.style.accent
        } else {
            self.style.secondary
        };
        let paragraph = Arc::new(simple(
            &mut self.shaper,
            &(index + 1).to_string(),
            11.0,
            color,
            self.scale,
            Align::Center,
        ));
        self.numbers.insert((index, selected), paragraph.clone());
        paragraph
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter(count: usize) -> PdfView {
        let mut view = PdfView::new(vec![(612.0, 792.0); count]);
        view.resize(1000, 800, 1.0);
        view
    }

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        rgba.repeat((width * height) as usize)
    }

    #[test]
    fn fit_width_fills_the_column() {
        let view = letter(3);
        let column = 1000.0 - STRIP_WIDTH;
        let ppp = view.pixels_per_point();
        assert!((612.0 * ppp - (column - 2.0 * MARGIN)).abs() < 0.01);
        let rect = view.page_rect(0);
        assert_eq!(rect.y0, MARGIN);
        assert!((rect.x0 - (STRIP_WIDTH + MARGIN)).abs() <= 1.0);
    }

    #[test]
    fn only_pages_near_the_window_are_wanted() {
        let mut view = letter(100);
        let wanted = view.wanted();
        let pages: Vec<usize> = wanted.iter().map(|(p, _)| *p).collect();
        // Page 0 is on screen and page 1 is the one after; then thumbnails.
        assert_eq!(&pages[..2], &[0, 1]);
        assert!(wanted.len() < 20, "{wanted:?}");
        view.go_to(50);
        assert_eq!(view.current(), 50);
        let main: Vec<usize> = view
            .wanted()
            .iter()
            .filter(|(_, s)| (*s as f64 - view.pixels_per_point()).abs() < 1e-3)
            .map(|(p, _)| *p)
            .collect();
        assert_eq!(main[0], 50);
        assert!(main.iter().all(|p| (48..=52).contains(p)), "{main:?}");
    }

    #[test]
    fn rendered_pages_are_not_wanted_again_until_the_zoom_changes() {
        let mut view = letter(2);
        let ppp = view.pixels_per_point() as f32;
        let (w, h) = ((612.0 * ppp) as u32, (792.0 * ppp) as u32);
        assert!(view.insert(0, ppp, w, h, solid(w, h, [255, 0, 0, 255])));
        assert!(!view.wanted().iter().any(|(p, s)| *p == 0 && *s == ppp));
        view.set_zoom(Zoom::Percent(200.0));
        assert!(view.wanted().contains(&(0, 2.0)));
        // The old rendering is drawn stretched meanwhile.
        let pixmap = view.render();
        let rect_center = |p: &Pixmap, x: u16, y: u16| p.sample(x, y);
        let c = rect_center(pixmap, 600, 400);
        assert_eq!((c.r, c.g, c.b), (255, 0, 0));
    }

    #[test]
    fn zoom_steps_and_limits() {
        let mut view = letter(1);
        view.set_zoom(Zoom::Percent(100.0));
        assert_eq!(view.zoom_percent(), 100.0);
        view.zoom_in();
        assert_eq!(view.zoom_percent(), 125.0);
        view.zoom_out();
        view.zoom_out();
        assert_eq!(view.zoom_percent(), 90.0);
        view.set_zoom(Zoom::Percent(5000.0));
        assert_eq!(view.zoom_percent(), ZOOM_MAX);
        for _ in 0..30 {
            view.zoom_out();
        }
        assert_eq!(view.zoom_percent(), ZOOM_MIN);
        view.set_zoom(Zoom::FitPage);
        let ppp = view.pixels_per_point();
        assert!(792.0 * ppp <= 800.0 - 2.0 * MARGIN + 0.01);
    }

    #[test]
    fn zooming_keeps_the_place() {
        let mut view = letter(40);
        view.go_to(20);
        view.scroll_by(100.0);
        let before = view.current();
        view.zoom_in();
        view.zoom_in();
        assert_eq!(view.current(), before);
        view.set_zoom(Zoom::Percent(50.0));
        assert!(view.current().abs_diff(before) <= 1);
    }

    #[test]
    fn clicking_a_thumbnail_goes_to_its_page() {
        let mut view = letter(30);
        let target = view.thumb_rect(3);
        assert!(view.press(target.center().x, target.center().y));
        assert_eq!(view.current(), 3);
        // Clicks right of the strip do nothing.
        assert!(!view.press(STRIP_WIDTH + 10.0, 10.0));
    }

    #[test]
    fn the_strip_is_hidden_in_narrow_windows_and_on_request() {
        let mut view = letter(3);
        assert!(view.shows_strip());
        view.resize(400, 800, 1.0);
        assert!(!view.shows_strip());
        view.resize(1000, 800, 1.0);
        view.set_strip(false);
        assert!(!view.shows_strip());
        assert!(
            view.wanted()
                .iter()
                .all(|(_, s)| (*s as f64 - view.pixels_per_point()).abs() < 1e-3)
        );
    }

    #[test]
    fn wide_pages_scroll_sideways_when_zoomed() {
        let mut view = letter(1);
        view.set_zoom(Zoom::Percent(400.0));
        let before = view.page_rect(0).x0;
        assert!(view.wheel(500.0, 300.0, 0.0));
        assert_eq!(view.page_rect(0).x0, before - 300.0);
        // At fit width there is nothing to scroll sideways.
        view.set_zoom(Zoom::FitWidth);
        assert!(!view.wheel(500.0, 300.0, 0.0));
    }

    #[test]
    fn far_pages_are_forgotten() {
        let mut view = letter(50);
        let ppp = view.pixels_per_point() as f32;
        let (w, h) = ((612.0 * ppp) as u32, (792.0 * ppp) as u32);
        view.insert(0, ppp, w, h, solid(w, h, [0, 0, 0, 255]));
        view.go_to(40);
        view.insert(40, ppp, w, h, solid(w, h, [0, 0, 0, 255]));
        assert!(!view.shots.contains_key(&0));
        assert!(view.shots.contains_key(&40));
        // Wrong sizes are refused.
        assert!(!view.insert(1, ppp, 10, 10, vec![0; 12]));
    }

    #[test]
    fn the_last_page_is_current_at_the_end() {
        let mut view = PdfView::new(vec![(612.0, 792.0), (612.0, 200.0)]);
        view.resize(1000, 800, 1.0);
        view.set_scroll(f64::MAX);
        assert_eq!(view.current(), 1);
        assert!(view.previous_page());
        assert_eq!(view.current(), 0);
    }
}
