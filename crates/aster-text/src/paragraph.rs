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

use crate::breaker::{self, BreakKind, Cluster, EndTreatment, Fx, Options, Shape, Stop};
pub use crate::breaker::{Leader, TabAlign};
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
            distribute: false,
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

/// A tab stop, in logical pixels from the margin [`Extra::margin_offset`]
/// is counted from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabStop {
    pub position: f32,
    pub align: TabAlign,
    pub leader: Leader,
}

/// A thing in the line that is not text: the U+FFFC at `offset` is given a
/// width and a height. It sits on the baseline and makes its line as high
/// as it needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InlineObject {
    /// Byte offset of its U+FFFC in the text.
    pub offset: usize,
    /// Logical pixels.
    pub width: f32,
    pub height: f32,
}

/// Space added after every character of a range, logical pixels; negative
/// to tighten.
#[derive(Clone, Debug, PartialEq)]
pub struct Spacing {
    pub range: Range<usize>,
    pub amount: f32,
}

/// How a line's height is worked out.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum LineRule {
    /// [`Spec::line_height`] times the largest font size, but never less than
    /// the fonts' ascent and descent. What slides and previews use.
    #[default]
    Legacy,
    /// A multiple of the natural height: ascent, descent and line gap of the
    /// largest face on the line, which is Word's single spacing at 1.0.
    Multiple(f32),
    /// The natural height, but no less than this many logical pixels.
    AtLeast(f32),
    /// This many logical pixels whatever the text.
    Exactly(f32),
}

/// A line drawn under or through text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Stroke {
    #[default]
    None,
    Single,
    Double,
    /// Twice as thick.
    Thick,
    Dotted,
    Dashed,
    Wave,
}

/// How a decoration is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LineKind {
    #[default]
    Solid,
    Dotted,
    Dashed,
    Wave,
}

/// Room kept above a range, for what is drawn there (a ruby).
#[derive(Clone, Debug, PartialEq)]
pub struct Above {
    pub range: Range<usize>,
    /// Logical pixels above the range's own ascent.
    pub height: f32,
}

/// A range stretched or squeezed horizontally.
#[derive(Clone, Debug, PartialEq)]
pub struct Scaling {
    pub range: Range<usize>,
    pub factor: f32,
}

/// Underline and strikethrough for a range, with more choice than
/// [`Style::underline`] and [`Style::strike`] give.
#[derive(Clone, Debug, PartialEq)]
pub struct Decor {
    pub range: Range<usize>,
    pub underline: Stroke,
    /// The underline skips spaces.
    pub words_only: bool,
    pub strike: Stroke,
    /// The underline's colour; `None` for the text's.
    pub color: Option<Rgba>,
}

/// The natural line height of a range, as a multiple of its font size.
#[derive(Clone, Debug, PartialEq)]
pub struct Natural {
    pub range: Range<usize>,
    pub ratio: f32,
}

/// What a word processor adds to a paragraph. Lengths are in logical pixels
/// (they are multiplied by [`Spec::scale`]).
#[derive(Clone, Debug)]
pub struct Extra<'a> {
    /// The first line starts this far in; negative to hang it out to the
    /// left of the others.
    pub first_line: f32,
    /// Tab stops, counted from a margin that is `margin_offset` to the left
    /// of the paragraph's left edge (that is, the paragraph's left indent).
    pub tabs: &'a [TabStop],
    pub margin_offset: f32,
    /// Interval of the stops after the last explicit one; 0 gives a tab the
    /// width of its glyph.
    pub default_tab: f32,
    /// Sorted by offset.
    pub objects: &'a [InlineObject],
    pub spacing: &'a [Spacing],
    /// Sorted, not overlapping.
    pub decor: &'a [Decor],
    pub line: LineRule,
    /// For ranges whose font the machine does not have: how many times the
    /// font size a line of it is high (the font the document names has these
    /// metrics, not the substitute). Sorted, not overlapping.
    pub natural: &'a [Natural],
    /// Room above ranges, sorted.
    pub above: &'a [Above],
    /// Horizontal stretch of ranges, sorted.
    pub scaling: &'a [Scaling],
    /// The same for an empty line.
    pub base_natural: Option<f32>,
    /// Lines are rounded up to whole multiples of this, the text centred
    /// (the grid of East Asian documents).
    pub grid: Option<f32>,
    /// With `Rules::justify`: the last line and lines ended by a line break
    /// are justified too.
    pub distribute: bool,
    /// Round heights and positions to whole pixels, as the plain layout does.
    /// Off for a layout in points that is scaled later.
    pub snap: bool,
    /// A line break at the very end of the text is followed by an empty line
    /// to put the caret on.
    pub trailing_line: bool,
}

impl Default for Extra<'_> {
    fn default() -> Self {
        Self {
            first_line: 0.0,
            tabs: &[],
            margin_offset: 0.0,
            default_tab: 0.0,
            objects: &[],
            spacing: &[],
            decor: &[],
            line: LineRule::Legacy,
            natural: &[],
            above: &[],
            scaling: &[],
            base_natural: None,
            grid: None,
            distribute: false,
            snap: true,
            trailing_line: false,
        }
    }
}

/// An inline object's place, in paragraph coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObjectRect {
    pub offset: usize,
    pub line: usize,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The gap a tab makes, for drawing its leader.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabGap {
    pub line: usize,
    pub x0: f32,
    pub x1: f32,
    pub baseline: f32,
    /// The font size of the text the gap is in, physical pixels.
    pub size: f32,
    pub leader: Leader,
    pub color: Rgba,
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
    /// The font's line gap.
    leading: f32,
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
    /// Horizontal stretch of the glyphs themselves (their positions are
    /// already stretched).
    pub x_scale: f32,
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
    pub kind: LineKind,
}

