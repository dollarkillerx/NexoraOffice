//! A Writer document on screen: its pages one under the other on a canvas,
//! scrolled continuously, with a caret, a selection, find and an outline.
//!
//! The view owns the [`Document`], a [`Layouter`] and the [`Layout`] it made.
//! Layout is in points and does not depend on the zoom or the window: only
//! painting scales. Pages are painted by [`DocPainter`] into pixmaps that are
//! kept while they are near the window, so scrolling moves pixels that are
//! already there; the selection, find marks and the caret are drawn over them
//! each frame. A page too large to keep whole at a high zoom is drawn as a
//! band around what is visible instead.
//!
//! The view does not change the document in this version. Positions are
//! [`Pos`]es: a paragraph [`Path`] and a byte offset on a `char` boundary,
//! always of a paragraph of the body (table cells included). Headers,
//! footers, notes and text boxes are drawn but cannot be pointed at.
//!
//! Building a view lays the document out, which for a large document takes a
//! few hundred milliseconds: do it on a worker thread, [`DocView`] is `Send`.

mod editing;
mod search;
#[cfg(test)]
mod tests;
mod text;

use std::collections::HashMap;
use std::sync::Arc;

use aster_layout::{Item, Layout, Layouter, OutlineEntry, Page, Rect as PageRect};
use aster_text::Shaper;
use aster_writer::{Document, Path, Pos};
use vello_cpu::kurbo::{Affine, Rect};
use vello_cpu::peniko::{Color, Fill as FillRule};
use vello_cpu::{Pixmap, RenderContext, Resources};

use crate::doc_paint::DocPainter;
use crate::pdf_view::ZOOM_STEPS;
use crate::picture::draw_image;
use crate::text_surface::{HAN_FALLBACK_FAMILIES, Rgba8};

pub use crate::pdf_view::{LINE_STEP, ZOOM_MAX, ZOOM_MIN, Zoom};
pub use editing::Move;
pub use search::{DocQuery, MAX_MATCHES, Match};
pub use text::text_between;

/// Around and between pages, logical pixels.
const MARGIN: f64 = 24.0;
const GAP: f64 = 16.0;
/// Pages kept painted beyond those on screen, before and after.
const KEEP_AROUND: usize = 1;
/// Most bytes of painted pages a view holds.
pub const PIXEL_BUDGET: usize = 256 << 20;
/// A page with more pixels than this is not kept whole.
const MAX_SHOT_PIXELS: f64 = 24_000_000.0;

fn color(rgba: Rgba8) -> Color {
    Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3])
}

/// Colours around and over the pages. The pages are white with dark text
/// whatever these are.
#[derive(Clone, Debug, PartialEq)]
pub struct DocStyle {
    pub background: Rgba8,
    /// The soft edge round each page.
    pub hairline: Rgba8,
    /// The caret: it is on white paper.
    pub caret: Rgba8,
    /// Selected text (translucent, drawn over the page).
    pub selection: Rgba8,
    /// Find matches, and the current one.
    pub hit: Rgba8,
    pub current_hit: Rgba8,
}

impl Default for DocStyle {
    fn default() -> Self {
        Self {
            background: [0xe9, 0xe9, 0xe9, 0xff],
            hairline: [0xd0, 0xd0, 0xd0, 0xff],
            caret: [0x1f, 0x1f, 0x1f, 0xff],
            selection: [0x7c, 0x5c, 0xe0, 0x59],
            hit: [0xff, 0xeb, 0x3b, 0x80],
            current_hit: [0xff, 0x8c, 0x00, 0xa6],
        }
    }
}

/// The caret and the other end of the selection.
#[derive(Clone, Debug, PartialEq)]
struct Sel {
    anchor: Pos,
    focus: Pos,
}

/// A page painted at a width.
struct Shot {
    width: u32,
    pixmap: Arc<Pixmap>,
    used: u64,
}

/// The part of a large page painted, in pixels of the whole page.
struct Band {
    page: usize,
    width: u32,
    x0: u32,
    y0: u32,
    pixmap: Arc<Pixmap>,
}

/// What the picture shows, to know whether it must be drawn again.
#[derive(Clone, Debug, PartialEq)]
struct Drawn {
    scroll: (f64, f64),
    size: (u16, u16),
    scale: f32,
    zoom: Zoom,
    version: u64,
    caret: bool,
}

