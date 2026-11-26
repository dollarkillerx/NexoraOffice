//! What the two PDF exports share: colours and paths, embedding fonts and
//! picking the text each glyph stands for, writing a run of glyphs, and
//! turning a decoded picture into an image.
//!
//! Nothing here keeps global state; a [`Fonts`] belongs to one export.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use aster_text::{DrawRun, Paragraph};
use krilla::Data;
use krilla::color::rgb;
use krilla::destination::XyzDestination;
use krilla::geom::{PathBuilder, Point, Transform};
use krilla::image::Image;
use krilla::num::NormalizedF32;
use krilla::outline::{Outline, OutlineNode};
use krilla::paint::{Fill, FillRule, Stroke};
use krilla::surface::Surface;
use krilla::text::{Font, GlyphId, KrillaGlyph, Tag};
use vello_cpu::Pixmap;
use vello_cpu::kurbo::{PathEl, Shape};

use crate::text_surface::Rgba8;

/// Outline width of made-up bold, as a fraction of the font size: the same
/// as on screen.
pub(crate) const SYNTHETIC_BOLD: f32 = 0.035;

pub(crate) fn paint(color: Rgba8) -> (krilla::paint::Paint, NormalizedF32) {
    let paint = rgb::Color::new(color[0], color[1], color[2]).into();
    let opacity = NormalizedF32::new(f32::from(color[3]) / 255.0).unwrap_or(NormalizedF32::ONE);
    (paint, opacity)
}

pub(crate) fn fill(color: Rgba8) -> Fill {
    let (paint, opacity) = paint(color);
    Fill {
        paint,
        opacity,
        rule: FillRule::NonZero,
    }
}

pub(crate) fn stroke(color: Rgba8, width: f64) -> Stroke {
    let (paint, opacity) = paint(color);
    Stroke {
        paint,
        width: width as f32,
        opacity,
        ..Stroke::default()
    }
}

/// A kurbo shape as a PDF path.
pub(crate) fn path(shape: &impl Shape) -> Option<krilla::geom::Path> {
    let mut builder = PathBuilder::new();
    let f = |value: f64| value as f32;
    for element in shape.path_elements(0.1) {
        match element {
            PathEl::MoveTo(p) => builder.move_to(f(p.x), f(p.y)),
            PathEl::LineTo(p) => builder.line_to(f(p.x), f(p.y)),
            PathEl::QuadTo(a, p) => builder.quad_to(f(a.x), f(a.y), f(p.x), f(p.y)),
            PathEl::CurveTo(a, b, p) => {
                builder.cubic_to(f(a.x), f(a.y), f(b.x), f(b.y), f(p.x), f(p.y))
            }
            PathEl::ClosePath => builder.close(),
        }
    }
    builder.finish()
}

/// The text each glyph of a line stands for, for the ToUnicode map and for
/// copying: glyphs in drawing order, each given the cluster under its
/// origin. Glyphs whose cluster could not be told apart share one, and
/// clusters without glyphs of their own (a line's trailing space, a bracket
/// drawn into the space of the character before it) join the glyph before
/// them, so that the pieces always add up to the whole line.
pub(crate) fn clusters(
    paragraph: &Paragraph,
    runs: &[aster_text::DrawRun<'_>],
    middle: f32,
    range: Range<usize>,
) -> Vec<Range<usize>> {
    let mut starts: Vec<usize> = Vec::new();
    let mut last = range.start;
    for run in runs {
        let nudge = (run.face.size * 0.05).min(1.0);
        for glyph in &run.glyphs {
            let mut start = paragraph.offset_at(glyph.x + nudge, middle);
            if start < last || start >= range.end {
                start = last;
            }
            if starts.is_empty() {
                start = range.start;
            }
            starts.push(start);
            last = start;
        }
    }
    let mut out = vec![0..0; starts.len()];
    let mut end = range.end;
    for index in (0..starts.len()).rev() {
        if index + 1 < starts.len() && starts[index + 1] != starts[index] {
            end = starts[index + 1];
        }
        out[index] = starts[index]..end.max(starts[index]);
    }
    out
}

/// A decoded picture as an image with straight alpha.
pub(crate) fn image(pixmap: &Pixmap) -> Image {
    let mut data = Vec::with_capacity(pixmap.data().len() * 4);
    for pixel in pixmap.data() {
        let a = pixel.a;
        let straight = |c: u8| {
            if a == 0 {
                0
            } else {
                ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8
            }
        };
        data.extend([straight(pixel.r), straight(pixel.g), straight(pixel.b), a]);
    }
    Image::from_rgba8(data, u32::from(pixmap.width()), u32::from(pixmap.height()))
}

// --- Variable fonts ---
//
// Parley hands out the instance of a variable font as normalised
// coordinates, after the font's `avar` mapping; krilla wants the axis values
// a user would pick. The two tables involved are read here directly.

fn be16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// A table of the font at `index` in a font file or collection.
fn table<'a>(data: &'a [u8], index: u32, tag: &[u8; 4]) -> Option<&'a [u8]> {
    let start = if data.get(0..4)? == b"ttcf" {
        be32(data, 12 + 4 * index as usize)? as usize
    } else {
        0
    };
    let count = usize::from(be16(data, start + 4)?);
    for record in (0..count).map(|i| start + 12 + 16 * i) {
        if data.get(record..record + 4)? == tag {
            let offset = be32(data, record + 8)? as usize;
            let length = be32(data, record + 12)? as usize;
            return data.get(offset..offset.checked_add(length)?);
        }
    }
    None
}

