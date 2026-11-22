//! Real fonts: Parley shapes the paragraph and picks fallback fonts, and this
//! module turns the result into the clusters the breaker works on, then draws
//! the lines the breaker produced.
//!
//! Parley is asked for a single unbroken line. Its own line breaker is not used,
//! except by `parley_lines`, which exists only to compare against.

use std::ops::Range;

use parley::fontique::Script;
use parley::layout::PositionedLayoutItem;
use parley::{
    FontContext, FontData, GenericFamily, Layout, LayoutContext, LineHeight, StyleProperty,
};
use vello_cpu::kurbo::{Affine, Rect};
use vello_cpu::peniko::Color;
use vello_cpu::{Glyph, RenderContext, Resources};

use crate::breaker::{Cluster, EndTreatment, Fx, Line};
use crate::class::classify;
use crate::metrics::break_opportunities;

const INK: Color = Color::from_rgb8(0x0b, 0x0b, 0x0b);
const HUNG: Color = Color::from_rgb8(0x7c, 0x5c, 0xe0);

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

/// The glyphs of one cluster, relative to the cluster's left edge.
struct ClusterGlyphs {
    font: FontData,
    font_size: f32,
    coords: Vec<i16>,
    glyphs: Vec<parley::layout::Glyph>,
}

pub struct Shaped {
    pub clusters: Vec<Cluster>,
    glyphs: Vec<Option<ClusterGlyphs>>,
}

/// A run of text with the settings needed to lay it out.
#[derive(Clone, Copy)]
pub struct Paragraph<'a> {
    pub text: &'a str,
    pub language: &'a str,
    pub font_size: f32,
}

pub struct Shaper {
    font_cx: FontContext,
    layout_cx: LayoutContext<Brush>,
}

fn to_fx(value: f32) -> Fx {
    (value * 64.0).round() as Fx
}

impl Shaper {
    pub fn new() -> Self {
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
        }
    }

    fn layout(
        &mut self,
        text: &str,
        language: &str,
        font_size: f32,
        width: Option<f32>,
    ) -> Layout<Brush> {
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, false);
        builder.push_default(GenericFamily::SystemUi);
        builder.push_default(StyleProperty::FontSize(font_size));
        builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(1.7)));
        builder.push_default(StyleProperty::Brush(Brush(INK)));
        builder.push_default(StyleProperty::Locale(language.parse().ok()));
        let mut layout = builder.build(text);
        layout.break_all_lines(width);
        layout
    }

    /// Shape a paragraph and describe it as breaker clusters, in text order.
    pub fn shape(&mut self, text: &str, language: &str, font_size: f32) -> Shaped {
        let layout = self.layout(text, language, font_size, None);
        let breaks = break_opportunities(text, language);
        let em = to_fx(font_size);

        let mut found: Vec<(Cluster, Option<ClusterGlyphs>)> = Vec::new();
        for line in layout.lines() {
            for run in line.runs() {
                let coords = bytemuck::cast_slice::<_, i16>(run.normalized_coords()).to_vec();
                for cluster in run.clusters() {
                    let range = cluster.text_range();
                    if range.is_empty() {
                        continue;
                    }
                    let first = text[range.clone()].chars().next().unwrap_or(' ');
                    let newline = cluster.is_hard_line_break();
                    found.push((
                        Cluster {
                            class: classify(first),
                            advance: if newline { 0 } else { to_fx(cluster.advance()) },
                            em: to_fx(run.font_size()),
                            break_before: range.start > 0
                                && breaks.binary_search(&range.start).is_ok(),
                            newline,
                            text: range,
                        },
                        Some(ClusterGlyphs {
                            font: run.font().clone(),
                            font_size: run.font_size(),
                            coords: coords.clone(),
                            glyphs: cluster.glyphs().collect(),
                        }),
                    ));
                }
            }
        }
        found.sort_by_key(|(cluster, _)| cluster.text.start);

        // Parley may not report a cluster for every character (a newline, for
        // one). The breaker needs the text covered without holes.
        let mut clusters = Vec::with_capacity(found.len());
        let mut glyphs = Vec::with_capacity(found.len());
        let mut offset = 0;
        let fill = |from: usize,
                    to: usize,
                    clusters: &mut Vec<Cluster>,
                    glyphs: &mut Vec<Option<ClusterGlyphs>>| {
            for (at, c) in text[from..to].char_indices() {
                let start = from + at;
                clusters.push(Cluster {
                    text: start..start + c.len_utf8(),
                    class: classify(c),
                    advance: 0,
                    em,
                    break_before: false,
                    newline: c == '\n',
                });
                glyphs.push(None);
            }
        };
        for (cluster, cluster_glyphs) in found {
            if cluster.text.start < offset {
                continue;
            }
            fill(offset, cluster.text.start, &mut clusters, &mut glyphs);
            offset = cluster.text.end;
            clusters.push(cluster);
            glyphs.push(cluster_glyphs);
        }
        fill(offset, text.len(), &mut clusters, &mut glyphs);
        Shaped { clusters, glyphs }
    }

    /// The byte ranges of the lines Parley's own breaker produces at `width`.
    pub fn parley_lines(
        &mut self,
        text: &str,
        language: &str,
        font_size: f32,
        width: f32,
    ) -> Vec<Range<usize>> {
        let layout = self.layout(text, language, font_size, Some(width));
        layout.lines().map(|line| line.text_range()).collect()
    }

    /// Draw a paragraph with Parley's own line breaking, top-left at `origin`.
    /// Returns the height used.
    pub fn paint_parley(
        &mut self,
        ctx: &mut RenderContext,
        resources: &mut Resources,
        paragraph: &Paragraph<'_>,
        width: f32,
        origin: (f64, f64),
    ) -> f64 {
        let Paragraph {
            text,
            language,
            font_size,
        } = *paragraph;
        let layout = self.layout(text, language, font_size, Some(width));
        ctx.set_transform(Affine::translate(origin));
        ctx.set_paint(INK);
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                    continue;
                };
                let run = glyph_run.run();
                let coords = bytemuck::cast_slice::<_, i16>(run.normalized_coords());
                ctx.glyph_run(resources, run.font())
                    .font_size(run.font_size())
                    .normalized_coords(coords)
                    .fill_glyphs(glyph_run.positioned_glyphs().map(|g| Glyph {
                        id: g.id,
                        x: g.x,
                        y: g.y,
                    }));
            }
        }
        f64::from(layout.height())
    }
}

