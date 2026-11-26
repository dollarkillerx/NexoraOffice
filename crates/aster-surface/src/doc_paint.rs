//! Painting a laid-out Writer page.
//!
//! A page is white paper with dark text whatever the application's theme, as
//! slides and PDF pages are. [`DocPainter`] draws the display list of one
//! [`aster_layout::Page`] onto a `vello_cpu` context at a position and scale
//! (zoom times device scale); glyphs are placed in device pixels at the
//! scaled font size, so hinting works at the size that is shown.

use std::sync::Arc;

use aster_layout::{Item, LeaderItem, Page, Rect as PageRect, RuleStyle};
use aster_text::Leader;
use vello_cpu::kurbo::{Affine, Line, Rect, Shape as _, Stroke};
use vello_cpu::peniko::{Color, Fill};
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

use crate::glyphs::{self, Face};
use crate::picture::{ImageCache, draw_image};

const PAPER: [u8; 4] = [0xff, 0xff, 0xff, 0xff];
const PLACEHOLDER_FILL: [u8; 4] = [0xee, 0xee, 0xee, 0xff];
const PLACEHOLDER_LINE: [u8; 4] = [0x99, 0x99, 0x99, 0xff];

fn color(rgba: [u8; 4]) -> Color {
    Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3])
}

/// Draws pages. Keeps decoded pictures between pages.
#[derive(Default)]
pub struct DocPainter {
    resources: Resources,
    images: ImageCache,
    buffer: Vec<Glyph>,
}

impl DocPainter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Paint `page` with its top-left corner at `(x, y)` device pixels, one
    /// point being `scale` pixels. Draws the paper, then the items.
    pub fn paint_page(&mut self, ctx: &mut RenderContext, page: &Page, x: f64, y: f64, scale: f64) {
        let s = scale;
        ctx.set_transform(Affine::IDENTITY);
        ctx.set_fill_rule(Fill::NonZero);
        ctx.set_paint(color(PAPER));
        ctx.fill_rect(&Rect::new(
            x,
            y,
            x + f64::from(page.width) * s,
            y + f64::from(page.height) * s,
        ));
        let mut pen = Pen {
            ctx,
            resources: &mut self.resources,
            images: &mut self.images,
            buffer: &mut self.buffer,
            x,
            y,
            s,
            clips: 0,
        };
        pen.page_clip(page);
        for item in &page.items {
            pen.item(item);
        }
        while pen.clips > 0 {
            pen.ctx.pop_layer();
            pen.clips -= 1;
        }
    }

    /// Render a page alone onto a new pixmap, one point being `scale` pixels.
    pub fn render(&mut self, page: &Page, scale: f64) -> Pixmap {
        let width = (f64::from(page.width) * scale).ceil().clamp(1.0, 16_000.0) as u16;
        let height = (f64::from(page.height) * scale).ceil().clamp(1.0, 16_000.0) as u16;
        let mut ctx = RenderContext::new(width, height);
        self.paint_page(&mut ctx, page, 0.0, 0.0, scale);
        ctx.flush();
        let mut pixmap = Pixmap::new(width, height);
        ctx.render(&mut pixmap, &mut self.resources);
        pixmap
    }

    /// The page as a PNG.
    pub fn encode_png(&mut self, page: &Page, scale: f64) -> Result<Vec<u8>, String> {
        self.render(page, scale)
            .into_png()
            .map_err(|e| e.to_string())
    }
}

struct Pen<'a> {
    ctx: &'a mut RenderContext,
    resources: &'a mut Resources,
    images: &'a mut ImageCache,
    buffer: &'a mut Vec<Glyph>,
    x: f64,
    y: f64,
    s: f64,
    clips: usize,
}

