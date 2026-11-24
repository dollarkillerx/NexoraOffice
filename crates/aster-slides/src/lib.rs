//! The Slides document model.
//!
//! A [`Presentation`] is a slide size and a list of [`Slide`]s; a slide is a
//! list of [`Shape`]s in drawing order, already placed: what a slide inherits
//! from its layout and master (placeholder positions, text sizes, bullets,
//! shapes drawn on every slide) has been resolved by the reader. Positions
//! and sizes are in EMU (English Metric Units, 914,400 per inch, 12,700 per
//! point), the unit of the file format; text sizes are in points.
//!
//! It models what is needed to show a slide as it looks: the background,
//! shapes with their fills and outlines, text with its colours and fonts,
//! pictures and tables, and the speaker notes. Colours are final: theme
//! colours and their modifiers have been worked out by the reader. Charts,
//! diagrams and other graphics are recorded only as "something is here".
//! Shape geometry is meant to move to `aster-drawing` once that crate exists
//! (Phase 4 of the plan).
//!
//! Slides, shapes, paragraphs and runs read from a file carry where they came
//! from ([`SlideSource`], [`ShapeSource`], [`ParaSource`], [`RunSource`]):
//! the XML as written and the model as read. Saving compares the model with
//! what was read and writes back as written whatever did not change. Text
//! bodies also carry what each outline level inherits ([`LevelLook`]), so a
//! paragraph moved to another level looks as that level does. [`edit`] has
//! the text edits.
//!
//! Nothing here reads files; `aster-ooxml` turns PPTX packages into this
//! model. Nothing here knows about the GUI toolkit either.

pub mod edit;

use std::sync::Arc;

/// EMU per point.
pub const EMU_PER_POINT: f64 = 12_700.0;
/// EMU per inch.
pub const EMU_PER_INCH: f64 = 914_400.0;
/// The size of a new 16:9 PowerPoint slide: 13.333 by 7.5 inches.
pub const DEFAULT_SIZE: (i64, i64) = (12_192_000, 6_858_000);
/// Text size when nothing says otherwise.
pub const DEFAULT_TEXT_POINTS: f32 = 18.0;

/// Red, green, blue, alpha.
pub type Rgba = [u8; 4];

/// How an area is painted.
#[derive(Clone, Debug, PartialEq)]
pub enum Fill {
    Solid(Rgba),
    /// Stops (position 0 to 1, colour) and the direction in degrees,
    /// clockwise from left to right.
    Gradient {
        stops: Vec<(f32, Rgba)>,
        angle: f64,
    },
    /// A picture stretched over the area.
    Picture(Picture),
}

impl Fill {
    /// The colour that best stands for the fill: a gradient's first stop.
    pub fn main_color(&self) -> Option<Rgba> {
        match self {
            Fill::Solid(color) => Some(*color),
            Fill::Gradient { stops, .. } => stops.first().map(|(_, color)| *color),
            Fill::Picture(_) => None,
        }
    }
}

/// A picture: the file's own bytes (PNG, JPEG or another format), decoded
/// when drawn, and how much is cropped off each side.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub image: std::sync::Arc<[u8]>,
    /// Shares of the picture cut off the left, top, right and bottom.
    pub crop: [f64; 4],
}

/// A line around a shape or along a table cell's edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outline {
    pub color: Rgba,
    /// EMU.
    pub width: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Presentation {
    /// Slide width and height in EMU.
    pub size: (i64, i64),
    pub slides: Vec<Slide>,
    /// The layouts a new slide can take, in the order the masters list them.
    pub layouts: Vec<Layout>,
    /// The East Asian language the file's text is marked with (`ja-JP`,
    /// `zh-CN` ...), for text typed here.
    pub east_asian_language: Option<String>,
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            size: DEFAULT_SIZE,
            slides: Vec::new(),
            layouts: Vec::new(),
            east_asian_language: None,
        }
    }
}

