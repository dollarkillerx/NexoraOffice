//! The Slides document model.
//!
//! A [`Presentation`] is a slide size and a list of [`Slide`]s; a slide is a
//! list of [`Shape`]s in drawing order, already placed: what a slide inherits
//! from its layout and master (placeholder positions, text sizes, bullets,
//! shapes drawn on every slide) has been resolved by the reader. Positions
//! and sizes are in EMU (English Metric Units, 914,400 per inch, 12,700 per
//! point), the unit of the file format; text sizes are in points.
//!
//! This version is read-only. It models what is needed to show a slide as
//! it looks: the background, shapes with their fills and outlines, text
//! with its colours and fonts, pictures and tables, and the speaker notes.
//! Colours are final: theme colours and their modifiers have been worked
//! out by the reader. Charts, diagrams and other graphics are recorded only
//! as "something is here". Shape geometry is meant to move to
//! `aster-drawing` once that crate exists (Phase 4 of the plan).
//!
//! Nothing here reads files; `aster-ooxml` turns PPTX packages into this
//! model. Nothing here knows about the GUI toolkit either.

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
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            size: DEFAULT_SIZE,
            slides: Vec::new(),
        }
    }
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
        }
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
}

impl Paragraph {
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
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
