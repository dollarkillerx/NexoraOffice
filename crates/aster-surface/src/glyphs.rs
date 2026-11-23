//! Drawing glyphs that Parley has laid out, with the effects both surfaces
//! need: a colour per stretch, and bold and italic made up from the regular
//! face where the font has no real one.

use parley::Run;
use vello_cpu::kurbo::{Affine, Rect, Stroke};
use vello_cpu::peniko::Color;
use vello_cpu::{Glyph, RenderContext, Resources};

use crate::text_surface::Brush;

/// Slant of made-up italics, in degrees.
pub const SYNTHETIC_SKEW_DEGREES: f32 = 12.0;
/// Outline width of made-up bold, as a fraction of the font size.
const SYNTHETIC_BOLD: f32 = 0.035;

/// How to draw one stretch of glyphs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphPaint {
    pub color: Color,
    /// Thicken the outlines.
    pub embolden: bool,
    /// Slant, in degrees.
    pub skew: Option<f32>,
}

/// Draw glyphs of `run`, already positioned in the current transform.
pub fn draw(
    ctx: &mut RenderContext,
    resources: &mut Resources,
    run: &Run<'_, Brush>,
    glyphs: &[Glyph],
    paint: GlyphPaint,
) {
    if glyphs.is_empty() {
        return;
    }
    let synthesis = run.synthesis();
    let embolden = paint.embolden || synthesis.embolden();
    let skew = paint.skew.or(synthesis.skew());
    let font_size = run.font_size();
    let coords = bytemuck::cast_slice::<_, i16>(run.normalized_coords());
    let transform = skew.map(|degrees| Affine::skew(f64::from(degrees.to_radians().tan()), 0.0));

    ctx.set_paint(paint.color);
    let mut builder = ctx
        .glyph_run(resources, run.font())
        .font_size(font_size)
        .hint(transform.is_none())
        .normalized_coords(coords);
    if let Some(transform) = transform {
        builder = builder.glyph_transform(transform);
    }
    builder.fill_glyphs(glyphs.iter().copied());

    if embolden {
        ctx.set_stroke(Stroke::new(f64::from(font_size * SYNTHETIC_BOLD)));
        let mut builder = ctx
            .glyph_run(resources, run.font())
            .font_size(font_size)
            .hint(false)
            .normalized_coords(coords);
        if let Some(transform) = transform {
            builder = builder.glyph_transform(transform);
        }
        builder.stroke_glyphs(glyphs.iter().copied());
    }
}

/// A horizontal line under or through text: `baseline` and `offset` as Parley
/// gives them (offset positive above the baseline), at least `min_size` thick.
pub fn line(
    ctx: &mut RenderContext,
    color: Color,
    x: (f64, f64),
    baseline: f32,
    offset: f32,
    size: f32,
    min_size: f32,
) {
    if x.1 <= x.0 {
        return;
    }
    let size = size.max(min_size);
    let y = f64::from(baseline - offset);
    ctx.set_paint(color);
    ctx.fill_rect(&Rect::new(x.0, y, x.1, y + f64::from(size)));
}
