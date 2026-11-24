//! Paragraph layout: Parley shapes the text and picks fallback fonts, our
//! breaker decides where lines end, and the result knows where every
//! character is: for drawing, for hit testing and for selections.
//!
//! Parley is asked for one unbroken line (hard breaks aside). Its own line
//! breaker is not used, so the Chinese and Japanese rules of
//! [`crate::breaker`] apply. Nothing here draws: [`Paragraph::draw_runs`] and
//! [`Paragraph::decorations`] describe the glyphs and lines for whoever does.
//!
//! Positions are in physical pixels with the paragraph's top-left at 0, 0.
//! Only left-to-right text is supported.

use std::borrow::Cow;
use std::ops::Range;

use parley::fontique::{FontStyle as FaceStyle, Script};
use parley::{
    FontContext, FontData, FontFamily, FontStyle, FontWeight, GenericFamily, LayoutContext,
    StyleProperty,
};

use crate::breaker::{self, BreakKind, Cluster, EndTreatment, Fx, Options};
use crate::class::{Class, classify};
use crate::segment::break_opportunities;

/// Opaque RGBA, 8 bits per channel.
pub type Rgba = [u8; 4];

/// Slant of made-up italics, in degrees.
pub const SYNTHETIC_SKEW_DEGREES: f32 = 12.0;

/// What Parley carries per style; the real styles are looked up by position.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct NoBrush;

#[derive(Clone, Debug, PartialEq)]
pub enum Family {
    /// The system's interface font.
    SystemUi,
    /// A CSS-style list of family names, best first.
    List(Cow<'static, str>),
}

/// How a stretch of text looks.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub family: Family,
    /// In logical pixels.
    pub size: f32,
    pub color: Rgba,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Logical pixels to raise the text above the baseline, as for a
    /// footnote number.
    pub rise: f32,
}

/// A stretch of text that does not look like the paragraph's base style.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    /// Byte range in the paragraph text.
    pub range: Range<usize>,
    pub style: Style,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
}

/// Which of the breaker's rules apply. Hard rules (no closing mark at the
/// start of a line, no opening mark at the end) always do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rules {
    /// Squeeze the blank halves of adjacent full-width marks. Assumes the
    /// blank half is where Simplified Chinese and Japanese fonts put it, so it
    /// is off for Traditional Chinese, whose marks sit in the middle.
    pub compress: bool,
    /// Let a closing mark at the end of a line give up its blank half, and a
    /// comma or full stop hang past the margin.
    pub line_end: bool,
    /// A quarter em between Han or kana and Latin letters or digits.
    pub autospace: bool,
    pub justify: bool,
}

impl Rules {
    /// What a paragraph in `language` gets by default, unjustified.
    pub fn for_language(language: &str) -> Self {
        let traditional = language.starts_with("zh-Hant")
            || language.starts_with("zh-TW")
            || language.starts_with("zh-HK");
        Self {
            compress: !traditional,
            line_end: true,
            autospace: true,
            justify: false,
        }
    }

    fn options(self, max_width: Fx) -> Options {
        Options {
            max_width,
            compress_adjacent: self.compress,
            compress_line_end: self.line_end,
            hang: self.line_end,
            autospace: self.autospace,
            justify: self.justify,
        }
    }
}

/// Everything needed to lay out one paragraph.
#[derive(Clone, Debug)]
pub struct Spec<'a> {
    pub text: &'a str,
    pub base: Style,
    /// Sorted, not overlapping; what they do not cover has the base style.
    pub spans: &'a [Span],
    /// BCP 47; chooses the line-breaking tailoring and the glyph forms.
    pub language: &'a str,
    /// Physical pixels; `None` lays the text out on as few lines as its hard
    /// breaks allow.
    pub width: Option<f32>,
    /// Physical pixels per logical pixel.
    pub scale: f32,
    /// Line height as a multiple of the largest font size on the line.
    pub line_height: f32,
    pub align: Align,
    pub rules: Rules,
}

/// A font at a size, as one run of the shaped text used it.
#[derive(Clone, Debug)]
pub struct Face {
    pub font: FontData,
    /// Physical pixels.
    pub size: f32,
    pub coords: Vec<i16>,
    pub embolden: bool,
    /// Degrees of made-up slant, if any.
    pub skew: Option<f32>,
    ascent: f32,
    descent: f32,
    underline_offset: f32,
    underline_size: f32,
    strike_offset: f32,
    strike_size: f32,
    /// The face is not italic although italic was asked for.
    upright: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub id: u32,
    pub x: f32,
    pub y: f32,
}