impl Pen<'_> {
    fn rect(&self, r: &PageRect) -> Rect {
        Rect::new(
            self.x + f64::from(r.x0) * self.s,
            self.y + f64::from(r.y0) * self.s,
            self.x + f64::from(r.x1) * self.s,
            self.y + f64::from(r.y1) * self.s,
        )
    }

    fn page_clip(&mut self, page: &Page) {
        let r = self.rect(&PageRect::new(0.0, 0.0, page.width, page.height));
        self.ctx.push_clip_layer(&r.to_path(0.1));
        self.clips += 1;
    }

    fn fill(&mut self, rect: Rect, rgba: [u8; 4]) {
        self.ctx.set_paint(color(rgba));
        self.ctx.fill_rect(&rect);
    }

    fn item(&mut self, item: &Item) {
        match item {
            Item::Fill { rect, color } => {
                let r = self.rect(rect);
                self.fill(r, *color);
            }
            Item::Rule {
                from,
                to,
                width,
                color,
                style,
            } => self.rule(*from, *to, *width, *color, *style),
            Item::Text(text) => self.text(text),
            Item::Leader(leader) => self.leader(leader),
            Item::Picture(picture) => {
                let rect = self.rect(&picture.rect);
                match self.images.get(&picture.image) {
                    Some(pixmap) => {
                        self.ctx.push_clip_layer(&rect.to_path(0.1));
                        draw_image(
                            self.ctx,
                            &pixmap,
                            rect,
                            Some(picture.crop),
                            Affine::IDENTITY,
                        );
                        self.ctx.pop_layer();
                    }
                    None => self.placeholder(rect),
                }
            }
            Item::Frame(frame) => {
                let rect = self.rect(&frame.rect);
                if let Some(fill) = frame.fill {
                    self.fill(rect, fill);
                }
                if let Some((width, rgba)) = frame.line {
                    self.ctx.set_paint(color(rgba));
                    self.ctx
                        .set_stroke(Stroke::new((f64::from(width) * self.s).max(0.5)));
                    self.ctx.stroke_rect(&rect);
                }
            }
            Item::Placeholder { rect, .. } => {
                let rect = self.rect(rect);
                self.placeholder(rect);
            }
            Item::PushClip(rect) => {
                let rect = self.rect(rect);
                self.ctx.push_clip_layer(&rect.to_path(0.1));
                self.clips += 1;
            }
            Item::PopClip => {
                if self.clips > 1 {
                    self.ctx.pop_layer();
                    self.clips -= 1;
                }
            }
        }
    }

    fn placeholder(&mut self, rect: Rect) {
        self.fill(rect, PLACEHOLDER_FILL);
        self.ctx.set_paint(color(PLACEHOLDER_LINE));
        self.ctx.set_stroke(Stroke::new((self.s * 0.75).max(1.0)));
        self.ctx.stroke_rect(&rect);
        for (a, b) in [
            ((rect.x0, rect.y0), (rect.x1, rect.y1)),
            ((rect.x0, rect.y1), (rect.x1, rect.y0)),
        ] {
            self.ctx.stroke_path(&Line::new(a, b).to_path(0.1));
        }
    }

    fn rule(
        &mut self,
        from: (f32, f32),
        to: (f32, f32),
        width: f32,
        rgba: [u8; 4],
        style: RuleStyle,
    ) {
        let w = (f64::from(width) * self.s).max(1.0);
        let (x0, y0) = (
            self.x + f64::from(from.0) * self.s,
            self.y + f64::from(from.1) * self.s,
        );
        let (x1, y1) = (
            self.x + f64::from(to.0) * self.s,
            self.y + f64::from(to.1) * self.s,
        );
        let horizontal = (y1 - y0).abs() < (x1 - x0).abs();
        // One thin bar at `at` across the line.
        let bar = |pen: &mut Self, offset: f64, thickness: f64, a: f64, b: f64| {
            let r = if horizontal {
                Rect::new(
                    a,
                    y0 + offset - thickness / 2.0,
                    b,
                    y0 + offset + thickness / 2.0,
                )
            } else {
                Rect::new(
                    x0 + offset - thickness / 2.0,
                    a,
                    x0 + offset + thickness / 2.0,
                    b,
                )
            };
            pen.fill(r, rgba);
        };
        let (a, b) = if horizontal {
            (x0.min(x1), x0.max(x1))
        } else {
            (y0.min(y1), y0.max(y1))
        };
        match style {
            RuleStyle::Single => bar(self, 0.0, w, a, b),
            RuleStyle::Double => {
                let t = (w / 3.0).max(1.0);
                bar(self, -t, t, a, b);
                bar(self, t, t, a, b);
            }
            RuleStyle::Dotted | RuleStyle::Dashed => {
                let (on, off) = if style == RuleStyle::Dotted {
                    (w, w * 1.5)
                } else {
                    (w * 4.0, w * 2.0)
                };
                let mut at = a;
                while at < b {
                    bar(self, 0.0, w, at, (at + on).min(b));
                    at += on + off;
                }
            }
        }
    }

    fn leader(&mut self, leader: &LeaderItem) {
        let s = self.s;
        let size = f64::from(leader.size) * s;
        let (x0, x1) = (
            self.x + f64::from(leader.x0) * s,
            self.x + f64::from(leader.x1) * s,
        );
        let baseline = self.y + f64::from(leader.baseline) * s;
        let thickness = (size * 0.07).max(1.0);
        let (pitch, mark_w, y) = match leader.kind {
            Leader::Dots => (size * 0.33, thickness, baseline - thickness),
            Leader::MiddleDots => (size * 0.33, thickness, baseline - size * 0.28),
            Leader::Hyphens => (size * 0.5, size * 0.3, baseline - size * 0.28),
            Leader::Underscore => (x1 - x0, x1 - x0, baseline + thickness),
            Leader::None => return,
        };
        if pitch <= 0.0 {
            return;
        }
        // Marks sit on a grid anchored to the right end, as Word's do, so
        // that leaders of neighbouring lines line up.
        let mut at = x1 - mark_w - pitch * 0.5;
        let rgba = leader.color;
        while at >= x0 + pitch * 0.25 {
            self.fill(Rect::new(at, y, at + mark_w, y + thickness), rgba);
            at -= pitch;
        }
        if leader.kind == Leader::Underscore {
            self.fill(Rect::new(x0, y, x1, y + thickness), rgba);
        }
    }

    fn text(&mut self, item: &aster_layout::TextItem) {
        let s = self.s as f32;
        let (ox, oy) = (self.x as f32 + item.x * s, self.y as f32 + item.y * s);
        let paragraph: &Arc<aster_text::Paragraph> = &item.paragraph;
        for run in paragraph.draw_runs_in(item.lines.clone()) {
            self.buffer.clear();
            self.buffer.extend(run.glyphs.iter().map(|g| Glyph {
                id: g.id,
                x: ox + g.x * s,
                y: oy + g.y * s,
            }));
            let face = Face {
                font: &run.face.font,
                size: run.face.size * s,
                coords: &run.face.coords,
                embolden: run.face.embolden,
                skew: run.skew,
            };
            glyphs::draw_face(
                self.ctx,
                self.resources,
                &face,
                color(run.color),
                self.buffer,
            );
        }
        for line in paragraph.decorations_in(item.lines.clone(), 0.5) {
            self.ctx.set_paint(color(line.color));
            self.ctx.fill_rect(&Rect::new(
                f64::from(ox + line.x0 * s),
                f64::from(oy + line.y * s),
                f64::from(ox + line.x1 * s),
                f64::from(oy + (line.y + line.thickness) * s),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aster_layout::Layouter;
    use aster_writer::{
        Block, Document, Drawing, DrawingContent, Inline, Paragraph, Picture, Run, RunLook,
        Section, Story,
    };

    use super::DocPainter;

    fn document() -> Document {
        let look = RunLook {
            size: 18.0,
            ..RunLook::default()
        };
        // A picture the decoders cannot read draws as a placeholder.
        let picture = Run::object(
            Inline::Drawing(Arc::new(Drawing {
                size: (40 * 12_700, 30 * 12_700),
                content: DrawingContent::Picture(Picture {
                    image: Arc::from(vec![1u8, 2, 3]),
                    crop: [0.0; 4],
                }),
                float: None,
                alt: None,
            })),
            look.clone(),
        );
        let paragraph = Paragraph {
            runs: vec![Run::text("Paper and ink", look.clone()), picture],
            base: look.clone(),
            mark: look,
            ..Paragraph::default()
        };
        Document {
            body: Story {
                blocks: vec![Block::paragraph(paragraph)],
                part: None,
            },
            last_section: Arc::new(Section::default()),
            ..Document::default()
        }
    }

    #[test]
    fn a_page_is_white_paper_with_dark_text() {
        let layout = Layouter::new().layout(&document());
        let mut painter = DocPainter::new();
        let pixmap = painter.render(&layout.pages[0], 1.0);
        let data = pixmap.data();
        let (w, h) = (usize::from(pixmap.width()), usize::from(pixmap.height()));
        assert_eq!((w, h), (596, 842));
        let pixel = |x: usize, y: usize| data[y * w + x];
        let corner = pixel(2, 2);
        assert_eq!(
            (corner.r, corner.g, corner.b, corner.a),
            (255, 255, 255, 255)
        );
        // Some ink where the first line is, and the placeholder's grey box.
        let dark = (60..110)
            .flat_map(|y| (60..300).map(move |x| (x, y)))
            .filter(|(x, y)| pixel(*x, *y).r < 100)
            .count();
        assert!(dark > 50, "{dark}");
        let png = painter.encode_png(&layout.pages[0], 0.5).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }

    fn ink(bold: bool, painter: &mut DocPainter) -> usize {
        let look = RunLook {
            size: 30.0,
            bold,
            font: Some("Calibri".into()),
            east_asian_font: Some("宋体".into()),
            ..RunLook::default()
        };
        let paragraph = Paragraph {
            runs: vec![Run::text("第一章谢谢", look.clone())],
            base: look.clone(),
            mark: look,
            ..Paragraph::default()
        };
        let document = Document {
            body: Story {
                blocks: vec![Block::paragraph(paragraph)],
                part: None,
            },
            last_section: Arc::new(Section::default()),
            ..Document::default()
        };
        let layout = Layouter::new().layout(&document);
        let pixmap = painter.render(&layout.pages[0], 2.0);
        pixmap.data().iter().filter(|p| p.r < 128).count()
    }

    #[test]
    fn bold_east_asian_text_has_more_ink_even_after_regular_text() {
        let mut painter = DocPainter::new();
        let regular = ink(false, &mut painter);
        let bold = ink(true, &mut painter);
        assert!(bold * 100 > regular * 118, "{regular} {bold}");
    }
}