/// A slide layout of the presentation.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// As the file names it: `Title Slide`, `Title and Content`.
    pub name: String,
    /// Its part, `ppt/slideLayouts/slideLayout1.xml`.
    pub part: String,
    /// A new slide on this layout: its placeholders empty, the layout's and
    /// master's shapes behind them.
    pub slide: Slide,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Slide {
    /// In drawing order: layout and master shapes first.
    pub shapes: Vec<Shape>,
    /// Skipped in a slide show.
    pub hidden: bool,
    /// Its own, its layout's or its master's; `None` for white.
    pub background: Option<Fill>,
    /// The speaker notes, one paragraph per line.
    pub notes: Option<String>,
    /// Where it was read from; `None` for a slide added since.
    pub source: Option<Arc<SlideSource>>,
    /// Index into [`Presentation::layouts`].
    pub layout: Option<usize>,
    /// How shapes and text boxes inserted on this slide look.
    pub look: Arc<SlideLook>,
}

/// A slide as read from its part.
#[derive(Clone, Debug, PartialEq)]
pub struct SlideSource {
    /// The part, `ppt/slides/slide3.xml`.
    pub part: String,
    /// The [`ShapeSource::index`] of each of its own shapes, in order.
    pub shapes: Vec<u32>,
    /// The speaker notes as read.
    pub notes: Option<String>,
    /// A copy of that slide, to be written as a new part.
    pub copy: bool,
}

/// The theme colours and text styles a slide gives new shapes.
#[derive(Clone, Debug, PartialEq)]
pub struct SlideLook {
    /// A new shape's fill (`accent1`), outline (`accent1` darker) and text
    /// (`lt1`).
    pub accent: Rgba,
    pub accent_line: Rgba,
    pub on_accent: Rgba,
    /// What text in a new text box inherits, by level.
    pub text: Arc<Levels>,
}

impl Default for SlideLook {
    fn default() -> Self {
        Self {
            accent: [0x44, 0x72, 0xC4, 0xFF],
            accent_line: [0x2F, 0x52, 0x8F, 0xFF],
            on_accent: [0xFF, 0xFF, 0xFF, 0xFF],
            text: Arc::new(default_levels()),
        }
    }
}

/// What a paragraph at each outline level inherits.
pub type Levels = [LevelLook; 9];

/// The look a paragraph at one outline level has when it sets nothing
/// itself: from the shape's list style, the placeholders it inherits from,
/// the master's text styles and the presentation's defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct LevelLook {
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub color: Option<Rgba>,
    pub font: Option<String>,
    pub east_asian_font: Option<String>,
    pub align: Align,
    pub list: Option<List>,
    /// EMU.
    pub margin: i64,
    pub indent: i64,
}

impl Default for LevelLook {
    fn default() -> Self {
        Self {
            size: DEFAULT_TEXT_POINTS,
            bold: false,
            italic: false,
            color: None,
            font: None,
            east_asian_font: None,
            align: Align::Left,
            list: None,
            margin: 0,
            indent: 0,
        }
    }
}

/// Nine levels with nothing set.
pub fn default_levels() -> Levels {
    std::array::from_fn(|_| LevelLook::default())
}

/// A bullet or numbering scheme.
#[derive(Clone, Debug, PartialEq)]
pub enum List {
    /// A character, such as `•`.
    Char(String),
    /// Automatic numbers: the scheme as the file names it (`arabicPeriod`,
    /// `romanUcPeriod` ...) and the first number.
    Number(String, u32),
}

impl List {
    /// The mark of the `n`th paragraph (from 0) of a run of this list.
    pub fn mark(&self, n: u32) -> String {
        match self {
            List::Char(c) => c.clone(),
            List::Number(scheme, start) => number_mark(scheme, start.saturating_add(n)),
        }
    }
}

/// An automatic number in a PowerPoint numbering scheme.
pub fn number_mark(scheme: &str, n: u32) -> String {
    let (digits, rest) = if let Some(rest) = scheme.strip_prefix("arabic") {
        (n.to_string(), rest)
    } else if let Some(rest) = scheme.strip_prefix("alphaLc") {
        (alpha(n).to_lowercase(), rest)
    } else if let Some(rest) = scheme.strip_prefix("alphaUc") {
        (alpha(n), rest)
    } else if let Some(rest) = scheme.strip_prefix("romanLc") {
        (roman(n).to_lowercase(), rest)
    } else if let Some(rest) = scheme.strip_prefix("romanUc") {
        (roman(n), rest)
    } else if scheme.starts_with("circleNum") {
        let circled = (1..=20)
            .contains(&n)
            .then(|| char::from_u32(0x2460 + n - 1))
            .flatten();
        return circled.map_or_else(|| n.to_string(), String::from);
    } else {
        (n.to_string(), "Period")
    };
    match rest {
        "ParenR" => format!("{digits})"),
        "ParenBoth" => format!("({digits})"),
        "Plain" => digits,
        "Minus" => format!("- {digits} -"),
        _ => format!("{digits}."),
    }
}