/// Glyphs of one face in one colour, positioned in paragraph coordinates.
#[derive(Clone, Debug)]
pub struct DrawRun<'a> {
    pub face: &'a Face,
    pub color: Rgba,
    /// Slant to draw with, in degrees: the face's own made-up italic, or one
    /// added because italic was asked for and the face has none.
    pub skew: Option<f32>,
    pub glyphs: Vec<Glyph>,
}

/// An underline or strikethrough, in paragraph coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decoration {
    pub x0: f32,
    pub x1: f32,
    /// Top of the line.
    pub y: f32,
    pub thickness: f32,
    pub color: Rgba,
}

/// Per cluster: which face, which glyphs, which style.
#[derive(Clone, Debug)]
struct Look {
    face: Option<usize>,
    glyphs: Range<usize>,
    /// Glyph offsets within the cluster, as Parley gave them.
    style: usize,
}

#[derive(Clone, Debug)]
struct ShapedGlyph {
    id: u32,
    x: f32,
    y: f32,
    advance: f32,
}

/// One line on screen.
#[derive(Clone, Debug)]
pub struct LineBox {
    pub clusters: Range<usize>,
    pub top: f32,
    pub height: f32,
    pub baseline: f32,
    /// Left edge of the line, from alignment.
    pub left: f32,
    /// Width of what must stay inside the line.
    pub width: f32,
    pub kind: BreakKind,
    pub end: EndTreatment,
    placed: Vec<breaker::Placed>,
}

/// A laid-out paragraph.
#[derive(Clone, Debug)]
pub struct Paragraph {
    text: String,
    clusters: Vec<Cluster>,
    looks: Vec<Look>,
    faces: Vec<Face>,
    glyphs: Vec<ShapedGlyph>,
    styles: Vec<Style>,
    lines: Vec<LineBox>,
    height: f32,
    /// The width of the widest line.
    width: f32,
    scale: f32,
}

/// Shapes and lays out paragraphs. Owns its fonts, so it can live on a
/// worker thread.
pub struct Shaper {
    font_cx: FontContext,
    layout_cx: LayoutContext<NoBrush>,
}

impl Default for Shaper {
    fn default() -> Self {
        Self::new()
    }
}

fn to_fx(value: f32) -> Fx {
    (value * 64.0).round() as Fx
}

fn from_fx(value: Fx) -> f32 {
    value as f32 / 64.0
}

fn family_property(family: &Family) -> StyleProperty<'static, NoBrush> {
    match family {
        Family::SystemUi => StyleProperty::from(GenericFamily::SystemUi),
        Family::List(list) => StyleProperty::FontFamily(FontFamily::Source(list.clone())),
    }
}

impl Shaper {
    pub fn new() -> Self {
        Self::with_fonts(FontContext::default())
    }

    /// Use a font context the caller has set up, such as with fallbacks.
    pub fn with_fonts(font_cx: FontContext) -> Self {
        Self {
            font_cx,
            layout_cx: LayoutContext::default(),
        }
    }

    /// Prefer `families` for Han characters, where installed. Parley would
    /// otherwise pick whatever the system lists first, which on macOS can be
    /// a Japanese or Traditional font for Simplified text.
    pub fn set_han_fallbacks(&mut self, families: &[&str]) {
        let han = Script::from_bytes(*b"Hani");
        let found: Vec<_> = families
            .iter()
            .filter_map(|name| self.font_cx.collection.family_id(name))
            .collect();
        if !found.is_empty() {
            self.font_cx
                .collection
                .set_fallbacks(han, found.into_iter());
        }
    }