pub struct DocView {
    document: Document,
    layouter: Layouter,
    layout: Layout,
    painter: DocPainter,
    /// Paragraphs of the body in reading order.
    paths: Vec<Path>,
    /// Page sizes in points, and the top of each in points (page `n` is the
    /// total), without gaps.
    sizes: Vec<(f32, f32)>,
    tops: Vec<f64>,
    zoom: Zoom,
    /// Physical pixels.
    scroll_x: f64,
    scroll_y: f64,
    width: u16,
    height: u16,
    scale: f32,
    style: DocStyle,
    sel: Option<Sel>,
    /// Where the caret was before Up and Down began: a page x.
    goal_x: Option<f32>,
    caret_on: bool,
    press: Option<editing::Press>,
    sel_rects: Option<Vec<(usize, PageRect)>>,
    find: Option<search::Finding>,
    hit_rects: HashMap<usize, Vec<(usize, PageRect)>>,
    shots: HashMap<usize, Shot>,
    band: Option<Band>,
    clock: u64,
    budget: usize,
    ctx: RenderContext,
    pixmap: Pixmap,
    resources: Resources,
    drawn: Option<Drawn>,
    /// Bumped whenever something that is painted over the pages changes.
    version: u64,
}

impl DocView {
    /// A view of `document`, laid out now. The caret is at the start.
    pub fn new(document: Document) -> Self {
        let mut shaper = Shaper::new();
        shaper.set_han_fallbacks(&HAN_FALLBACK_FAMILIES);
        let mut layouter = Layouter::with_shaper(shaper);
        let layout = layouter.layout(&document);
        let mut view = Self {
            document,
            layouter,
            layout,
            painter: DocPainter::new(),
            paths: Vec::new(),
            sizes: Vec::new(),
            tops: Vec::new(),
            zoom: Zoom::FitWidth,
            scroll_x: 0.0,
            scroll_y: 0.0,
            width: 0,
            height: 0,
            scale: 1.0,
            style: DocStyle::default(),
            sel: None,
            goal_x: None,
            caret_on: true,
            press: None,
            sel_rects: None,
            find: None,
            hit_rects: HashMap::new(),
            shots: HashMap::new(),
            band: None,
            clock: 0,
            budget: PIXEL_BUDGET,
            ctx: RenderContext::new(1, 1),
            pixmap: Pixmap::new(1, 1),
            resources: Resources::new(),
            drawn: None,
            version: 0,
        };
        view.lay_out();
        view.sel = view.document.start().map(|start| Sel {
            anchor: start.clone(),
            focus: start,
        });
        view
    }

    /// Replace the document, for the editor to come: laid out again (what
    /// did not change is not shaped again), the selection kept where it
    /// still is valid.
    pub fn set_document(&mut self, document: Document) {
        self.document = document;
        self.layout = self.layouter.layout(&self.document);
        self.lay_out();
        self.shots.clear();
        self.band = None;
        let keep = self.sel.take().and_then(|sel| {
            Some(Sel {
                anchor: text::sanitized(&self.document, sel.anchor)?,
                focus: text::sanitized(&self.document, sel.focus)?,
            })
        });
        self.sel = keep.or_else(|| {
            self.document.start().map(|start| Sel {
                anchor: start.clone(),
                focus: start,
            })
        });
        self.sel_rects = None;
        self.find = None;
        self.hit_rects.clear();
        self.press = None;
        self.clamp();
        self.touch();
    }

