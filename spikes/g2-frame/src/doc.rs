//! A long plain document laid out one paragraph at a time.
//!
//! This is the shape the architecture plans for every editor: each paragraph owns
//! its own cached layout, an edit re-lays-out only the paragraph it touched, and
//! painting visits only the paragraphs that intersect the requested band.

use parley::fontique::Script;
use parley::layout::PositionedLayoutItem;
use parley::{FontContext, GenericFamily, Layout, LayoutContext, LineHeight, StyleProperty};
use vello_cpu::kurbo::{Affine, Rect};
use vello_cpu::peniko::{Color, Fill};
use vello_cpu::{Glyph, RenderContext, Resources};

const BODY_SIZE: f32 = 18.0;
const HEADING_SIZE: f32 = 26.0;
/// Space between the surface edge and the text column, in logical pixels.
const MARGIN: f64 = 48.0;
/// Space after each paragraph, as a multiple of its font size.
const PARAGRAPH_GAP: f64 = 0.7;

const PAGE: Color = Color::from_rgb8(0xff, 0xff, 0xff);
const INK: Color = Color::from_rgb8(0x0b, 0x0b, 0x0b);

/// See `spikes/g1-ime/src/surface.rs` for why this chain is set explicitly.
const HAN_FALLBACK_FAMILIES: [&str; 4] =
    ["Hiragino Sans GB", "Heiti SC", "Songti SC", "Hiragino Sans"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Brush(pub Color);

impl Default for Brush {
    fn default() -> Self {
        Self(INK)
    }
}

struct Paragraph {
    text: String,
    font_size: f32,
    layout: Layout<Brush>,
    /// Top edge in physical pixels from the start of the document.
    y: f64,
    /// Height in physical pixels, including the gap that follows.
    height: f64,
}

pub struct Document {
    font_cx: FontContext,
    layout_cx: LayoutContext<Brush>,
    paragraphs: Vec<Paragraph>,
    scale: f32,
    text_width: f32,
    total_height: f64,
}

const SENTENCES: [&str; 12] = [
    "离线优先意味着所有文件都保存在本地，没有网络也能完成全部编辑工作。",
    "表格必须采用虚拟化渲染，只绘制当前可见的单元格，滚动时保持流畅。",
    "文字处理采用分页布局模型，长文档在后台增量分页，不阻塞输入。",
    "排版引擎对中文、日文和英文使用同一套断行与字体回退规则。",
    "日本語の文章でも同じレイアウトエンジンを使い、禁則処理を正しく行います。",
    "スプレッドシートは見えているセルだけを描画するので、大きなファイルでも軽快です。",
    "ファイルはすべて手元に保存され、ネットワークがなくても編集を続けられます。",
    "The document surface is drawn by our own renderer and handed to the shell as a frame.",
    "Every paragraph keeps its own cached layout, so an edit only touches the text around it.",
    "Performance is measured with benchmarks on reference hardware, never judged by feel.",
    "在 2560×1440 的窗口里连续滚动时，帧耗时的 95 分位不应超过 16 ms。",
    "Mixed text such as Rust 1.95、Slint 1.19 和 Parley 0.13 must wrap without breaking words.",
];

const HEADINGS: [&str; 4] = [
    "一、项目目标与核心原则",
    "二、文書レイアウトの方針",
    "3. Rendering pipeline",
    "四、性能预算与基准测试",
];

/// A small deterministic generator, so every run lays out the same document.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % bound
    }
}

fn build_layout(
    font_cx: &mut FontContext,
    layout_cx: &mut LayoutContext<Brush>,
    text: &str,
    font_size: f32,
    scale: f32,
    width: f32,
) -> Layout<Brush> {
    let mut builder = layout_cx.ranged_builder(font_cx, text, scale, true);
    builder.push_default(GenericFamily::SystemUi);
    builder.push_default(StyleProperty::FontSize(font_size));
    builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(1.6)));
    builder.push_default(StyleProperty::Brush(Brush(INK)));
    builder.push_default(StyleProperty::Locale("zh-Hans".parse().ok()));
    let mut layout = builder.build(text);
    layout.break_all_lines(Some(width));
    layout
}

impl Document {
    /// Build a document of `count` paragraphs. Nothing is laid out yet.
    pub fn generate(count: usize) -> Self {
        let mut rng = Lcg(0x5eed);
        let paragraphs = (0..count)
            .map(|index| {
                let (text, font_size) = if index % 12 == 0 {
                    (
                        HEADINGS[(index / 12) % HEADINGS.len()].to_owned(),
                        HEADING_SIZE,
                    )
                } else {
                    let sentences = 3 + rng.next(6);
                    let text = (0..sentences)
                        .map(|_| SENTENCES[rng.next(SENTENCES.len())])
                        .collect::<String>();
                    (text, BODY_SIZE)
                };
                Paragraph {
                    text,
                    font_size,
                    layout: Layout::default(),
                    y: 0.0,
                    height: 0.0,
                }
            })
            .collect();

        let mut font_cx = FontContext::default();
        let han = Script::from_bytes(*b"Hani");
        let families: Vec<_> = HAN_FALLBACK_FAMILIES
            .iter()
            .filter_map(|name| font_cx.collection.family_id(name))
            .collect();
        if !families.is_empty() {
            font_cx.collection.set_fallbacks(han, families.into_iter());
        }

        Self {
            font_cx,
            layout_cx: LayoutContext::default(),
            paragraphs,
            scale: 1.0,
            text_width: 1.0,
            total_height: 0.0,
        }
    }