    pub fn lay_out(&mut self, spec: &Spec<'_>) -> Paragraph {
        let text = spec.text;
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, spec.scale, true);
        builder.push_default(family_property(&spec.base.family));
        builder.push_default(StyleProperty::FontSize(spec.base.size));
        builder.push_default(StyleProperty::Locale(spec.language.parse().ok()));
        if spec.base.bold {
            builder.push_default(StyleProperty::FontWeight(FontWeight::BOLD));
        }
        if spec.base.italic {
            builder.push_default(StyleProperty::FontStyle(FontStyle::Italic));
        }
        for span in spec.spans {
            let range = span.range.clone();
            let style = &span.style;
            builder.push(family_property(&style.family), range.clone());
            builder.push(StyleProperty::FontSize(style.size), range.clone());
            builder.push(
                StyleProperty::FontWeight(if style.bold {
                    FontWeight::BOLD
                } else {
                    FontWeight::NORMAL
                }),
                range.clone(),
            );
            builder.push(
                StyleProperty::FontStyle(if style.italic {
                    FontStyle::Italic
                } else {
                    FontStyle::Normal
                }),
                range,
            );
        }
        let mut layout = builder.build(text);
        layout.break_all_lines(None);

        let mut styles = vec![spec.base.clone()];
        styles.extend(spec.spans.iter().map(|span| span.style.clone()));
        // The spans are sorted and do not overlap: the last one starting at
        // or before `offset` is the only one that can hold it.
        let style_at = |offset: usize| -> usize {
            let after = spec
                .spans
                .partition_point(|span| span.range.start <= offset);
            match after.checked_sub(1) {
                Some(index) if spec.spans[index].range.contains(&offset) => index + 1,
                _ => 0,
            }
        };

        let breaks = break_opportunities(text, spec.language);
        let base_em = to_fx(spec.base.size * spec.scale);
        let mut faces: Vec<Face> = Vec::new();
        let mut glyphs: Vec<ShapedGlyph> = Vec::new();
        let mut found: Vec<(Cluster, Look)> = Vec::new();
        for line in layout.lines() {
            for run in line.runs() {
                let metrics = run.font_metrics();
                let synthesis = run.synthesis();
                let face_index = faces.len();
                faces.push(Face {
                    font: run.font().clone(),
                    size: run.font_size(),
                    coords: bytemuck::cast_slice::<_, i16>(run.normalized_coords()).to_vec(),
                    embolden: synthesis.embolden(),
                    skew: synthesis.skew(),
                    ascent: metrics.ascent,
                    descent: metrics.descent,
                    underline_offset: metrics.underline_offset,
                    underline_size: metrics.underline_size,
                    strike_offset: metrics.strikethrough_offset,
                    strike_size: metrics.strikethrough_size,
                    upright: run.font_attrs().style == FaceStyle::Normal,
                });
                for cluster in run.clusters() {
                    let range = cluster.text_range();
                    if range.is_empty() {
                        continue;
                    }
                    let first = text[range.clone()].chars().next().unwrap_or(' ');
                    let newline = cluster.is_hard_line_break();
                    let start = glyphs.len();
                    glyphs.extend(cluster.glyphs().map(|g| ShapedGlyph {
                        id: g.id,
                        x: g.x,
                        y: g.y,
                        advance: g.advance,
                    }));
                    let style = style_at(range.start);
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
                        Look {
                            face: Some(face_index),
                            glyphs: start..glyphs.len(),
                            style,
                        },
                    ));
                }
            }
        }
        found.sort_by_key(|(cluster, _)| cluster.text.start);

        // Parley may not report a cluster for every character (a newline,
        // for one). The breaker needs the text covered without holes.
        let mut clusters = Vec::with_capacity(found.len());
        let mut looks = Vec::with_capacity(found.len());
        let mut offset = 0;
        let fill = |from: usize, to: usize, clusters: &mut Vec<Cluster>, looks: &mut Vec<Look>| {
            for (at, c) in text[from..to].char_indices() {
                let start = from + at;
                clusters.push(Cluster {
                    text: start..start + c.len_utf8(),
                    class: classify(c),
                    advance: 0,
                    em: base_em,
                    break_before: start > 0 && breaks.binary_search(&start).is_ok(),
                    newline: c == '\n',
                });
                looks.push(Look {
                    face: None,
                    glyphs: 0..0,
                    style: style_at(start),
                });
            }
        };
        for (cluster, look) in found {
            if cluster.text.start < offset {
                continue;
            }
            fill(offset, cluster.text.start, &mut clusters, &mut looks);
            offset = cluster.text.end;
            clusters.push(cluster);
            looks.push(look);
        }
        fill(offset, text.len(), &mut clusters, &mut looks);

        let max_width = spec.width.map_or(Fx::MAX / 4, to_fx);
        let broken = breaker::break_lines(&clusters, spec.rules.options(max_width));

        let mut paragraph = Paragraph {
            text: text.to_owned(),
            clusters,
            looks,
            faces,
            glyphs,
            styles,
            lines: Vec::with_capacity(broken.len()),
            height: 0.0,
            width: 0.0,
            scale: spec.scale,
        };
        let line_factor = spec.line_height;
        let mut top = 0.0;
        for line in broken {
            let (mut ascent, mut descent, mut size) = (0.0f32, 0.0f32, 0.0f32);
            for index in line.clusters.clone() {
                let look = &paragraph.looks[index];
                let style_size = paragraph.styles[look.style].size * spec.scale;
                size = size.max(style_size);
                if let Some(face) = look.face.map(|f| &paragraph.faces[f]) {
                    ascent = ascent.max(face.ascent);
                    descent = descent.max(face.descent);
                }
            }
            if size == 0.0 {
                size = spec.base.size * spec.scale;
            }
            if ascent == 0.0 && descent == 0.0 {
                ascent = size * 0.8;
                descent = size * 0.2;
            }
            let height = (size * line_factor).max(ascent + descent).round();
            let baseline = top + ((height - (ascent + descent)) / 2.0 + ascent).round();
            let width = from_fx(line.width);
            paragraph.width = paragraph.width.max(width);
            paragraph.lines.push(LineBox {
                clusters: line.clusters,
                top,
                height,
                baseline,
                left: 0.0,
                width,
                kind: line.kind,
                end: line.end,
                placed: line.placed,
            });
            top += height;
        }
        if paragraph.lines.is_empty() {
            // An empty paragraph still takes a line.
            let size = spec.base.size * spec.scale;
            let height = (size * line_factor).round();
            paragraph.lines.push(LineBox {
                clusters: 0..0,
                top: 0.0,
                height,
                baseline: (height / 2.0 + size * 0.3).round(),
                left: 0.0,
                width: 0.0,
                kind: BreakKind::End,
                end: EndTreatment::None,
                placed: Vec::new(),
            });
            top = height;
        }
        paragraph.height = top;
        let container = spec.width.unwrap_or(paragraph.width);
        for line in &mut paragraph.lines {
            line.left = match spec.align {
                Align::Start => 0.0,
                Align::Center => ((container - line.width) / 2.0).max(0.0).round(),
                Align::End => (container - line.width).max(0.0).round(),
            };
        }
        paragraph
    }
}

