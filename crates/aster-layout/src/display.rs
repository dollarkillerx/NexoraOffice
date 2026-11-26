//! What a laid-out page is made of: a display list in paint order.
//!
//! Units are points (1/72 inch) with the origin at the page's top-left
//! corner and y growing downwards.

use std::ops::Range;
use std::sync::Arc;

use aster_text::{Leader, Paragraph};

pub type Rgba = [u8; 4];

/// A rectangle, `x0 <= x1` and `y0 <= y1`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }

    pub(crate) fn offset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x0 + dx, self.y0 + dy, self.x1 + dx, self.y1 + dy)
    }
}

/// How a rule is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleStyle {
    Single,
    /// Two thin lines with a gap of their own width between.
    Double,
    Dotted,
    Dashed,
}

/// A paragraph's lines, shared with the layout's cache.
#[derive(Clone, Debug)]
pub struct TextItem {
    pub paragraph: Arc<Paragraph>,
    /// Where the paragraph's own origin (its top-left) is on the page; the
    /// lines of this item may start further down.
    pub x: f32,
    pub y: f32,
    /// Which lines of the paragraph are drawn here.
    pub lines: Range<usize>,
}

/// The dots, hyphens or line that fill a tab.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LeaderItem {
    pub x0: f32,
    pub x1: f32,
    pub baseline: f32,
    /// The size of the text it is in.
    pub size: f32,
    pub kind: Leader,
    pub color: Rgba,
}

/// A picture to draw stretched over `rect`; `crop` is the share cut off the
/// left, top, right and bottom of it.
#[derive(Clone, Debug)]
pub struct PictureItem {
    pub image: Arc<[u8]>,
    pub rect: Rect,
    pub crop: [f64; 4],
}

/// A text box's background and outline. Its content follows as ordinary
/// items between a [`Item::PushClip`] and a [`Item::PopClip`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameItem {
    pub rect: Rect,
    pub fill: Option<Rgba>,
    pub line: Option<(f32, Rgba)>,
}

#[derive(Clone, Debug)]
pub enum Item {
    /// Shading, highlights and cell backgrounds.
    Fill {
        rect: Rect,
        color: Rgba,
    },
    /// Borders, column separators, the footnote separator.
    Rule {
        from: (f32, f32),
        to: (f32, f32),
        width: f32,
        color: Rgba,
        style: RuleStyle,
    },
    Text(TextItem),
    Leader(LeaderItem),
    Picture(PictureItem),
    Frame(FrameItem),
    /// A drawing this version does not draw (`chart`, `shape`, ...).
    Placeholder {
        rect: Rect,
        label: String,
    },
    /// Everything up to the matching `PopClip` is cut to the rectangle.
    PushClip(Rect),
    PopClip,
}

#[derive(Clone, Debug)]
pub struct Page {
    pub width: f32,
    pub height: f32,
    /// The number the page shows (after restarts).
    pub number: u32,
    /// That number as written in its section's format.
    pub label: String,
    /// Which section it belongs to (an index into `Document::sections()`).
    pub section: usize,
    pub items: Vec<Item>,
}