impl Shaped {
    /// Draw the lines the breaker produced, top-left at `origin`. A mark that
    /// hangs outside the line is drawn in the accent colour so it can be seen.
    /// Returns the height used.
    pub fn paint(
        &self,
        ctx: &mut RenderContext,
        resources: &mut Resources,
        lines: &[Line],
        font_size: f32,
        origin: (f64, f64),
    ) -> f64 {
        let line_height = f64::from(font_size) * 1.7;
        let baseline = f64::from(font_size) * 1.25;
        ctx.set_transform(Affine::translate(origin));
        for (n, line) in lines.iter().enumerate() {
            let y = (n as f64 * line_height + baseline) as f32;
            let hung = match line.end {
                EndTreatment::Hung => line
                    .placed
                    .iter()
                    .rev()
                    .find(|p| !self.clusters[p.index].newline && self.clusters[p.index].advance > 0)
                    .map(|p| p.index),
                _ => None,
            };
            for placed in &line.placed {
                let Some(cluster) = &self.glyphs[placed.index] else {
                    continue;
                };
                ctx.set_paint(if hung == Some(placed.index) {
                    HUNG
                } else {
                    INK
                });
                let mut pen = placed.glyph_x as f32 / 64.0;
                let glyphs: Vec<Glyph> = cluster
                    .glyphs
                    .iter()
                    .map(|g| {
                        let glyph = Glyph {
                            id: g.id,
                            x: pen + g.x,
                            y: y + g.y,
                        };
                        pen += g.advance;
                        glyph
                    })
                    .collect();
                ctx.glyph_run(resources, &cluster.font)
                    .font_size(cluster.font_size)
                    .normalized_coords(&cluster.coords)
                    .fill_glyphs(glyphs.into_iter());
            }
        }
        lines.len() as f64 * line_height
    }
}

/// A thin vertical rule, used to show where the right margin is.
pub fn paint_rule(ctx: &mut RenderContext, x: f64, top: f64, bottom: f64) {
    ctx.set_transform(Affine::IDENTITY);
    ctx.set_paint(Color::from_rgb8(0xd9, 0xd9, 0xd9));
    ctx.fill_rect(&Rect::new(x, top, x + 1.0, bottom));
}