/// The axis values of an instance given by normalised coordinates, for the
/// axes that are not at their default.
pub(crate) fn variations(data: &[u8], index: u32, coords: &[i16]) -> Vec<(Tag, f32)> {
    if coords.iter().all(|c| *c == 0) {
        return Vec::new();
    }
    let Some(fvar) = table(data, index, b"fvar") else {
        return Vec::new();
    };
    let avar = table(data, index, b"avar");
    let (Some(axes_at), Some(count), Some(axis_size)) =
        (be16(fvar, 4), be16(fvar, 8), be16(fvar, 10))
    else {
        return Vec::new();
    };
    let fixed = |at: usize| be32(fvar, at).map(|v| v as i32 as f32 / 65536.0);
    // The segment maps of `avar`, one per axis, in order.
    let mut maps: Vec<Vec<(f32, f32)>> = Vec::new();
    if let Some(avar) = avar
        && let Some(axes) = be16(avar, 6)
    {
        let mut at = 8;
        for _ in 0..axes {
            let Some(pairs) = be16(avar, at) else { break };
            at += 2;
            let mut map = Vec::with_capacity(usize::from(pairs));
            for _ in 0..pairs {
                let (Some(from), Some(to)) = (be16(avar, at), be16(avar, at + 2)) else {
                    break;
                };
                let f2dot14 = |v: u16| f32::from(v as i16) / 16384.0;
                map.push((f2dot14(from), f2dot14(to)));
                at += 4;
            }
            maps.push(map);
        }
    }
    let mut out = Vec::new();
    for (axis, coord) in coords.iter().enumerate().take(usize::from(count)) {
        if *coord == 0 {
            continue;
        }
        let record = usize::from(axes_at) + axis * usize::from(axis_size);
        let (Some(tag), Some(min), Some(default), Some(max)) = (
            fvar.get(record..record + 4),
            fixed(record + 4),
            fixed(record + 8),
            fixed(record + 12),
        ) else {
            continue;
        };
        let mut value = f32::from(*coord) / 16384.0;
        if let Some(map) = maps.get(axis) {
            value = unmap(map, value);
        }
        let user = if value < 0.0 {
            default + value * (default - min)
        } else {
            default + value * (max - default)
        };
        let tag: [u8; 4] = tag.try_into().unwrap_or(*b"    ");
        out.push((Tag::new(&tag), user));
    }
    out
}

/// The coordinate an `avar` segment map sends to `value`.
pub(crate) fn unmap(map: &[(f32, f32)], value: f32) -> f32 {
    for pair in map.windows(2) {
        let ((from0, to0), (from1, to1)) = (pair[0], pair[1]);
        if value >= to0 && value <= to1 {
            if to1 - to0 <= f32::EPSILON {
                return from0;
            }
            return from0 + (value - to0) * (from1 - from0) / (to1 - to0);
        }
    }
    value
}

/// The fonts of one export: each font file, face and instance is embedded
/// once however many runs use it.
#[derive(Default)]
pub(crate) struct Fonts {
    fonts: HashMap<(u64, u32, Vec<i16>), Option<Font>>,
}