    fn lay_out(&mut self) {
        self.paths = text::paragraph_paths(&self.document);
        self.sizes = self
            .layout
            .pages
            .iter()
            .map(|page| (page.width.max(1.0), page.height.max(1.0)))
            .collect();
        self.tops = Vec::with_capacity(self.sizes.len() + 1);
        let mut top = 0.0;
        for (_, height) in &self.sizes {
            self.tops.push(top);
            top += f64::from(*height);
        }
        self.tops.push(top);
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn set_style(&mut self, style: DocStyle) {
        if style != self.style {
            self.style = style;
            self.touch();
        }
    }

    /// Something painted over the pages changed.
    fn touch(&mut self) {
        self.version += 1;
    }

    // --- Geometry ---

    pub fn resize(&mut self, width: u16, height: u16, scale: f32) {
        if (width, height) == (self.width, self.height) && scale == self.scale {
            return;
        }
        let anchor = self.anchor();
        self.width = width;
        self.height = height;
        self.scale = scale;
        self.ctx = RenderContext::new(width.max(1), height.max(1));
        self.pixmap = Pixmap::new(width.max(1), height.max(1));
        self.drawn = None;
        self.restore(anchor);
    }

    pub fn size(&self) -> (u16, u16, f32) {
        (self.width, self.height, self.scale)
    }

    fn s(&self) -> f64 {
        f64::from(self.scale)
    }

    /// Number of pages (at least one).
    pub fn page_count(&self) -> usize {
        self.sizes.len()
    }

    fn widest(&self) -> f64 {
        self.sizes
            .iter()
            .map(|(w, _)| f64::from(*w))
            .fold(1.0, f64::max)
    }

    fn tallest(&self) -> f64 {
        self.sizes
            .iter()
            .map(|(_, h)| f64::from(*h))
            .fold(1.0, f64::max)
    }

    /// Physical pixels per point.
    pub fn pixels_per_point(&self) -> f64 {
        let s = self.s();
        let margin = MARGIN * s;
        let fit_width = ((f64::from(self.width) - 2.0 * margin) / self.widest()).max(0.01);
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
        let n = self.page_count();
        2.0 * MARGIN * s
            + self.tops[n] * self.pixels_per_point()
            + GAP * s * n.saturating_sub(1) as f64
    }

    fn content_width(&self) -> f64 {
        2.0 * MARGIN * self.s() + self.widest() * self.pixels_per_point()
    }

    /// The pixels a page is painted in: its width, and the height that
    /// follows from painting at a scale that makes it that wide.
    fn page_px(&self, index: usize) -> (f64, f64) {
        let (w, h) = self.sizes[index];
        let ppp = self.pixels_per_point();
        let pw = (f64::from(w) * ppp).round().max(1.0);
        let ph = (f64::from(h) * paint_scale(pw, w)).ceil().max(1.0);
        (pw, ph)
    }

    /// Where page `index` is on screen, physical pixels.
    fn page_rect(&self, index: usize) -> Rect {
        let (w, h) = self.page_px(index);
        let column = f64::from(self.width);
        let content = self.content_width();
        let x = if content <= column {
            (column - w) / 2.0
        } else {
            (content - w) / 2.0 - self.scroll_x
        };
        let y = self.page_top(index) - self.scroll_y;
        let (x, y) = (x.round(), y.round());
        Rect::new(x, y, x + w, y + h)
    }

    /// The first page whose bottom is below `y` (canvas pixels).
    fn page_at(&self, y: f64) -> usize {
        let n = self.page_count();
        let ppp = self.pixels_per_point();
        let half_gap = GAP * self.s() / 2.0;
        let mut low = 0;
        let mut high = n - 1;
        while low < high {
            let mid = (low + high) / 2;
            let bottom = self.page_top(mid) + f64::from(self.sizes[mid].1) * ppp + half_gap;
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
        let first = self.page_at(self.scroll_y);
        let last = self.page_at(self.scroll_y + f64::from(self.height));
        first..last + 1
    }

    /// Page and how far down it (0 to 1) the top of the window is, and how
    /// far across the content the middle of the window is (when the content
    /// is wider than the window): to keep the place across zooms and
    /// resizes. `None` at the very top.
    fn anchor(&self) -> Option<(usize, f64, Option<f64>)> {
        if self.width == 0 {
            return None;
        }
        let across = (self.content_width() > f64::from(self.width))
            .then(|| (self.scroll_x + f64::from(self.width) / 2.0) / self.content_width());
        let page = self.page_at(self.scroll_y);
        let height = f64::from(self.sizes[page].1) * self.pixels_per_point();
        let into = (self.scroll_y - self.page_top(page)) / height.max(1.0);
        Some((
            page,
            if self.scroll_y <= 0.0 {
                f64::NEG_INFINITY
            } else {
                into
            },
            across,
        ))
    }

    fn restore(&mut self, anchor: Option<(usize, f64, Option<f64>)>) {
        let Some((page, into, across)) = anchor else {
            self.clamp();
            return;
        };
        if into == f64::NEG_INFINITY {
            self.scroll_y = 0.0;
        } else {
            let height = f64::from(self.sizes[page].1) * self.pixels_per_point();
            self.scroll_y = self.page_top(page) + into * height;
        }
        self.scroll_x = match across {
            Some(across) => across * self.content_width() - f64::from(self.width) / 2.0,
            None => 0.0,
        };
        self.clamp();
    }

    fn clamp(&mut self) {
        let max_y = (self.content_height() - f64::from(self.height)).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
        let max_x = (self.content_width() - f64::from(self.width)).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
    }

    /// Scroll the pages by physical pixels, vertically.
    pub fn scroll_by(&mut self, delta: f64) -> bool {
        let before = self.scroll_y;
        self.scroll_y += delta;
        self.clamp();
        self.scroll_y != before
    }

    /// The wheel or trackpad: both ways.
    pub fn wheel(&mut self, dx: f64, dy: f64) -> bool {
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

    /// Scroll by a window's height, less a little to keep context.
    pub fn page_down(&mut self, down: bool) -> bool {
        let step = self.page_step();
        self.scroll_by(if down { step } else { -step })
    }

    fn page_step(&self) -> f64 {
        (f64::from(self.height) - 2.0 * LINE_STEP * self.s()).max(LINE_STEP * self.s())
    }

    /// Show the top of page `index` at the top of the window.
    pub fn go_to_page(&mut self, index: usize) -> bool {
        let index = index.min(self.page_count() - 1);
        let before = self.scroll_y;
        self.scroll_y = self.page_top(index) - MARGIN * self.s() / 2.0;
        self.clamp();
        self.scroll_y != before
    }

    /// The page the reader is on: the caret's when it is in the window,
    /// otherwise the one a third of the way down it, or the last at the end.
    pub fn current_page(&self) -> usize {
        if let Some(rect) = self.caret_rect()
            && rect.1 >= 0.0
            && rect.1 + rect.3 <= f64::from(self.height)
            && let Some(page) = self
                .sel
                .as_ref()
                .and_then(|s| self.layout.page_of(&s.focus))
        {
            return page;
        }
        let (offset, max) = self.scroll_range();
        if max > 0.0 && offset >= max - 1.0 {
            return self.page_count() - 1;
        }
        self.page_at(self.scroll_y + f64::from(self.height) / 3.0)
    }

    /// The headings of the document, for the sidebar.
    pub fn outline(&self) -> &[OutlineEntry] {
        self.layout.outline()
    }

    // --- Painting ---

    pub fn set_caret_on(&mut self, on: bool) {
        self.caret_on = on;
    }

    pub fn needs_render(&self) -> bool {
        self.drawn.as_ref() != Some(&self.state())
    }

    fn state(&self) -> Drawn {
        Drawn {
            scroll: (self.scroll_x, self.scroll_y),
            size: (self.width, self.height),
            scale: self.scale,
            zoom: self.zoom,
            version: self.version,
            caret: self.caret_shown(),
        }
    }

    /// The caret is drawn: it blinks on, nothing is selected and it is in
    /// the window.
    fn caret_shown(&self) -> bool {
        self.caret_on
            && self.sel.as_ref().is_some_and(|s| s.anchor == s.focus)
            && self
                .caret_rect()
                .is_some_and(|(_, y, _, h)| y + h > 0.0 && y < f64::from(self.height))
    }

    /// Paint page `index` whole, or the part of it a band holds, and the
    /// pixels to draw it from.
    fn shot(&mut self, index: usize) -> Arc<Pixmap> {
        let (pw, _) = self.page_px(index);
        let width = pw as u32;
        self.clock += 1;
        if let Some(shot) = self.shots.get_mut(&index)
            && shot.width == width
        {
            shot.used = self.clock;
            return shot.pixmap.clone();
        }
        let page = &self.layout.pages[index];
        let scale = paint_scale(pw, page.width.max(1.0));
        let pixmap = Arc::new(self.painter.render(page, scale));
        self.shots.insert(
            index,
            Shot {
                width,
                pixmap: pixmap.clone(),
                used: self.clock,
            },
        );
        pixmap
    }

    /// Whether page `index` is too large to keep painted whole.
    fn is_large(&self, index: usize) -> bool {
        let (w, h) = self.page_px(index);
        w * h > MAX_SHOT_PIXELS || w > 16_000.0 || h > 16_000.0
    }

    /// Paint the part of a large page near the window and draw it.
    fn draw_band(&mut self, index: usize, rect: Rect) {
        let (pw, ph) = self.page_px(index);
        let (w, h) = (f64::from(self.width), f64::from(self.height));
        // What of the page is in the window, in pixels of the page.
        let vx0 = (-rect.x0).clamp(0.0, pw);
        let vx1 = (w - rect.x0).clamp(0.0, pw);
        let vy0 = (-rect.y0).clamp(0.0, ph);
        let vy1 = (h - rect.y0).clamp(0.0, ph);
        if vx1 <= vx0 || vy1 <= vy0 {
            return;
        }
        let reusable = self.band.as_ref().is_some_and(|band| {
            band.page == index
                && f64::from(band.width) == pw
                && f64::from(band.x0) <= vx0
                && f64::from(band.y0) <= vy0
                && f64::from(band.x0 + u32::from(band.pixmap.width())) >= vx1
                && f64::from(band.y0 + u32::from(band.pixmap.height())) >= vy1
        });
        if !reusable {
            let scale = paint_scale(pw, self.sizes[index].0);
            let bx0 = (vx0 - w / 4.0).max(0.0);
            let bx1 = (vx1 + w / 4.0).min(pw);
            let by0 = (vy0 - h / 2.0).max(0.0);
            let by1 = (vy1 + h / 2.0).min(ph);
            // The band starts at a whole point, so that text is placed as it
            // is in the page and does not shift when another band is made.
            let area = [
                (bx0 / scale).floor(),
                (by0 / scale).floor(),
                (bx1 / scale).ceil(),
                (by1 / scale).ceil(),
            ];
            let cropped = crop_page(&self.layout.pages[index], area);
            let pixmap = Arc::new(self.painter.render(&cropped, scale));
            self.band = Some(Band {
                page: index,
                width: pw as u32,
                x0: (area[0] * scale).round() as u32,
                y0: (area[1] * scale).round() as u32,
                pixmap,
            });
        }
        if let Some(band) = &self.band {
            let target = Rect::new(
                rect.x0 + f64::from(band.x0),
                rect.y0 + f64::from(band.y0),
                rect.x0 + f64::from(band.x0) + f64::from(band.pixmap.width()),
                rect.y0 + f64::from(band.y0) + f64::from(band.pixmap.height()),
            );
            let pixmap = band.pixmap.clone();
            draw_image(&mut self.ctx, &pixmap, target, None, Affine::IDENTITY);
        }
    }

    /// Drop painted pages that are far from the window, and the least
    /// recently used until the rest fit the budget.
    fn trim_shots(&mut self, visible: &std::ops::Range<usize>) {
        let keep = visible.start.saturating_sub(KEEP_AROUND)..visible.end + KEEP_AROUND;
        self.shots.retain(|page, _| keep.contains(page));
        let bytes = |shots: &HashMap<usize, Shot>| -> usize {
            shots
                .values()
                .map(|s| usize::from(s.pixmap.width()) * usize::from(s.pixmap.height()) * 4)
                .sum()
        };
        while bytes(&self.shots) > self.budget && self.shots.len() > 1 {
            let oldest = self
                .shots
                .iter()
                .min_by_key(|(_, shot)| shot.used)
                .map(|(page, _)| *page);
            match oldest {
                Some(page) => self.shots.remove(&page),
                None => break,
            };
        }
    }

    /// Bytes of painted pages held now.
    pub fn pixel_bytes(&self) -> usize {
        self.shots
            .values()
            .map(|s| usize::from(s.pixmap.width()) * usize::from(s.pixmap.height()) * 4)
            .sum::<usize>()
            + self.band.as_ref().map_or(0, |b| {
                usize::from(b.pixmap.width()) * usize::from(b.pixmap.height()) * 4
            })
    }

    pub fn set_pixel_budget(&mut self, bytes: usize) {
        self.budget = bytes;
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
        let visible = self.visible();
        let ppp = self.pixels_per_point();
        for index in visible.clone() {
            let rect = self.page_rect(index);
            // A thin soft edge, so white pages stand out from the canvas.
            self.ctx.set_paint(color(style.hairline));
            self.ctx.fill_rect(&rect.inflate(hairline, hairline));
            self.ctx.set_paint(Color::WHITE);
            self.ctx.fill_rect(&rect);
            if self.is_large(index) {
                self.draw_band(index, rect);
            } else {
                let pixmap = self.shot(index);
                draw_image(&mut self.ctx, &pixmap, rect, None, Affine::IDENTITY);
            }
            self.draw_marks(index, rect, ppp, &style);
        }
        self.trim_shots(&visible);
        self.ctx.flush();
        self.ctx.render(&mut self.pixmap, &mut self.resources);
        &self.pixmap
    }

    /// Search matches, the selection and the caret on one page.
    fn draw_marks(&mut self, index: usize, rect: Rect, ppp: f64, style: &DocStyle) {
        let on_page = |r: &PageRect| {
            Rect::new(
                rect.x0 + f64::from(r.x0) * ppp,
                rect.y0 + f64::from(r.y0) * ppp,
                rect.x0 + f64::from(r.x1) * ppp,
                rect.y0 + f64::from(r.y1) * ppp,
            )
        };
        let hits = self.hit_marks(index);
        for (current, r) in &hits {
            self.ctx.set_paint(color(if *current {
                style.current_hit
            } else {
                style.hit
            }));
            self.ctx.fill_rect(&on_page(r));
        }
        let selection: Vec<PageRect> = self
            .selection_rects()
            .iter()
            .filter(|(page, _)| *page == index)
            .map(|(_, r)| *r)
            .collect();
        self.ctx.set_paint(color(style.selection));
        for r in &selection {
            self.ctx.fill_rect(&on_page(r));
        }
        if self.caret_shown()
            && let Some(caret) = self
                .sel
                .as_ref()
                .and_then(|s| self.layout.caret(&s.focus))
                .filter(|c| c.page == index)
        {
            let width = (1.5 * self.s()).round().max(1.0);
            let x = (rect.x0 + f64::from(caret.x) * ppp - width / 2.0).round();
            let top = rect.y0 + f64::from(caret.top) * ppp;
            self.ctx.set_paint(color(style.caret));
            self.ctx.fill_rect(&Rect::new(
                x,
                top.round(),
                x + width,
                (top + f64::from(caret.height) * ppp).round(),
            ));
        }
    }

    /// The caret's rectangle: x, y, width, height, physical pixels from the
    /// window's top-left corner; `None` without a caret.
    pub fn caret_rect(&self) -> Option<(f64, f64, f64, f64)> {
        let sel = self.sel.as_ref()?;
        let caret = self.layout.caret(&sel.focus)?;
        let rect = self.page_rect(caret.page);
        let ppp = self.pixels_per_point();
        let width = (1.5 * self.s()).round().max(1.0);
        Some((
            (rect.x0 + f64::from(caret.x) * ppp - width / 2.0).round(),
            (rect.y0 + f64::from(caret.top) * ppp).round(),
            width,
            f64::from(caret.height) * ppp,
        ))
    }

    /// Where the candidate window goes: the caret's rectangle.
    pub fn ime_cursor_area(&mut self) -> (f64, f64, f64, f64) {
        self.caret_rect().unwrap_or((0.0, 0.0, 0.0, 0.0))
    }

    /// No input method composition yet: the view is read-only.
    pub fn is_composing(&self) -> bool {
        false
    }

    /// The whole text of the body, one paragraph per line.
    pub fn text(&self) -> String {
        self.document.plain_text()
    }

    // --- Canvas and page coordinates ---

    /// The page under a point of the window (the nearest one when it is in
    /// a gap or beside the pages) and the point in that page's points.
    fn locate(&self, x: f64, y: f64) -> (usize, f32, f32) {
        let index = self.page_at(y + self.scroll_y);
        let rect = self.page_rect(index);
        let ppp = self.pixels_per_point();
        (
            index,
            ((x - rect.x0) / ppp) as f32,
            ((y - rect.y0) / ppp) as f32,
        )
    }

    /// Whether a point of the window (physical pixels) is on a page.
    pub fn is_over_page(&self, x: f64, y: f64) -> bool {
        let (page, _, _) = self.locate(x, y);
        let rect = self.page_rect(page);
        x >= rect.x0 && x < rect.x1 && y >= rect.y0 && y < rect.y1
    }

    /// Scroll just enough to show a rectangle of the window (x, y, width,
    /// height, physical pixels), with some room around it.
    fn reveal(&mut self, rect: (f64, f64, f64, f64)) -> bool {
        let before = (self.scroll_x, self.scroll_y);
        let (x, y, w, h) = rect;
        let pad_y = (f64::from(self.height) * 0.15).min(self.page_step() / 2.0);
        if y < pad_y {
            self.scroll_y += y - pad_y;
        } else if y + h > f64::from(self.height) - pad_y {
            self.scroll_y += y + h - (f64::from(self.height) - pad_y);
        }
        let pad_x = f64::from(self.width) * 0.1;
        if x < pad_x {
            self.scroll_x += x - pad_x;
        } else if x + w > f64::from(self.width) - pad_x {
            self.scroll_x += x + w - (f64::from(self.width) - pad_x);
        }
        self.clamp();
        (self.scroll_x, self.scroll_y) != before
    }
}

/// The scale to paint a page `width` points wide so it is `pixels` wide:
/// a little under, so that the painted size rounds up to exactly that.
fn paint_scale(pixels: f64, width: f32) -> f64 {
    (pixels - 0.25).max(0.5) / f64::from(width)
}

/// A copy of `page` with only what is within `area` (points, left, top,
/// right and bottom), moved so that the area's corner is the page's. For
/// painting a part of a page at a scale at which the whole would be too
/// large.
fn crop_page(page: &Page, area: [f64; 4]) -> Page {
    let [x0, y0, x1, y1] = area.map(|v| v as f32);
    let (dx, dy) = (x0, y0);
    let pad = 4.0;
    let outside = |top: f32, bottom: f32| bottom < y0 - pad || top > y1 + pad;
    let outside_rect = |r: &PageRect| outside(r.y0, r.y1) || r.x1 < x0 - pad || r.x0 > x1 + pad;
    let moved = |r: &PageRect| PageRect::new(r.x0 - dx, r.y0 - dy, r.x1 - dx, r.y1 - dy);
    let mut items = Vec::new();
    for item in &page.items {
        match item {
            Item::Fill { rect, color } => {
                if !outside_rect(rect) {
                    items.push(Item::Fill {
                        rect: moved(rect),
                        color: *color,
                    });
                }
            }
            Item::Rule {
                from,
                to,
                width,
                color,
                style,
            } => {
                let reach = *width + pad;
                let (top, bottom) = (from.1.min(to.1) - reach, from.1.max(to.1) + reach);
                if !(outside(top, bottom)
                    || from.0.max(to.0) + reach < x0
                    || from.0.min(to.0) - reach > x1)
                {
                    items.push(Item::Rule {
                        from: (from.0 - dx, from.1 - dy),
                        to: (to.0 - dx, to.1 - dy),
                        width: *width,
                        color: *color,
                        style: *style,
                    });
                }
            }
            Item::Text(text) => {
                let lines = text.paragraph.lines();
                let (Some(first), Some(last)) = (
                    lines.get(text.lines.start),
                    lines.get(text.lines.end.saturating_sub(1)),
                ) else {
                    continue;
                };
                let top = text.y + first.top;
                let bottom = text.y + last.top + last.height;
                if !outside(top, bottom) {
                    let mut text = text.clone();
                    text.x -= dx;
                    text.y -= dy;
                    items.push(Item::Text(text));
                }
            }
            Item::Leader(leader) => {
                let reach = leader.size * 1.5 + pad;
                if !(outside(leader.baseline - reach, leader.baseline + reach)
                    || leader.x1 < x0
                    || leader.x0 > x1)
                {
                    let mut leader = *leader;
                    leader.x0 -= dx;
                    leader.x1 -= dx;
                    leader.baseline -= dy;
                    items.push(Item::Leader(leader));
                }
            }
            Item::Picture(picture) => {
                if !outside_rect(&picture.rect) {
                    let mut picture = picture.clone();
                    picture.rect = moved(&picture.rect);
                    items.push(Item::Picture(picture));
                }
            }
            Item::Frame(frame) => {
                if !outside_rect(&frame.rect) {
                    let mut frame = *frame;
                    frame.rect = moved(&frame.rect);
                    items.push(Item::Frame(frame));
                }
            }
            Item::Placeholder { rect, label } => {
                if !outside_rect(rect) {
                    items.push(Item::Placeholder {
                        rect: moved(rect),
                        label: label.clone(),
                    });
                }
            }
            // Clips stay, balanced: they are cheap.
            Item::PushClip(rect) => items.push(Item::PushClip(moved(rect))),
            Item::PopClip => items.push(Item::PopClip),
        }
    }
    Page {
        width: (x1 - x0).max(1.0),
        height: (y1 - y0).max(1.0),
        number: page.number,
        label: page.label.clone(),
        section: page.section,
        items,
    }
}