/// Per cluster: which face, which glyphs, which style.
#[derive(Clone, Debug)]
struct Look {
    face: Option<usize>,
    glyphs: Range<usize>,
    /// Glyph offsets within the cluster, as Parley gave them.
    style: usize,
    /// Width and height of the inline object this cluster stands for.
    object: Option<[f32; 2]>,
    /// Horizontal stretch.
    xscale: f32,
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
    tabs: Vec<breaker::TabGap>,
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
    decor: Vec<Decor>,
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
    /// The weight class of fonts seen, by blob and index.
    weights: std::collections::HashMap<(u64, u32), Option<u16>>,
}

/// The `usWeightClass` of a font file (an OpenType font or a collection).
fn font_weight(data: &[u8], index: u32) -> Option<u16> {
    let be16 = |o: usize| data.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let be32 = |o: usize| {
        data.get(o..o + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let mut base = 0usize;
    if data.get(0..4) == Some(b"ttcf") {
        base = be32(12 + 4 * index as usize)? as usize;
    }
    let tables = be16(base + 4)? as usize;
    for i in 0..tables {
        let record = base + 12 + 16 * i;
        if data.get(record..record + 4) == Some(b"OS/2") {
            return be16(be32(record + 8)? as usize + 4);
        }
    }
    None
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
            weights: std::collections::HashMap::new(),
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

    /// Whether the machine has a font family of this name.
    pub fn has_family(&mut self, name: &str) -> bool {
        self.font_cx.collection.family_id(name).is_some()
    }

    pub fn lay_out(&mut self, spec: &Spec<'_>) -> Paragraph {
        self.lay_out_with(spec, &Extra::default())
    }

    /// The font metrics `style` has: what an empty line of it is as high as.
    fn probe(&mut self, spec: &Spec<'_>, ratio: Option<f32>) -> Option<(f32, f32, f32)> {
        let probe = Spec {
            text: " ",
            spans: &[],
            width: None,
            ..spec.clone()
        };
        let laid = self.lay_out_with(&probe, &Extra::default());
        let face = laid.faces.first()?;
        Some(scaled_metrics(
            (face.ascent, face.descent, face.leading),
            ratio.map(|r| r * spec.base.size * spec.scale),
        ))
    }

    /// Lay out a paragraph with what a word processor adds to a plain one;
    /// see [`Extra`].
    pub fn lay_out_with(&mut self, spec: &Spec<'_>, extra: &Extra<'_>) -> Paragraph {
        let text = spec.text;
        let scale = spec.scale;
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, scale, extra.snap);
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
        for spacing in extra.spacing {
            if spacing.range.start < spacing.range.end && spacing.range.end <= text.len() {
                builder.push(
                    StyleProperty::LetterSpacing(spacing.amount),
                    spacing.range.clone(),
                );
            }
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
        let base_em = to_fx(spec.base.size * scale);
        let mut faces: Vec<Face> = Vec::new();
        let mut glyphs: Vec<ShapedGlyph> = Vec::new();
        let mut found: Vec<(Cluster, Look)> = Vec::new();
        for line in layout.lines() {
            for run in line.runs() {
                let metrics = run.font_metrics();
                let synthesis = run.synthesis();
                // Bold asked for, and a font that is not: thicken it.
                let style_index = style_at(run.text_range().start);
                let wants_bold = if style_index == 0 {
                    spec.base.bold
                } else {
                    spec.spans[style_index - 1].style.bold
                };
                let east_asian_run = text[run.text_range()].chars().any(|c| c as u32 >= 0x2e80);
                let font = run.font();
                let coords_set = !run.normalized_coords().is_empty();
                let too_light = wants_bold && !coords_set && {
                    let key = (font.data.id(), font.index);
                    let weight = *self
                        .weights
                        .entry(key)
                        .or_insert_with(|| font_weight(font.data.data(), font.index));
                    // The bold of East Asian serif fonts is faint at body sizes:
                    // thicken it as for a font without one.
                    weight.is_some_and(|w| w < 600 || (w < 800 && east_asian_run))
                };
                let face_index = faces.len();
                faces.push(Face {
                    font: run.font().clone(),
                    size: run.font_size(),
                    coords: bytemuck::cast_slice::<_, i16>(run.normalized_coords()).to_vec(),
                    embolden: synthesis.embolden() || too_light,
                    skew: synthesis.skew(),
                    ascent: metrics.ascent,
                    descent: metrics.descent,
                    leading: metrics.leading,
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
                    let object = extra
                        .objects
                        .binary_search_by_key(&range.start, |object| object.offset)
                        .ok()
                        .map(|index| &extra.objects[index]);
                    let start = glyphs.len();
                    if object.is_none() && !first.is_control() {
                        glyphs.extend(cluster.glyphs().map(|g| ShapedGlyph {
                            id: g.id,
                            x: g.x,
                            y: g.y,
                            advance: g.advance,
                        }));
                    }
                    let style = style_at(range.start);
                    let xscale = {
                        let after = extra
                            .scaling
                            .partition_point(|n| n.range.start <= range.start);
                        after
                            .checked_sub(1)
                            .map(|i| &extra.scaling[i])
                            .filter(|n| n.range.contains(&range.start))
                            .map_or(1.0, |n| n.factor.clamp(0.1, 10.0))
                    };
                    let advance = if newline {
                        0
                    } else if let Some(object) = object {
                        to_fx(object.width * scale)
                    } else {
                        to_fx(cluster.advance() * xscale)
                    };
                    found.push((
                        Cluster {
                            class: classify(first),
                            advance,
                            em: to_fx(run.font_size()),
                            break_before: range.start > 0
                                && breaks.binary_search(&range.start).is_ok(),
                            newline,
                            tab: first == '\t',
                            point: first == '.',
                            text: range,
                        },
                        Look {
                            face: Some(face_index),
                            glyphs: start..glyphs.len(),
                            style,
                            object: object.map(|o| [o.width * scale, o.height * scale]),
                            xscale,
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
                    tab: false,
                    point: false,
                });
                looks.push(Look {
                    face: None,
                    glyphs: 0..0,
                    style: style_at(start),
                    object: None,
                    xscale: 1.0,
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
        let mut stops: Vec<Stop> = extra
            .tabs
            .iter()
            .map(|stop| Stop {
                position: to_fx(stop.position * scale),
                align: stop.align,
                leader: stop.leader,
            })
            .collect();
        stops.sort_by_key(|stop| stop.position);
        let shape = Shape {
            first_indent: to_fx(extra.first_line * scale),
            margin_offset: to_fx(extra.margin_offset * scale),
            stops: &stops,
            default_tab: to_fx(extra.default_tab * scale),
        };
        let mut options = spec.rules.options(max_width);
        options.distribute = extra.distribute;
        let broken = breaker::break_lines_shaped(&clusters, options, &shape);

        let mut paragraph = Paragraph {
            text: text.to_owned(),
            clusters,
            looks,
            faces,
            glyphs,
            styles,
            decor: extra.decor.to_vec(),
            lines: Vec::with_capacity(broken.len()),
            height: 0.0,
            width: 0.0,
            scale,
        };
        let snap = extra.snap;
        let round = |value: f32| if snap { value.round() } else { value };
        let rule = extra.line;
        let grid = extra.grid.filter(|pitch| *pitch > 0.0).map(|p| p * scale);
        let ends_in_break = broken.last().is_some_and(|line| {
            line.kind == BreakKind::Hard && line.clusters.end == paragraph.clusters.len()
        });
        let line_factor = spec.line_height;
        let mut top = 0.0;
        for line in broken {
            let (mut ascent, mut descent, mut size) = (0.0f32, 0.0f32, 0.0f32);
            let (mut leading, mut up, mut down, mut object) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for index in line.clusters.clone() {
                let look = &paragraph.looks[index];
                let style = &paragraph.styles[look.style];
                size = size.max(style.size * scale);
                if let Some([_, height]) = look.object {
                    object = object.max(height);
                }
                if let Some(face) = look.face.map(|f| &paragraph.faces[f]) {
                    let at = paragraph.clusters[index].text.start;
                    let after = extra.natural.partition_point(|n| n.range.start <= at);
                    let target = after
                        .checked_sub(1)
                        .map(|i| &extra.natural[i])
                        .filter(|n| n.range.contains(&at))
                        .map(|n| n.ratio * style.size * scale);
                    let (fa, fd, fl) =
                        scaled_metrics((face.ascent, face.descent, face.leading), target);
                    let above_at = extra.above.partition_point(|n| n.range.start <= at);
                    let room = above_at
                        .checked_sub(1)
                        .map(|i| &extra.above[i])
                        .filter(|n| n.range.contains(&at))
                        .map_or(0.0, |n| n.height * scale);
                    ascent = ascent.max(fa);
                    descent = descent.max(fd);
                    leading = leading.max(fl);
                    let rise = style.rise * scale;
                    up = up.max(fa + rise + room);
                    down = down.max(fd - rise);
                }
            }
            if size == 0.0 {
                size = spec.base.size * scale;
            }
            if ascent == 0.0 && descent == 0.0 {
                ascent = size * 0.8;
                descent = size * 0.2;
                (up, down) = (ascent, descent);
            }
            let (height, baseline) = if matches!(rule, LineRule::Legacy) {
                let ascent = ascent.max(object);
                let height = round((size * line_factor).max(ascent + descent));
                let baseline = round((height - (ascent + descent)) / 2.0 + ascent);
                grid_fit(grid, height, baseline)
            } else {
                line_extent(rule, grid, round, up.max(object), down, leading, scale)
            };
            let width = from_fx(line.width);
            paragraph.width = paragraph.width.max(width);
            paragraph.lines.push(LineBox {
                clusters: line.clusters,
                top,
                height,
                baseline: top + baseline,
                left: 0.0,
                width,
                kind: line.kind,
                end: line.end,
                placed: line.placed,
                tabs: line.tabs,
            });
            top += height;
        }
        if paragraph.lines.is_empty() || (extra.trailing_line && ends_in_break) {
            let at = paragraph.clusters.len();
            let size = spec.base.size * scale;
            let probed = if matches!(rule, LineRule::Legacy) {
                None
            } else {
                self.probe(spec, extra.base_natural)
            };
            let (height, baseline) = match probed {
                None => {
                    let height = round(size * line_factor);
                    (height, round(height / 2.0 + size * 0.3))
                }
                Some((ascent, descent, leading)) => {
                    line_extent(rule, grid, round, ascent, descent, leading, scale)
                }
            };
            let line = LineBox {
                clusters: at..at,
                top,
                height,
                baseline: top + baseline,
                left: 0.0,
                width: 0.0,
                kind: BreakKind::End,
                end: EndTreatment::None,
                placed: Vec::new(),
                tabs: Vec::new(),
            };
            top += line.height;
            paragraph.lines.push(line);
        }
        paragraph.height = top;
        let container = spec.width.unwrap_or(paragraph.width);
        let first_indent = extra.first_line * scale;
        for (n, line) in paragraph.lines.iter_mut().enumerate() {
            let indent = if n == 0 { first_indent } else { 0.0 };
            let room = container - indent;
            line.left = indent
                + match spec.align {
                    Align::Start => 0.0,
                    Align::Center => round(((room - line.width) / 2.0).max(0.0)),
                    Align::End => round((room - line.width).max(0.0)),
                };
        }
        paragraph
    }
}

/// Ascent, descent and line gap scaled so that they add up to `target`.
fn scaled_metrics(m: (f32, f32, f32), target: Option<f32>) -> (f32, f32, f32) {
    let sum = m.0 + m.1 + m.2;
    match target {
        Some(t) if sum > 0.0 && t > 0.0 => {
            let f = t / sum;
            (m.0 * f, m.1 * f, m.2 * f)
        }
        _ => m,
    }
}

/// The height of a line and where its baseline is in it, from the largest
/// ascent (with what is raised), descent and line gap on it.
fn line_extent(
    rule: LineRule,
    grid: Option<f32>,
    round: impl Fn(f32) -> f32,
    ascent: f32,
    descent: f32,
    leading: f32,
    scale: f32,
) -> (f32, f32) {
    let natural = ascent + descent + leading;
    if let Some(pitch) = grid
        && pitch > 0.0
        && !matches!(rule, LineRule::Exactly(_))
    {
        // A line takes whole grid lines, a multiple multiplies that, and the
        // text is in the middle.
        let rows = ((natural - 0.02) / pitch).ceil().max(1.0);
        let base = rows * pitch;
        let height = match rule {
            LineRule::Multiple(m) => base * m.max(0.0),
            LineRule::AtLeast(v) => base.max(v * scale),
            _ => base,
        };
        let baseline = (height - natural) / 2.0 + ascent + leading / 2.0;
        return (round(height), round(baseline));
    }
    let (height, baseline) = match rule {
        LineRule::Legacy => (natural, ascent + leading / 2.0),
        LineRule::Multiple(multiple) => {
            let height = natural * multiple.max(0.0);
            (height, height - descent - leading / 2.0)
        }
        LineRule::AtLeast(least) => {
            let height = natural.max(least * scale);
            (height, height - descent - leading / 2.0)
        }
        LineRule::Exactly(exact) => {
            let height = (exact * scale).max(0.0);
            let shrink = if natural > 0.0 {
                (height / natural).min(1.0)
            } else {
                1.0
            };
            (height, height - descent * shrink)
        }
    };
    (round(height), round(baseline))
}

/// Round a line up to whole grid lines, the text in the middle.
fn grid_fit(grid: Option<f32>, height: f32, baseline: f32) -> (f32, f32) {
    match grid {
        Some(pitch) => {
            let rows = (height / pitch - 1e-3).ceil().max(1.0);
            let total = rows * pitch;
            (total, baseline + (total - height) / 2.0)
        }
        None => (height, baseline),
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
        self.runs_of(self.visible_lines(top, bottom))
    }

    /// The glyphs of lines `lines`.
    pub fn draw_runs_in(&self, lines: Range<usize>) -> Vec<DrawRun<'_>> {
        self.runs_of(self.lines_in(lines))
    }

    fn lines_in(&self, lines: Range<usize>) -> impl Iterator<Item = &LineBox> {
        let end = lines.end.min(self.lines.len());
        let start = lines.start.min(end);
        self.lines[start..end].iter()
    }

    fn runs_of<'a>(&'a self, lines: impl Iterator<Item = &'a LineBox>) -> Vec<DrawRun<'a>> {
        let mut runs: Vec<DrawRun<'_>> = Vec::new();
        for line in lines {
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
                    std::ptr::eq(run.face, face)
                        && run.color == color
                        && run.skew == skew
                        && run.x_scale == look.xscale
                });
                if !same {
                    runs.push(DrawRun {
                        face,
                        color,
                        skew,
                        x_scale: look.xscale,
                        glyphs: Vec::new(),
                    });
                }
                let run = runs.last_mut().expect("pushed above");
                let mut pen = line.left + from_fx(placed.glyph_x);
                let baseline = line.baseline - self.styles[look.style].rise * self.scale;
                for glyph in &self.glyphs[look.glyphs.clone()] {
                    run.glyphs.push(Glyph {
                        id: glyph.id,
                        x: pen + glyph.x * look.xscale,
                        y: baseline + glyph.y,
                    });
                    pen += glyph.advance * look.xscale;
                }
            }
        }
        runs
    }

    /// Underlines and strikethroughs on the lines that overlap `top..bottom`.
    /// `min_thickness` keeps hairlines visible.
    pub fn decorations(&self, top: f32, bottom: f32, min_thickness: f32) -> Vec<Decoration> {
        self.decorations_of(self.visible_lines(top, bottom), min_thickness)
    }

    /// Underlines and strikethroughs of lines `lines`.
    pub fn decorations_in(&self, lines: Range<usize>, min_thickness: f32) -> Vec<Decoration> {
        self.decorations_of(self.lines_in(lines), min_thickness)
    }

    fn decor_at(&self, offset: usize) -> Option<&Decor> {
        let after = self
            .decor
            .partition_point(|decor| decor.range.start <= offset);
        let decor = &self.decor[after.checked_sub(1)?];
        decor.range.contains(&offset).then_some(decor)
    }

    fn decorations_of<'a>(
        &'a self,
        lines: impl Iterator<Item = &'a LineBox>,
        min_thickness: f32,
    ) -> Vec<Decoration> {
        let mut out: Vec<Decoration> = Vec::new();
        for line in lines {
            for placed in &line.placed {
                let cluster = &self.clusters[placed.index];
                if cluster.newline || cluster.advance == 0 {
                    continue;
                }
                let look = &self.looks[placed.index];
                if look.object.is_some() {
                    continue;
                }
                let style = &self.styles[look.style];
                let Some(face) = look.face.map(|f| &self.faces[f]) else {
                    continue;
                };
                let x0 = line.left + from_fx(placed.x);
                let x1 = x0 + from_fx(placed.advance);
                let baseline = line.baseline - style.rise * self.scale;
                let decor = self.decor_at(cluster.text.start);
                let color = style.color;
                let mut add = |offset: f32, size: f32, color: Rgba, kind: LineKind| {
                    let decoration = Decoration {
                        x0,
                        x1,
                        y: baseline - offset,
                        thickness: size.max(min_thickness),
                        color,
                        kind,
                    };
                    // Join with the piece just before on the same line.
                    if let Some(last) = out.iter_mut().rev().take(4).find(|last| {
                        last.y == decoration.y
                            && last.color == decoration.color
                            && last.kind == decoration.kind
                            && (last.x1 - decoration.x0).abs() < 1.0
                    }) {
                        last.x1 = decoration.x1;
                    } else {
                        out.push(decoration);
                    }
                };
                if style.underline {
                    add(
                        face.underline_offset,
                        face.underline_size,
                        color,
                        LineKind::Solid,
                    );
                }
                if style.strike {
                    add(face.strike_offset, face.strike_size, color, LineKind::Solid);
                }
                if let Some(decor) = decor {
                    let (offset, size) = (face.underline_offset, face.underline_size);
                    let blank = self.text[cluster.text.clone()]
                        .chars()
                        .all(char::is_whitespace);
                    let underline_color = decor.color.unwrap_or(color);
                    if !(decor.words_only && blank) {
                        let c = underline_color;
                        match decor.underline {
                            Stroke::None => {}
                            Stroke::Single => add(offset, size, c, LineKind::Solid),
                            Stroke::Thick => add(offset, size * 2.0, c, LineKind::Solid),
                            Stroke::Dotted => add(offset, size, c, LineKind::Dotted),
                            Stroke::Dashed => add(offset, size, c, LineKind::Dashed),
                            Stroke::Wave => add(offset - size, size, c, LineKind::Wave),
                            Stroke::Double => {
                                add(offset, size, c, LineKind::Solid);
                                add(offset - size * 2.0, size, c, LineKind::Solid);
                            }
                        }
                    }
                    let (offset, size) = (face.strike_offset, face.strike_size);
                    match decor.strike {
                        Stroke::None => {}
                        Stroke::Double => {
                            add(offset + size, size, color, LineKind::Solid);
                            add(offset - size, size, color, LineKind::Solid);
                        }
                        Stroke::Thick => add(offset, size * 2.0, color, LineKind::Solid),
                        _ => add(offset, size, color, LineKind::Solid),
                    }
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
        self.rects_in(range, 0..self.lines.len())
    }

    /// The same, for the lines `lines` only.
    pub fn rects_in(&self, range: Range<usize>, lines: Range<usize>) -> Vec<[f32; 4]> {
        let mut out = Vec::new();
        if range.is_empty() {
            return out;
        }
        for line in self.lines_in(lines) {
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

    /// Rectangles covering a byte range, one per line, as high as the
    /// text's own fonts (ascent to descent) rather than the line.
    pub fn text_rects_in(&self, range: Range<usize>, lines: Range<usize>) -> Vec<[f32; 4]> {
        let mut out = Vec::new();
        if range.is_empty() {
            return out;
        }
        for line in self.lines_in(lines) {
            let (mut x0, mut x1) = (f32::INFINITY, f32::NEG_INFINITY);
            let (mut y0, mut y1) = (f32::INFINITY, f32::NEG_INFINITY);
            for placed in &line.placed {
                let cluster = &self.clusters[placed.index];
                if cluster.newline
                    || cluster.text.end <= range.start
                    || cluster.text.start >= range.end
                {
                    continue;
                }
                let look = &self.looks[placed.index];
                let left = line.left + from_fx(placed.x);
                x0 = x0.min(left);
                x1 = x1.max(left + from_fx(placed.advance));
                let rise = self.styles[look.style].rise * self.scale;
                let (a, d) = match (look.face, look.object) {
                    (_, Some([_, h])) => (h, 0.0),
                    (Some(f), _) => (self.faces[f].ascent, self.faces[f].descent),
                    _ => (0.0, 0.0),
                };
                y0 = y0.min(line.baseline - rise - a);
                y1 = y1.max(line.baseline - rise + d);
            }
            if x0 < x1 && y0 < y1 {
                out.push([x0, y0, x1, y1]);
            }
        }
        out
    }

    /// Where the inline objects are.
    pub fn objects(&self) -> Vec<ObjectRect> {
        let mut out = Vec::new();
        for (n, line) in self.lines.iter().enumerate() {
            for placed in &line.placed {
                let Some([width, height]) = self.looks[placed.index].object else {
                    continue;
                };
                out.push(ObjectRect {
                    line: n,
                    offset: self.clusters[placed.index].text.start,
                    x: line.left + from_fx(placed.x),
                    y: line.baseline - height,
                    width,
                    height,
                });
            }
        }
        out
    }

    /// The gaps tabs make, for leaders.
    pub fn tab_gaps(&self) -> Vec<TabGap> {
        let mut out = Vec::new();
        for (n, line) in self.lines.iter().enumerate() {
            for gap in &line.tabs {
                let Some(placed) = line.placed.get(gap.index - line.clusters.start) else {
                    continue;
                };
                let style = &self.styles[self.looks[gap.index].style];
                let x0 = line.left + from_fx(placed.x);
                out.push(TabGap {
                    line: n,
                    x0,
                    x1: x0 + from_fx(placed.advance),
                    baseline: line.baseline,
                    size: style.size * self.scale,
                    leader: gap.leader,
                    color: style.color,
                });
            }
        }
        out
    }

    /// The line a position is on: the one its character starts on, so a
    /// position where a line breaks is at the start of the next line; the
    /// last line for the end of the text.
    pub fn line_of(&self, offset: usize) -> usize {
        let index = self.clusters.partition_point(|c| c.text.end <= offset);
        if index >= self.clusters.len() {
            return self.lines.len() - 1;
        }
        self.lines
            .partition_point(|line| line.clusters.end <= index)
            .min(self.lines.len() - 1)
    }

    /// Where a caret at `offset` is: its line and its x.
    pub fn caret(&self, offset: usize) -> (usize, f32) {
        let line_index = self.line_of(offset);
        let line = &self.lines[line_index];
        let index = self.clusters.partition_point(|c| c.text.end <= offset);
        if index < line.clusters.end
            && index >= line.clusters.start
            && let Some(placed) = line.placed.get(index - line.clusters.start)
        {
            return (line_index, line.left + from_fx(placed.x));
        }
        // The end of the text, or of an empty line.
        let end = line
            .placed
            .iter()
            .rev()
            .find(|placed| !self.clusters[placed.index].newline)
            .map_or(0, |placed| placed.x + placed.advance);
        (line_index, line.left + from_fx(end))
    }

    /// The offset a line starts at.
    pub fn line_start_offset(&self, line: usize) -> usize {
        let line = &self.lines[line.min(self.lines.len() - 1)];
        match self.clusters.get(line.clusters.start) {
            Some(cluster) if !line.clusters.is_empty() => cluster.text.start,
            _ => self.text.len(),
        }
    }

    /// The offset a line ends at: before its line break and before the
    /// spaces the line gave up at its end, except on the last line.
    pub fn line_end_offset(&self, line: usize) -> usize {
        let line = &self.lines[line.min(self.lines.len() - 1)];
        if line.clusters.is_empty() {
            return self.text.len();
        }
        match line.kind {
            BreakKind::End => self.text.len(),
            BreakKind::Hard => self.clusters[line.clusters.end - 1].text.start,
            BreakKind::Soft | BreakKind::Emergency => {
                let mut end = line.clusters.end;
                while end > line.clusters.start + 1 && self.clusters[end - 1].class == Class::Space
                {
                    end -= 1;
                }
                self.clusters[end - 1].text.end
            }
        }
    }

    /// The nearest position to a point, for a click: past the end of a
    /// line it is the end of that line, not the start of the next.
    pub fn hit(&self, x: f32, y: f32) -> usize {
        let line = self.line_at(y);
        let index = self
            .lines
            .partition_point(|l| l.top + l.height <= y)
            .min(self.lines.len() - 1);
        if line.clusters.is_empty() {
            return self.line_start_offset(index);
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
        self.line_end_offset(index)
    }

    /// The text of clusters no font has a glyph for. They are drawn with
    /// the fallback font's "missing glyph" (`.notdef`), which in the macOS
    /// system font is a box of horizontal bars. Spaces and line breaks are
    /// left out.
    pub fn undrawable(&self) -> Vec<&str> {
        let mut out = Vec::new();
        for (cluster, look) in self.clusters.iter().zip(&self.looks) {
            let text = &self.text[cluster.text.clone()];
            if cluster.newline || look.object.is_some() || text.chars().all(char::is_whitespace) {
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

#[cfg(test)]
mod extra_tests {
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

    fn spec(text: &str, width: Option<f32>) -> Spec<'_> {
        Spec {
            text,
            base: style(16.0),
            spans: &[],
            language: "en",
            width,
            scale: 1.0,
            line_height: 1.2,
            align: Align::Start,
            rules: Rules::for_language("en"),
        }
    }

    fn fine() -> Extra<'static> {
        Extra {
            snap: false,
            ..Extra::default()
        }
    }

    fn x_of(paragraph: &Paragraph, offset: usize) -> f32 {
        paragraph.caret(offset).1
    }

    #[test]
    fn plain_layout_is_what_it_was() {
        let mut shaper = Shaper::new();
        let text = "Some words to wrap over a few lines of text";
        let a = shaper.lay_out(&spec(text, Some(120.0)));
        let b = shaper.lay_out_with(&spec(text, Some(120.0)), &Extra::default());
        assert_eq!(a.line_ranges(), b.line_ranges());
        assert_eq!(a.height(), b.height());
    }

    #[test]
    fn the_first_line_can_be_indented_or_hang_out() {
        let mut shaper = Shaper::new();
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda";
        let plain = shaper.lay_out_with(&spec(text, Some(200.0)), &fine());
        let indented = shaper.lay_out_with(
            &spec(text, Some(200.0)),
            &Extra {
                first_line: 32.0,
                ..fine()
            },
        );
        assert_eq!(indented.lines()[0].left, 32.0);
        assert_eq!(indented.lines()[1].left, 0.0);
        assert!(indented.lines()[0].width <= 200.0 - 32.0 + 0.01);
        assert!(indented.lines().len() >= plain.lines().len());
        let hanging = shaper.lay_out_with(
            &spec(text, Some(200.0)),
            &Extra {
                first_line: -32.0,
                ..fine()
            },
        );
        assert_eq!(hanging.lines()[0].left, -32.0);
        assert!(hanging.lines()[0].width <= 232.0 + 0.01);
        assert!(hanging.lines()[0].width > plain.lines()[0].width - 0.01);
    }

    #[test]
    fn a_tab_goes_to_the_next_stop() {
        let mut shaper = Shaper::new();
        let text = "ab\tcd\tef";
        let stops = [
            TabStop {
                position: 100.0,
                align: TabAlign::Left,
                leader: Leader::Dots,
            },
            TabStop {
                position: 200.0,
                align: TabAlign::Left,
                leader: Leader::None,
            },
        ];
        let paragraph = shaper.lay_out_with(
            &spec(text, None),
            &Extra {
                tabs: &stops,
                ..fine()
            },
        );
        assert!((x_of(&paragraph, 3) - 100.0).abs() < 0.1);
        assert!((x_of(&paragraph, 6) - 200.0).abs() < 0.1);
        let gaps = paragraph.tab_gaps();
        assert_eq!(gaps.len(), 2);
        assert_eq!(gaps[0].leader, Leader::Dots);
        assert!((gaps[0].x1 - 100.0).abs() < 0.1 && gaps[0].x0 < 40.0);
    }

    #[test]
    fn default_stops_follow_the_last_explicit_one() {
        let mut shaper = Shaper::new();
        let stops = [TabStop {
            position: 50.0,
            align: TabAlign::Left,
            leader: Leader::None,
        }];
        let extra = Extra {
            tabs: &stops,
            default_tab: 36.0,
            ..fine()
        };
        let paragraph = shaper.lay_out_with(&spec("a\tb\tc\td", None), &extra);
        assert!((x_of(&paragraph, 2) - 50.0).abs() < 0.1);
        // Then every 36 from the margin: 72, 108.
        assert!((x_of(&paragraph, 4) - 72.0).abs() < 0.1);
        assert!((x_of(&paragraph, 6) - 108.0).abs() < 0.1);
    }

    #[test]
    fn right_centre_and_decimal_stops_line_the_text_up() {
        let mut shaper = Shaper::new();
        let stop = |align| {
            [TabStop {
                position: 150.0,
                align,
                leader: Leader::None,
            }]
        };
        let text = "x\t12.50";
        let right = shaper.lay_out_with(
            &spec(text, None),
            &Extra {
                tabs: &stop(TabAlign::Right),
                ..fine()
            },
        );
        assert!((x_of(&right, text.len()) - 150.0).abs() < 0.2);
        let centre = shaper.lay_out_with(
            &spec(text, None),
            &Extra {
                tabs: &stop(TabAlign::Center),
                ..fine()
            },
        );
        let (start, end) = (x_of(&centre, 2), x_of(&centre, text.len()));
        assert!(((start + end) / 2.0 - 150.0).abs() < 0.2);
        let decimal = shaper.lay_out_with(
            &spec(text, None),
            &Extra {
                tabs: &stop(TabAlign::Decimal),
                ..fine()
            },
        );
        assert!(
            (x_of(&decimal, 4) - 150.0).abs() < 0.2,
            "{}",
            x_of(&decimal, 4)
        );
    }

    #[test]
    fn stops_are_counted_from_the_margin_not_the_indent() {
        let mut shaper = Shaper::new();
        let stops = [TabStop {
            position: 100.0,
            align: TabAlign::Left,
            leader: Leader::None,
        }];
        // The paragraph is indented 40 from the margin: its x = 0 is 40.
        let paragraph = shaper.lay_out_with(
            &spec("a\tb", None),
            &Extra {
                tabs: &stops,
                margin_offset: 40.0,
                ..fine()
            },
        );
        assert!((x_of(&paragraph, 2) - 60.0).abs() < 0.1);
    }

    #[test]
    fn an_inline_object_sits_on_the_baseline_and_raises_its_line() {
        let mut shaper = Shaper::new();
        let text = "ab\u{fffc}cd";
        let objects = [InlineObject {
            offset: 2,
            width: 40.0,
            height: 60.0,
        }];
        let with = shaper.lay_out_with(
            &spec(text, None),
            &Extra {
                objects: &objects,
                ..fine()
            },
        );
        let without = shaper.lay_out_with(&spec("abcd", None), &fine());
        let rects = with.objects();
        assert_eq!(rects.len(), 1);
        let r = rects[0];
        assert_eq!((r.width, r.height, r.offset), (40.0, 60.0, 2));
        assert!((r.y + r.height - with.lines()[0].baseline).abs() < 0.01);
        assert!(with.lines()[0].height >= 60.0);
        assert!(with.height() > without.height());
        // It takes 40 of width, and is not reported as unshaped text.
        assert!((x_of(&with, 5) - x_of(&without, 2) - 40.0).abs() < 1.0);
        assert!(with.undrawable().is_empty());
    }

    #[test]
    fn line_rules() {
        let mut shaper = Shaper::new();
        let text = "line one\nline two";
        let with = |line| Extra { line, ..fine() };
        let single = shaper.lay_out_with(&spec(text, None), &with(LineRule::Multiple(1.0)));
        let natural = single.lines()[0].height;
        assert!(natural > 16.0 && natural < 30.0, "{natural}");
        let double = shaper.lay_out_with(&spec(text, None), &with(LineRule::Multiple(2.0)));
        assert!((double.lines()[0].height - 2.0 * natural).abs() < 0.01);
        let least = shaper.lay_out_with(&spec(text, None), &with(LineRule::AtLeast(10.0)));
        assert_eq!(least.lines()[0].height, natural);
        let more = shaper.lay_out_with(&spec(text, None), &with(LineRule::AtLeast(40.0)));
        assert_eq!(more.lines()[0].height, 40.0);
        let exact = shaper.lay_out_with(&spec(text, None), &with(LineRule::Exactly(12.0)));
        assert_eq!(exact.lines()[0].height, 12.0);
        assert_eq!(exact.height(), 24.0);
        // The baseline stays inside the line.
        assert!(exact.lines()[0].baseline <= 12.0);
        // An empty paragraph is as high as one line of its font.
        let empty = shaper.lay_out_with(&spec("", Some(100.0)), &with(LineRule::Multiple(1.0)));
        assert!((empty.height() - natural).abs() < 0.01);
    }

    #[test]
    fn a_grid_rounds_lines_up_and_centres_the_text() {
        let mut shaper = Shaper::new();
        let text = "grid line\ngrid line";
        let base = Extra {
            line: LineRule::Multiple(1.0),
            ..fine()
        };
        let free = shaper.lay_out_with(&spec(text, None), &base);
        let gridded = shaper.lay_out_with(
            &spec(text, None),
            &Extra {
                grid: Some(18.0),
                ..base.clone()
            },
        );
        let natural = free.lines()[0].height;
        let rows = (natural / 18.0).ceil();
        assert_eq!(gridded.lines()[0].height, rows * 18.0);
        let shift = gridded.lines()[0].baseline - free.lines()[0].baseline;
        assert!((shift - (rows * 18.0 - natural) / 2.0).abs() < 0.01);
    }

    #[test]
    fn distribute_justifies_the_last_line() {
        let mut shaper = Shaper::new();
        let mut spec = spec("一二三四五", Some(200.0));
        spec.language = "zh-Hans";
        spec.rules = Rules {
            justify: true,
            ..Rules::for_language("zh-Hans")
        };
        let plain = shaper.lay_out_with(&spec, &fine());
        assert!(plain.lines()[0].width < 190.0);
        let distributed = shaper.lay_out_with(
            &spec,
            &Extra {
                distribute: true,
                ..fine()
            },
        );
        assert!((distributed.lines()[0].width - 200.0).abs() < 0.1);
    }

    #[test]
    fn letter_spacing_widens_a_range() {
        let mut shaper = Shaper::new();
        let narrow = shaper.lay_out_with(&spec("abcdef", None), &fine());
        let spacing = [Spacing {
            range: 0..6,
            amount: 3.0,
        }];
        let wide = shaper.lay_out_with(
            &spec("abcdef", None),
            &Extra {
                spacing: &spacing,
                ..fine()
            },
        );
        assert!((wide.width() - narrow.width() - 18.0).abs() < 0.5);
    }

    #[test]
    fn double_underlines_and_coloured_ones() {
        let mut shaper = Shaper::new();
        let decor = [Decor {
            range: 0..4,
            underline: Stroke::Double,
            words_only: false,
            strike: Stroke::Single,
            color: Some([255, 0, 0, 255]),
        }];
        let paragraph = shaper.lay_out_with(
            &spec("text more", None),
            &Extra {
                decor: &decor,
                ..fine()
            },
        );
        let lines = paragraph.decorations(0.0, 100.0, 0.5);
        // Two underlines and a strikethrough, over the first four characters.
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(
            lines.iter().filter(|l| l.color == [255, 0, 0, 255]).count(),
            2
        );
        assert!(lines.iter().all(|l| l.x1 - l.x0 < 60.0));
    }

    #[test]
    fn carets_lines_and_clicks_agree() {
        let mut shaper = Shaper::new();
        let text = "alpha beta gamma delta epsilon zeta";
        let paragraph = shaper.lay_out_with(&spec(text, Some(110.0)), &fine());
        assert!(paragraph.lines().len() >= 3);
        for (offset, _) in text.char_indices() {
            let (line, x) = paragraph.caret(offset);
            let y = paragraph.lines()[line].top + 1.0;
            let back = paragraph.hit(x + 0.2, y);
            assert_eq!(back, offset, "offset {offset}");
        }
        // The end of a wrapped line is before the space it gave up.
        let ranges = paragraph.line_ranges();
        let end0 = paragraph.line_end_offset(0);
        assert!(text[..end0].ends_with(char::is_alphabetic));
        assert!(end0 <= ranges[0].end);
        assert_eq!(paragraph.line_start_offset(1), ranges[1].start);
        // Far to the right of a wrapped line is its end, not the next start.
        assert_eq!(paragraph.hit(1e5, 1.0), end0);
        let last = paragraph.lines().len() - 1;
        assert_eq!(paragraph.line_end_offset(last), text.len());
        // A line break at the end gets a line to put the caret on.
        let broken = shaper.lay_out_with(
            &spec("ab\n", None),
            &Extra {
                trailing_line: true,
                ..fine()
            },
        );
        assert_eq!(broken.lines().len(), 2);
        assert_eq!(broken.caret(3).0, 1);
        assert_eq!(broken.caret(2).0, 0);
    }
}