impl Fonts {
    /// The font of a run, once per font file, face and instance.
    pub(crate) fn get(&mut self, face: &aster_text::Face) -> Option<Font> {
        let key = (face.font.data.id(), face.font.index, face.coords.clone());
        self.fonts
            .entry(key)
            .or_insert_with(|| {
                let bytes = face.font.data.clone();
                let variations = variations(bytes.as_ref(), face.font.index, &face.coords);
                let data: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(bytes);
                Font::new_variable(Data::from(data), face.font.index, &variations)
            })
            .clone()
    }
}

/// Write the glyphs of `runs` as text. `(x, y)` is where the paragraph's
/// top left corner is, in the surface's space; `text` is the paragraph's
/// text and `clusters` what each glyph stands for ([`clusters`]), in the
/// order of the runs' glyphs. Each run has its own face and colour, made-up
/// bold (an outline in the text colour), made-up slant (a shear about the
/// baseline) and horizontal stretch.
pub(crate) fn draw_runs(
    surface: &mut Surface<'_>,
    fonts: &mut Fonts,
    text: &str,
    runs: &[DrawRun<'_>],
    clusters: &[Range<usize>],
    x: f64,
    y: f64,
) {
    let mut next = 0;
    for run in runs {
        let Some(font) = fonts.get(run.face) else {
            next += run.glyphs.len();
            continue;
        };
        let Some(first) = run.glyphs.first() else {
            continue;
        };
        let size = run.face.size;
        // Glyph positions are stretched already; the glyphs themselves are
        // stretched by the transform below, which stretches advances too.
        let stretch = if (run.x_scale - 1.0).abs() > 0.001 {
            run.x_scale
        } else {
            1.0
        };
        let glyphs: Vec<KrillaGlyph> = run
            .glyphs
            .iter()
            .enumerate()
            .map(|(index, glyph)| {
                let advance = run
                    .glyphs
                    .get(index + 1)
                    .map_or(0.0, |after| after.x - glyph.x);
                KrillaGlyph {
                    glyph_id: GlyphId::new(glyph.id),
                    text_range: clusters[next + index].clone(),
                    x_advance: advance / size / stretch,
                    x_offset: 0.0,
                    y_offset: (first.y - glyph.y) / size,
                    y_advance: 0.0,
                    location: None,
                }
            })
            .collect();
        next += run.glyphs.len();
        let origin = (x + f64::from(first.x), y + f64::from(first.y));
        let skew = run.skew.map(|degrees| degrees.to_radians().tan());
        let transformed = skew.is_some() || stretch != 1.0;
        if transformed {
            // Stretch about the run's start, then lean the tops to the
            // right about the baseline.
            let tan = skew.unwrap_or(0.0);
            let baseline = origin.1 as f32;
            let left = origin.0 as f32;
            surface.push_transform(&Transform::from_row(
                stretch,
                0.0,
                -tan,
                1.0,
                left * (1.0 - stretch) + tan * baseline,
                0.0,
            ));
        }
        surface.set_fill(Some(fill(run.color)));
        surface.set_stroke(
            run.face
                .embolden
                .then(|| stroke(run.color, f64::from(size * SYNTHETIC_BOLD))),
        );
        surface.draw_glyphs(
            Point::from_xy(origin.0 as f32, origin.1 as f32),
            &glyphs,
            font,
            text,
            size,
            false,
        );
        surface.set_stroke(None);
        if transformed {
            surface.pop();
        }
    }
}

/// Bookmarks from entries of a level (above 0), a title and a place, nested
/// by level: an entry belongs to the nearest one above it with a lower level.
pub(crate) fn bookmarks(entries: &[(u8, String, XyzDestination)]) -> Outline {
    let mut bookmarks = Outline::new();
    let mut at = 0;
    for node in nest(entries, &mut at, 0) {
        bookmarks.push_child(node);
    }
    bookmarks
}

/// Bookmarks below `level`, from `entries[*at..]`, until one at `level` or
/// above.
fn nest(entries: &[(u8, String, XyzDestination)], at: &mut usize, level: u8) -> Vec<OutlineNode> {
    let mut out = Vec::new();
    while let Some((own, title, destination)) = entries.get(*at) {
        if *own <= level {
            break;
        }
        *at += 1;
        let mut node = OutlineNode::new(title.clone(), destination.clone());
        for child in nest(entries, at, *own) {
            node.push_child(child);
        }
        out.push(node);
    }
    out
}