fn alpha(mut n: u32) -> String {
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push(b'A' + (n % 26) as u8);
        n /= 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn roman(mut n: u32) -> String {
    const TABLE: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value, letters) in TABLE {
        while n >= value {
            out.push_str(letters);
            n -= value;
        }
    }
    out
}

impl Slide {
    /// The text of the title placeholder, or the first text on the slide.
    pub fn title(&self) -> Option<String> {
        let title = self
            .shapes
            .iter()
            .filter(|shape| !shape.inherited)
            .find(|shape| shape.role == Role::Title)
            .or_else(|| {
                self.shapes
                    .iter()
                    .filter(|shape| !shape.inherited)
                    .find(|shape| shape.text.as_ref().is_some_and(|t| !t.is_blank()))
            })?;
        let text = title.text.as_ref()?.plain_text();
        let line = text.lines().find(|line| !line.trim().is_empty())?.trim();
        (!line.is_empty()).then(|| line.to_owned())
    }

    /// All text on the slide, one paragraph per line.
    pub fn plain_text(&self) -> String {
        self.shapes
            .iter()
            .filter_map(|shape| shape.text.as_ref())
            .map(TextBody::plain_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A rectangle in EMU.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Frame {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

impl Frame {
    pub const fn new(x: i64, y: i64, width: i64, height: i64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// What a shape is, for drawing its outline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Geometry {
    Rect,
    RoundRect,
    Ellipse,
    /// From the top-left to the bottom-right corner of the frame, before
    /// flipping.
    Line,
    /// Any other preset (`triangle`, `rightArrow`, ...) or a custom path; its
    /// outline is drawn as the frame.
    Other(String),
}

/// Why the shape is there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Title,
    /// Body text, subtitles, other placeholders.
    Body,
    /// Date, footer and slide number placeholders.
    Footer,
    /// A text box or an ordinary shape.
    Shape,
    Picture,
    Table,
    Chart,
    /// Diagrams, embedded objects, media.
    Graphic,
}

/// A placeholder's type (`title`, `body`, `subTitle` ...) and index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placeholder {
    pub kind: String,
    pub idx: Option<u32>,
}

/// A shape as read from its slide's part.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeSource {
    /// Which shape of the part, counting `sp`, `cxnSp`, `pic` and
    /// `graphicFrame` elements in document order, through groups and the
    /// first choice of `AlternateContent`.
    pub index: u32,
    /// A copy of that shape: written as a new element with a new id.
    pub copy: bool,
    /// The frame as read.
    pub frame: Frame,
    /// For a shape in a group: how the group's child coordinates map to
    /// the slide's, `slide = offset + child × scale`, as x scale, y scale,
    /// x offset, y offset.
    pub group: Option<[f64; 4]>,
    /// The paragraphs of its text as read, in order.
    pub paragraphs: Vec<Arc<ParaSource>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub frame: Frame,
    /// Clockwise, degrees.
    pub rotation: f64,
    pub flip_h: bool,
    pub flip_v: bool,
    pub geometry: Geometry,
    pub role: Role,
    /// What is inside its outline.
    pub fill: Option<Fill>,
    /// Its outline, if drawn: text boxes and placeholders have none unless
    /// the file gives them one.
    pub line: Option<Outline>,
    pub text: Option<TextBody>,
    /// For pictures.
    pub picture: Option<Picture>,
    /// For tables.
    pub table: Option<Table>,
    /// Comes from the layout or the master, not from the slide itself.
    pub inherited: bool,
    /// For a placeholder: which one.
    pub placeholder: Option<Placeholder>,
    /// Where it was read from; `None` for a shape added since.
    pub source: Option<ShapeSource>,
}

impl Shape {
    pub fn new(frame: Frame, geometry: Geometry, role: Role) -> Self {
        Self {
            frame,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            geometry,
            role,
            fill: None,
            line: None,
            text: None,
            picture: None,
            table: None,
            inherited: false,
            placeholder: None,
            source: None,
        }
    }

    /// Whether text can be typed into it: a text box, a placeholder or an
    /// autoshape. Pictures, tables, charts and lines take none.
    pub fn takes_text(&self) -> bool {
        matches!(
            self.role,
            Role::Title | Role::Body | Role::Footer | Role::Shape
        ) && self.geometry != Geometry::Line
            && !self.inherited
    }
}

/// A table: column widths, and rows of cells. Every row has one cell per
/// column; cells covered by a merged neighbour are marked `merged`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Table {
    /// EMU.
    pub columns: Vec<i64>,
    pub rows: Vec<TableRow>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct TableRow {
    /// EMU; the least height, rows grow to fit their text.
    pub height: i64,
    pub cells: Vec<TableCell>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableCell {
    pub text: Option<TextBody>,
    pub fill: Option<Fill>,
    /// Left, top, right, bottom.
    pub borders: [Option<Outline>; 4],
    /// Columns and rows this cell spans, at least 1.
    pub span: (u32, u32),
    /// Covered by a cell to its left or above that spans over it.
    pub merged: bool,
}

impl Default for TableCell {
    fn default() -> Self {
        Self {
            text: None,
            fill: None,
            borders: [None; 4],
            span: (1, 1),
            merged: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Anchor {
    #[default]
    Top,
    Middle,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextBody {
    pub paragraphs: Vec<Paragraph>,
    /// Left, top, right, bottom, EMU.
    pub insets: [i64; 4],
    pub anchor: Anchor,
    /// Lines wrap at the frame's width.
    pub wrap: bool,
    /// "Shrink text on overflow": the file's own scale for the text, 1.0 for
    /// none.
    pub font_scale: f32,
    /// "Resize shape to fit text": the shape grows and shrinks with it.
    pub grow: bool,
    /// What its paragraphs inherit at each level; `None` where nothing
    /// was worked out (table cells).
    pub levels: Option<Arc<Levels>>,
}

impl Default for TextBody {
    fn default() -> Self {
        Self {
            paragraphs: Vec::new(),
            // PowerPoint's defaults: 0.1 inch left and right, 0.05 top and
            // bottom.
            insets: [91_440, 45_720, 91_440, 45_720],
            anchor: Anchor::Top,
            wrap: true,
            font_scale: 1.0,
            grow: false,
            levels: None,
        }
    }
}

impl TextBody {
    pub fn plain_text(&self) -> String {
        self.paragraphs
            .iter()
            .map(Paragraph::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn is_blank(&self) -> bool {
        self.paragraphs
            .iter()
            .all(|p| p.runs.iter().all(|r| r.text.trim().is_empty()))
    }

    /// Nothing typed at all: an empty placeholder shows its prompt.
    pub fn is_empty(&self) -> bool {
        self.paragraphs
            .iter()
            .all(|p| p.runs.iter().all(|r| r.text.is_empty()))
    }

    /// The level looks, or nine with nothing set.
    pub fn level_looks(&self) -> Arc<Levels> {
        self.levels
            .clone()
            .unwrap_or_else(|| Arc::new(default_levels()))
    }

    /// Work out each paragraph's bullet from its list: numbers count up
    /// through paragraphs of the same level and restart under a new parent
    /// or after a paragraph of that level without numbers. Paragraphs
    /// without text show no bullet.
    pub fn renumber(&mut self) {
        let mut numbers = [0u32; 9];
        for paragraph in &mut self.paragraphs {
            let lvl = usize::from(paragraph.level.min(8));
            let has_text = paragraph.runs.iter().any(|r| !r.text.trim().is_empty());
            paragraph.bullet = match &paragraph.list {
                _ if !has_text => None,
                Some(list @ List::Number(..)) => {
                    let mark = list.mark(numbers[lvl]);
                    numbers[lvl] += 1;
                    Some(mark)
                }
                Some(list) => {
                    numbers[lvl] = 0;
                    Some(list.mark(0))
                }
                None => {
                    numbers[lvl] = 0;
                    None
                }
            };
            for deeper in numbers.iter_mut().skip(lvl + 1) {
                *deeper = 0;
            }
        }
    }
}

/// A paragraph as read: its XML, its properties and end mark as written,
/// what the paragraph looked like, and which of its looks it set itself.
#[derive(Clone, Debug, PartialEq)]
pub struct ParaSource {
    /// The `<a:p>` element.
    pub xml: String,
    /// Its `<a:pPr>` and `<a:endParaRPr>` elements, empty when missing.
    pub props: String,
    pub end: String,
    /// As read, without sources.
    pub original: Paragraph,
    /// It sets its own alignment, list, margin and indent.
    pub own_align: bool,
    pub own_list: bool,
    pub own_margin: bool,
    pub own_indent: bool,
}

/// A run as read.
#[derive(Clone, Debug, PartialEq)]
pub struct RunSource {
    /// The `<a:r>`, `<a:fld>` or `<a:br>` element; empty for a run made
    /// from a paragraph's end mark.
    pub xml: String,
    /// Its properties element (`<a:rPr>`, or the `<a:endParaRPr>` it was
    /// made from), empty when missing.
    pub props: String,
    /// As read, without a source.
    pub original: Run,
    /// It sets its own size, bold, italic and colour.
    pub own_size: bool,
    pub own_bold: bool,
    pub own_italic: bool,
    pub own_color: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Paragraph {
    pub runs: Vec<Run>,
    pub align: Align,
    /// Outline level, 0 to 8.
    pub level: u8,
    /// The bullet character, if any.
    pub bullet: Option<String>,
    /// Left margin of the text and indent of the first line (negative for a
    /// hanging bullet), EMU.
    pub margin: i64,
    pub indent: i64,
    /// Size of an empty paragraph's line, points.
    pub end_size: f32,
    /// Its bullet or numbering scheme; `bullet` is worked out from it.
    pub list: Option<List>,
    /// How text typed at the end of an empty paragraph looks (no text).
    pub end: Run,
    /// Where it was read from; `None` for a paragraph made here.
    pub source: Option<Arc<ParaSource>>,
}

impl Paragraph {
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }

    /// Its length in bytes.
    pub fn len(&self) -> usize {
        self.runs.iter().map(|run| run.text.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|run| run.text.is_empty())
    }

    /// The same text and looks, whatever the sources.
    pub fn same_as(&self, other: &Paragraph) -> bool {
        self.align == other.align
            && self.level == other.level
            && self.list == other.list
            && self.margin == other.margin
            && self.indent == other.indent
            && self.runs.len() == other.runs.len()
            && self
                .runs
                .iter()
                .zip(&other.runs)
                .all(|(a, b)| a.text == b.text && a.same_look(b))
    }

    /// Put the paragraph at outline level `level`, taking that level's
    /// looks where neither it nor its runs set their own.
    pub fn set_level(&mut self, level: u8, levels: &Levels) {
        let level = level.min(8);
        let look = &levels[usize::from(level)];
        let own = self.source.clone();
        let own = own.as_deref();
        self.level = level;
        if !own.is_some_and(|s| s.own_align) {
            self.align = look.align;
        }
        if !own.is_some_and(|s| s.own_list) {
            self.list = look.list.clone();
        }
        if !own.is_some_and(|s| s.own_margin) {
            self.margin = look.margin;
        }
        if !own.is_some_and(|s| s.own_indent) {
            self.indent = look.indent;
        }
        for run in self.runs.iter_mut().chain(std::iter::once(&mut self.end)) {
            let source = run.source.clone();
            let own = source.as_deref();
            if !own.is_some_and(|s| s.own_size) {
                run.size = look.size;
            }
            if !own.is_some_and(|s| s.own_bold) {
                run.bold = look.bold;
            }
            if !own.is_some_and(|s| s.own_italic) {
                run.italic = look.italic;
            }
            if !own.is_some_and(|s| s.own_color) {
                run.color = look.color;
            }
        }
        self.end_size = self.end.size;
    }

    /// An empty paragraph at `level` that looks as that level does.
    pub fn new_at(level: u8, levels: &Levels) -> Self {
        let look = &levels[usize::from(level.min(8))];
        let end = Run {
            size: look.size,
            bold: look.bold,
            italic: look.italic,
            color: look.color,
            font: look.font.clone(),
            east_asian_font: look.east_asian_font.clone(),
            ..Run::default()
        };
        Paragraph {
            runs: Vec::new(),
            align: look.align,
            level: level.min(8),
            bullet: None,
            margin: look.margin,
            indent: look.indent,
            end_size: look.size,
            list: look.list.clone(),
            end,
            source: None,
        }
    }

    /// Size of the largest run, or of the paragraph end when it is empty.
    pub fn size(&self) -> f32 {
        self.runs
            .iter()
            .map(|run| run.size)
            .fold(None, |max: Option<f32>, size| {
                Some(max.map_or(size, |m| m.max(size)))
            })
            .unwrap_or(self.end_size)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    /// A line break inside the paragraph is `\n`.
    pub text: String,
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// `None` for the default, dark text.
    pub color: Option<Rgba>,
    /// Typefaces for Latin and for East Asian text, as the file names them.
    pub font: Option<String>,
    pub east_asian_font: Option<String>,
    /// Where it was read from; `None` for text typed here in a paragraph
    /// made here.
    pub source: Option<Arc<RunSource>>,
}

impl Default for Run {
    fn default() -> Self {
        Self::new(String::new(), DEFAULT_TEXT_POINTS)
    }
}

impl Run {
    pub fn new(text: impl Into<String>, size: f32) -> Self {
        Self {
            text: text.into(),
            size,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            color: None,
            font: None,
            east_asian_font: None,
            source: None,
        }
    }

    /// The same size, emphasis, colour and fonts.
    pub fn same_look(&self, other: &Run) -> bool {
        self.size == other.size
            && self.bold == other.bold
            && self.italic == other.italic
            && self.underline == other.underline
            && self.strike == other.strike
            && self.color == other.color
            && self.font == other.font
            && self.east_asian_font == other.east_asian_font
    }

    /// The same look and the same source: two such runs side by side can
    /// be one.
    pub fn joins(&self, other: &Run) -> bool {
        self.same_look(other)
            && match (&self.source, &other.source) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a.props == b.props && a.xml.is_empty(),
                _ => false,
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_shape(role: Role, lines: &[&str]) -> Shape {
        let mut shape = Shape::new(Frame::default(), Geometry::Rect, role);
        shape.text = Some(TextBody {
            paragraphs: lines
                .iter()
                .map(|line| Paragraph {
                    runs: vec![Run::new(*line, 18.0)],
                    ..Paragraph::default()
                })
                .collect(),
            ..TextBody::default()
        });
        shape
    }

    #[test]
    fn a_slide_is_named_by_its_title_or_first_text() {
        let mut slide = Slide::default();
        assert_eq!(slide.title(), None);
        let mut footer = text_shape(Role::Footer, &["机密"]);
        footer.inherited = true;
        slide.shapes.push(footer);
        assert_eq!(slide.title(), None);
        slide
            .shapes
            .push(text_shape(Role::Body, &["", "本文の最初の行"]));
        assert_eq!(slide.title().as_deref(), Some("本文の最初の行"));
        slide
            .shapes
            .push(text_shape(Role::Title, &["  第三季度报告  ", "副标题"]));
        assert_eq!(slide.title().as_deref(), Some("第三季度报告"));
        assert_eq!(
            slide.plain_text(),
            "机密\n\n本文の最初の行\n  第三季度报告  \n副标题"
        );
    }

    #[test]
    fn paragraph_size_is_its_largest_run() {
        let mut paragraph = Paragraph {
            end_size: 12.0,
            ..Paragraph::default()
        };
        assert_eq!(paragraph.size(), 12.0);
        paragraph.runs.push(Run::new("a", 20.0));
        paragraph.runs.push(Run::new("b", 32.0));
        assert_eq!(paragraph.size(), 32.0);
    }
}