impl Paragraph {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn height(&self) -> f32 {
        self.height
    }

    /// The width of the widest line.
    pub fn width(&self) -> f32 {
        self.width
    }

    pub fn lines(&self) -> &[LineBox] {
        &self.lines
    }

    /// The byte range of each line, without its line break.
    pub fn line_ranges(&self) -> Vec<Range<usize>> {
        self.lines
            .iter()
            .map(|line| {
                if line.clusters.is_empty() {
                    return 0..0;
                }
                let start = self.clusters[line.clusters.start].text.start;
                let mut end = self.clusters[line.clusters.end - 1].text.end;
                if self.clusters[line.clusters.end - 1].newline {
                    end = self.clusters[line.clusters.end - 1].text.start;
                }
                start..end
            })
            .collect()
    }

    /// The slant a cluster is drawn with.
    fn skew(&self, look: &Look, face: &Face) -> Option<f32> {
        let italic = self.styles[look.style].italic;
        face.skew
            .or_else(|| (italic && face.upright).then_some(SYNTHETIC_SKEW_DEGREES))
    }

    /// The glyphs on the lines that overlap `top..bottom`, grouped by face,
    /// colour and slant.
    pub fn draw_runs(&self, top: f32, bottom: f32) -> Vec<DrawRun<'_>> {
        let mut runs: Vec<DrawRun<'_>> = Vec::new();
        for line in self.visible_lines(top, bottom) {
            for placed in &line.placed {
                let look = &self.looks[placed.index];
                let Some(face_index) = look.face else {
                    continue;
                };
                if look.glyphs.is_empty() || self.clusters[placed.index].newline {
                    continue;
                }
                let face = &self.faces[face_index];
                let color = self.styles[look.style].color;
                let skew = self.skew(look, face);
                let same = runs.last().is_some_and(|run| {
                    std::ptr::eq(run.face, face) && run.color == color && run.skew == skew
                });
                if !same {
                    runs.push(DrawRun {
                        face,
                        color,
                        skew,
                        glyphs: Vec::new(),
                    });
                }
                let run = runs.last_mut().expect("pushed above");
                let mut pen = line.left + from_fx(placed.glyph_x);
                let baseline = line.baseline - self.styles[look.style].rise * self.scale;
                for glyph in &self.glyphs[look.glyphs.clone()] {
                    run.glyphs.push(Glyph {
                        id: glyph.id,
                        x: pen + glyph.x,
                        y: baseline + glyph.y,
                    });
                    pen += glyph.advance;
                }
            }
        }
        runs
    }

    /// Underlines and strikethroughs on the lines that overlap `top..bottom`.
    /// `min_thickness` keeps hairlines visible.
    pub fn decorations(&self, top: f32, bottom: f32, min_thickness: f32) -> Vec<Decoration> {
        let mut out: Vec<Decoration> = Vec::new();
        for line in self.visible_lines(top, bottom) {
            for placed in &line.placed {
                let cluster = &self.clusters[placed.index];
                if cluster.newline || cluster.advance == 0 {
                    continue;
                }
                let look = &self.looks[placed.index];
                let style = &self.styles[look.style];
                let Some(face) = look.face.map(|f| &self.faces[f]) else {
                    continue;
                };
                let x0 = line.left + from_fx(placed.x);
                let x1 = x0 + from_fx(placed.advance);
                let baseline = line.baseline - style.rise * self.scale;
                let mut add = |offset: f32, size: f32| {
                    let decoration = Decoration {
                        x0,
                        x1,
                        y: baseline - offset,
                        thickness: size.max(min_thickness),
                        color: style.color,
                    };
                    // Join with the piece just before on the same line.
                    if let Some(last) = out.last_mut()
                        && last.y == decoration.y
                        && last.color == decoration.color
                        && (last.x1 - decoration.x0).abs() < 1.0
                    {
                        last.x1 = decoration.x1;
                    } else {
                        out.push(decoration);
                    }
                };
                if style.underline {
                    add(face.underline_offset, face.underline_size);
                }
                if style.strike {
                    add(face.strike_offset, face.strike_size);
                }
            }
        }
        out
    }

    fn visible_lines(&self, top: f32, bottom: f32) -> impl Iterator<Item = &LineBox> {
        let first = self
            .lines
            .partition_point(|line| line.top + line.height < top);
        self.lines[first..]
            .iter()
            .take_while(move |line| line.top <= bottom)
    }

    /// The line at height `y`, the first or last when outside.
    fn line_at(&self, y: f32) -> &LineBox {
        let index = self
            .lines
            .partition_point(|line| line.top + line.height <= y)
            .min(self.lines.len() - 1);
        &self.lines[index]
    }

    /// The text position nearest to a point: before the character whose
    /// left half the point is in, after it if in its right half.
    pub fn offset_at(&self, x: f32, y: f32) -> usize {
        let line = self.line_at(y);
        if line.clusters.is_empty() {
            return 0;
        }
        for placed in &line.placed {
            let cluster = &self.clusters[placed.index];
            if cluster.newline {
                return cluster.text.start;
            }
            let left = line.left + from_fx(placed.x);
            if x < left + from_fx(placed.advance) / 2.0 {
                return cluster.text.start;
            }
        }
        let last = &self.clusters[line.clusters.end - 1];
        if last.newline {
            last.text.start
        } else {
            last.text.end
        }
    }

    /// Rectangles covering a byte range, one per line, as `[x0, y0, x1, y1]`.
    pub fn rects(&self, range: Range<usize>) -> Vec<[f32; 4]> {
        let mut out = Vec::new();
        if range.is_empty() {
            return out;
        }
        for line in &self.lines {
            let mut x0 = f32::INFINITY;
            let mut x1 = f32::NEG_INFINITY;
            for placed in &line.placed {
                let cluster = &self.clusters[placed.index];
                if cluster.text.end <= range.start || cluster.text.start >= range.end {
                    continue;
                }
                let left = line.left + from_fx(placed.x);
                // A selected line break shows as a little room after the line.
                let width = if cluster.newline {
                    self.styles[0].size / 3.0
                } else {
                    from_fx(placed.advance)
                };
                x0 = x0.min(left);
                x1 = x1.max(left + width);
            }
            if x0 < x1 {
                out.push([x0, line.top, x1, line.top + line.height]);
            }
        }
        out
    }

    /// The text of clusters no font has a glyph for. They are drawn with
    /// the fallback font's "missing glyph" (`.notdef`), which in the macOS
    /// system font is a box of horizontal bars. Spaces and line breaks are
    /// left out.
    pub fn undrawable(&self) -> Vec<&str> {
        let mut out = Vec::new();
        for (cluster, look) in self.clusters.iter().zip(&self.looks) {
            let text = &self.text[cluster.text.clone()];
            if cluster.newline || text.chars().all(char::is_whitespace) {
                continue;
            }
            let found = look.face.is_some()
                && !look.glyphs.is_empty()
                && self.glyphs[look.glyphs.clone()]
                    .iter()
                    .all(|glyph| glyph.id != 0);
            if !found {
                out.push(text);
            }
        }
        out
    }

    /// The class of the character at a byte offset, for tests and callers
    /// that care about punctuation.
    pub fn class_at(&self, offset: usize) -> Option<Class> {
        self.clusters
            .iter()
            .find(|cluster| cluster.text.contains(&offset))
            .map(|cluster| cluster.class)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(size: f32) -> Style {
        Style {
            family: Family::SystemUi,
            size,
            color: [0, 0, 0, 255],
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            rise: 0.0,
        }
    }

    fn spec<'a>(text: &'a str, spans: &'a [Span], width: Option<f32>) -> Spec<'a> {
        Spec {
            text,
            base: style(16.0),
            spans,
            language: crate::segment::guess_language(text, "zh-Hans"),
            width,
            scale: 1.0,
            line_height: 1.6,
            align: Align::Start,
            rules: Rules::for_language("zh-Hans"),
        }
    }

    fn shaper() -> Shaper {
        let mut shaper = Shaper::new();
        shaper.set_han_fallbacks(&["Hiragino Sans GB", "Heiti SC", "Songti SC", "Hiragino Sans"]);
        shaper
    }

    const PROSE: &str = "排版引擎需要处理“禁则”：句号、逗号和后引号（」』）》等）不能出现在行首，前引号（「『（《等）不能出现在行尾。日本語の組版では、行頭に句読点や閉じ括弧（」）、小書きの仮名（ゃ・ゅ・ょ・っ）、長音記号「ー」などを置くことはできません。";

    #[test]
    fn no_line_starts_with_a_closing_mark_or_ends_with_an_opening_one() {
        let mut shaper = shaper();
        for width in (100..=520).step_by(17) {
            let paragraph = shaper.lay_out(&spec(PROSE, &[], Some(width as f32)));
            let ranges = paragraph.line_ranges();
            assert!(ranges.len() > 1, "width {width}");
            let mut covered = 0;
            for (n, range) in ranges.iter().enumerate() {
                assert_eq!(range.start, covered, "width {width}: gap before line {n}");
                covered = range.end;
                let line = &PROSE[range.clone()];
                let first = line.chars().next().unwrap();
                let last = line.chars().next_back().unwrap();
                // A run with no allowed break that is wider than the line
                // has to be cut somewhere; the rules hold everywhere else.
                let after_emergency =
                    n > 0 && paragraph.lines()[n - 1].kind == BreakKind::Emergency;
                if n > 0 && !after_emergency {
                    assert!(
                        !"。，、」』）》：；！？ゃゅょっー”".contains(first),
                        "width {width}: line {n} starts with {first}: {line}"
                    );
                }
                if n + 1 < ranges.len() && paragraph.lines()[n].kind != BreakKind::Emergency {
                    assert!(
                        !"「『（《“".contains(last),
                        "width {width}: line {n} ends with {last}: {line}"
                    );
                }
                let fits = paragraph.lines()[n].kind == BreakKind::Emergency
                    || paragraph.lines()[n].width <= width as f32 + 0.01;
                assert!(fits, "width {width}: line {n} is too wide");
            }
            assert_eq!(covered, PROSE.len());
        }
    }

    #[test]
    fn hit_testing_and_rectangles_agree() {
        let mut shaper = shaper();
        let text = "中文和 English 混排的一段文字，用来测试命中。";
        let paragraph = shaper.lay_out(&spec(text, &[], Some(160.0)));
        assert!(paragraph.lines().len() >= 2);
        for (offset, _) in text.char_indices() {
            let rects =
                paragraph.rects(offset..offset + text[offset..].chars().next().unwrap().len_utf8());
            if rects.is_empty() {
                continue; // a space at the end of a line
            }
            let [x0, y0, x1, y1] = rects[0];
            let middle_y = (y0 + y1) / 2.0;
            // Left of the middle: before it; right of the middle: after it.
            assert_eq!(paragraph.offset_at(x0 + 0.5, middle_y), offset, "{offset}");
            let after = offset + text[offset..].chars().next().unwrap().len_utf8();
            if x1 - x0 > 2.0 {
                assert_eq!(paragraph.offset_at(x1 - 0.5, middle_y), after, "{offset}");
            }
        }
        // Far outside: the ends.
        assert_eq!(paragraph.offset_at(-50.0, -50.0), 0);
        assert_eq!(paragraph.offset_at(1e6, 1e6), text.len());
        // A range over two lines gives two rectangles.
        assert_eq!(
            paragraph.rects(0..text.len()).len(),
            paragraph.lines().len()
        );
    }

    #[test]
    fn hard_breaks_end_lines_and_empty_text_takes_one() {
        let mut shaper = shaper();
        let paragraph = shaper.lay_out(&spec("一行\n二行", &[], None));
        assert_eq!(paragraph.line_ranges(), [0..6, 7..13]);
        let empty = shaper.lay_out(&spec("", &[], Some(100.0)));
        assert_eq!(empty.lines().len(), 1);
        assert!(empty.height() > 16.0);
        assert_eq!(empty.offset_at(10.0, 10.0), 0);
    }

    #[test]
    fn emphasis_is_slanted_where_the_font_has_no_italic() {
        let mut shaper = shaper();
        let text = "强调的中文 and English";
        let mut italic = style(16.0);
        italic.italic = true;
        let spans = [Span {
            range: 0..text.len(),
            style: italic,
        }];
        let paragraph = shaper.lay_out(&spec(text, &spans, None));
        let runs = paragraph.draw_runs(0.0, 100.0);
        assert!(!runs.is_empty());
        // The Chinese is drawn slanted; every run is either slanted or from a
        // real italic face.
        let han = runs
            .iter()
            .find(|run| run.glyphs.len() >= 5)
            .expect("a run for the Chinese");
        assert!(han.skew.is_some(), "Chinese emphasis is not slanted");
        for run in &runs {
            assert!(run.skew.is_some() || !run.face.upright);
        }
        // Without emphasis nothing is slanted.
        let plain = shaper.lay_out(&spec(text, &[], None));
        assert!(
            plain
                .draw_runs(0.0, 100.0)
                .iter()
                .all(|run| run.skew.is_none())
        );
    }

    #[test]
    fn alignment_moves_lines_not_text() {
        let mut shaper = shaper();
        let mut at_end = spec("右", &[], Some(200.0));
        at_end.align = Align::End;
        let paragraph = shaper.lay_out(&at_end);
        let line = &paragraph.lines()[0];
        assert!((line.left + line.width - 200.0).abs() < 1.0);
        let rects = paragraph.rects(0..3);
        assert!((rects[0][2] - 200.0).abs() < 1.0);
    }

    #[test]
    fn characters_no_font_has_are_found() {
        let mut shaper = shaper();
        let ordinary = shaper.lay_out(&spec("中文、日本語、English：↑ • … — №", &[], None));
        assert!(
            ordinary.undrawable().is_empty(),
            "{:?}",
            ordinary.undrawable()
        );
        // ↩ is in none of the fonts the fallback looks at, so it comes out as
        // the system font's missing glyph: a box of bars. The footnote back
        // link used it at first.
        let hook = shaper.lay_out(&spec("回到↩正文", &[], None));
        assert_eq!(hook.undrawable(), ["↩"]);
        // Emoji are found, in the colour emoji font, and the renderer draws
        // its bitmaps.
        if cfg!(target_os = "macos") {
            let emoji = shaper.lay_out(&spec("表情😀和👍", &[], None));
            assert!(emoji.undrawable().is_empty(), "{:?}", emoji.undrawable());
        }
    }

    #[test]
    fn underlines_join_across_clusters() {
        let mut shaper = shaper();
        let text = "链接文字";
        let mut link = style(16.0);
        link.underline = true;
        let spans = [Span {
            range: 0..text.len(),
            style: link,
        }];
        let paragraph = shaper.lay_out(&spec(text, &spans, None));
        let lines = paragraph.decorations(0.0, 100.0, 1.0);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].x1 - lines[0].x0 > 50.0);
    }
}