    fn margin(&self) -> f64 {
        MARGIN * f64::from(self.scale)
    }

    /// Lay out every paragraph for a surface `width` physical pixels wide.
    pub fn layout_all(&mut self, width: u16, scale: f32) {
        self.scale = scale;
        self.text_width = (f64::from(width) - 2.0 * self.margin()).max(1.0) as f32;
        for index in 0..self.paragraphs.len() {
            self.layout_paragraph(index);
        }
        self.restack(0);
    }

    fn layout_paragraph(&mut self, index: usize) {
        let paragraph = &mut self.paragraphs[index];
        paragraph.layout = build_layout(
            &mut self.font_cx,
            &mut self.layout_cx,
            &paragraph.text,
            paragraph.font_size,
            self.scale,
            self.text_width,
        );
        let gap = PARAGRAPH_GAP * f64::from(paragraph.font_size) * f64::from(self.scale);
        paragraph.height = (f64::from(paragraph.layout.height()) + gap).ceil();
    }

    /// Recompute the vertical positions of paragraphs from `first` onwards.
    fn restack(&mut self, first: usize) {
        let mut y = match first {
            0 => self.margin(),
            _ => self.paragraphs[first - 1].y + self.paragraphs[first - 1].height,
        };
        for paragraph in &mut self.paragraphs[first..] {
            paragraph.y = y;
            y += paragraph.height;
        }
        self.total_height = y + self.margin();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.paragraphs.len()
    }

    pub fn total_height(&self) -> f64 {
        self.total_height
    }

    /// Index of the first paragraph whose bottom edge is below `y`.
    pub fn paragraph_at(&self, y: f64) -> usize {
        self.paragraphs
            .partition_point(|p| p.y + p.height <= y)
            .min(self.paragraphs.len().saturating_sub(1))
    }

    /// The vertical band `(top, bottom)` a paragraph occupies, in physical pixels.
    pub fn paragraph_band(&self, index: usize) -> (f64, f64) {
        let paragraph = &self.paragraphs[index];
        (paragraph.y, paragraph.y + paragraph.height)
    }

    /// Append text to one paragraph and re-lay-out only that paragraph. Returns
    /// `true` when its height changed, which moves everything below it.
    pub fn append(&mut self, index: usize, text: &str) -> bool {
        let before = self.paragraphs[index].height;
        self.paragraphs[index].text.push_str(text);
        self.layout_paragraph(index);
        let moved = self.paragraphs[index].height != before;
        if moved {
            self.restack(index);
        }
        moved
    }

    /// Paint the band that starts `top` physical pixels into the document and is
    /// `height` pixels tall, with the band's top-left at the context's origin.
    pub fn paint(&self, ctx: &mut RenderContext, resources: &mut Resources, top: f64, height: f64) {
        ctx.set_fill_rule(Fill::NonZero);
        ctx.set_transform(Affine::IDENTITY);
        ctx.set_paint(PAGE);
        ctx.fill_rect(&Rect::new(0.0, 0.0, f64::from(ctx.width()), height));

        let margin = self.margin();
        let first = self.paragraphs.partition_point(|p| p.y + p.height <= top);
        for paragraph in &self.paragraphs[first..] {
            if paragraph.y >= top + height {
                break;
            }
            ctx.set_transform(Affine::translate((margin, paragraph.y - top)));
            paint_layout(ctx, resources, &paragraph.layout);
        }
    }
}

fn paint_layout(ctx: &mut RenderContext, resources: &mut Resources, layout: &Layout<Brush>) {
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };
            let run = glyph_run.run();
            ctx.set_paint(glyph_run.style().brush.0);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Document {
        let mut doc = Document::generate(60);
        doc.layout_all(900, 1.5);
        doc
    }

    #[test]
    fn generation_is_deterministic() {
        let (a, b) = (document(), document());
        assert_eq!(a.total_height(), b.total_height());
        assert!(a.total_height() > 2000.0);
    }

    #[test]
    fn paragraphs_are_stacked_without_gaps_or_overlap() {
        let doc = document();
        for index in 1..doc.len() {
            let (_, previous_bottom) = doc.paragraph_band(index - 1);
            let (top, bottom) = doc.paragraph_band(index);
            assert_eq!(previous_bottom, top);
            assert!(bottom > top);
        }
    }

    #[test]
    fn paragraph_lookup_matches_bands() {
        let doc = document();
        for index in [0, 7, 31, doc.len() - 1] {
            let (top, bottom) = doc.paragraph_band(index);
            assert_eq!(doc.paragraph_at(top), index);
            assert_eq!(doc.paragraph_at(bottom - 1.0), index);
        }
    }

    #[test]
    fn appending_enough_text_moves_what_follows() {
        let mut doc = document();
        let (_, before) = doc.paragraph_band(5);
        let (next_before, _) = doc.paragraph_band(6);
        let mut moved = false;
        for _ in 0..40 {
            moved |= doc.append(5, "追加一段足够长的文字让这一段多出一行。");
        }
        assert!(moved);
        let (_, after) = doc.paragraph_band(5);
        let (next_after, _) = doc.paragraph_band(6);
        assert!(after > before);
        assert_eq!(next_after - next_before, after - before);
    }
}
