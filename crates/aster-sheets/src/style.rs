//! How a cell looks: font, fill, borders, alignment.
//!
//! Colours are final: theme colours, tints and the indexed palette are
//! worked out by the reader, so a style here is what is drawn. `None` for a
//! colour means "automatic": the window text colour for fonts and lines.

use std::sync::Arc;

use crate::format::Format;

/// Red, green, blue, alpha.
pub type Rgba = [u8; 4];

/// Horizontal alignment set on a cell. `None` in a style means "by value":
/// numbers to the right, text to the left, the rest centred.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

/// Vertical alignment. Excel's default is the bottom of the cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VAlign {
    Top,
    Center,
    #[default]
    Bottom,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    /// The family the file asks for, such as `Calibri` or `等线`.
    pub name: Option<Arc<str>>,
    /// Points.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub color: Option<Rgba>,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            name: None,
            size: 11.0,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            color: None,
        }
    }
}

/// The kinds of border line Excel offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineStyle {
    Hair,
    Thin,
    Medium,
    Thick,
    Double,
    Dotted,
    Dashed,
    DashDot,
    DashDotDot,
    MediumDashed,
    MediumDashDot,
    MediumDashDotDot,
    SlantDashDot,
}

impl LineStyle {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "hair" => Self::Hair,
            "thin" => Self::Thin,
            "medium" => Self::Medium,
            "thick" => Self::Thick,
            "double" => Self::Double,
            "dotted" => Self::Dotted,
            "dashed" => Self::Dashed,
            "dashDot" => Self::DashDot,
            "dashDotDot" => Self::DashDotDot,
            "mediumDashed" => Self::MediumDashed,
            "mediumDashDot" => Self::MediumDashDot,
            "mediumDashDotDot" => Self::MediumDashDotDot,
            "slantDashDot" => Self::SlantDashDot,
            _ => return None,
        })
    }

    /// Thickness in logical pixels; a double line is two thin lines with a
    /// gap, three in all.
    pub fn width(self) -> f64 {
        match self {
            Self::Hair | Self::Thin | Self::Dotted | Self::Dashed | Self::DashDot => 1.0,
            Self::DashDotDot => 1.0,
            Self::Medium
            | Self::MediumDashed
            | Self::MediumDashDot
            | Self::MediumDashDotDot
            | Self::SlantDashDot => 2.0,
            Self::Thick | Self::Double => 3.0,
        }
    }

    /// Dash pattern in logical pixels, on and off; empty for a solid line.
    pub fn dashes(self) -> &'static [f64] {
        match self {
            Self::Thin | Self::Medium | Self::Thick | Self::Double => &[],
            Self::Hair => &[1.0, 1.0],
            Self::Dotted => &[1.0, 2.0],
            Self::Dashed => &[3.0, 1.0],
            Self::MediumDashed => &[9.0, 3.0],
            Self::DashDot => &[9.0, 3.0, 3.0, 3.0],
            Self::MediumDashDot | Self::SlantDashDot => &[9.0, 3.0, 3.0, 3.0],
            Self::DashDotDot | Self::MediumDashDotDot => &[9.0, 3.0, 3.0, 3.0, 3.0, 3.0],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    pub style: LineStyle,
    pub color: Option<Rgba>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Borders {
    pub left: Option<Line>,
    pub right: Option<Line>,
    pub top: Option<Line>,
    pub bottom: Option<Line>,
}

impl Borders {
    pub fn is_empty(&self) -> bool {
        self.left.is_none() && self.right.is_none() && self.top.is_none() && self.bottom.is_none()
    }
}

/// A cell format: number format, alignment, font, fill and borders.
#[derive(Clone, Debug, PartialEq)]
pub struct CellStyle {
    pub format: Arc<Format>,
    pub align: Option<HAlign>,
    pub valign: VAlign,
    /// Lines wrap at the cell's width.
    pub wrap: bool,
    /// Indent levels from the aligned edge, three spaces each.
    pub indent: u8,
    pub font: Arc<Font>,
    /// Solid background colour, or the main colour of a pattern or gradient.
    pub fill: Option<Rgba>,
    pub borders: Arc<Borders>,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            format: Arc::new(Format::general()),
            align: None,
            valign: VAlign::Bottom,
            wrap: false,
            indent: 0,
            font: Arc::new(Font::default()),
            fill: None,
            borders: Arc::new(Borders::default()),
        }
    }
}

impl CellStyle {
    /// Whether an empty cell with this style shows anything: a fill or a
    /// border.
    pub fn is_visible_when_empty(&self) -> bool {
        self.fill.is_some() || !self.borders.is_empty()
    }
}

/// Excel's tint: lighten (positive) or darken (negative) a colour by moving
/// its lightness in HLS towards white or black.
pub fn tint(color: Rgba, tint: f64) -> Rgba {
    if tint == 0.0 || !tint.is_finite() {
        return color;
    }
    let tint = tint.clamp(-1.0, 1.0);
    let (h, l, s) = rgb_to_hls(color);
    let l = if tint < 0.0 {
        l * (1.0 + tint)
    } else {
        l * (1.0 - tint) + tint
    };
    let [r, g, b] = hls_to_rgb(h, l.clamp(0.0, 1.0), s);
    [r, g, b, color[3]]
}

/// Hue in turns (0 to 1), lightness and saturation from 0 to 1.
pub fn rgb_to_hls(color: Rgba) -> (f64, f64, f64) {
    let [r, g, b] = [color[0], color[1], color[2]].map(|c| f64::from(c) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, l, 0.0);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, l, s)
}

pub fn hls_to_rgb(h: f64, l: f64, s: f64) -> [u8; 3] {
    let to_byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if s == 0.0 {
        let v = to_byte(l);
        return [v, v, v];
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let channel = |t: f64| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [
        to_byte(channel(h + 1.0 / 3.0)),
        to_byte(channel(h)),
        to_byte(channel(h - 1.0 / 3.0)),
    ]
}

/// Excel's 64 indexed colours, used by old files and by `indexed="n"`.
/// 64 is the window text colour and 65 the window background.
pub const INDEXED_COLORS: [u32; 64] = [
    0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x000000,
    0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x800000, 0x008000,
    0x000080, 0x808000, 0x800080, 0x008080, 0xC0C0C0, 0x808080, 0x9999FF, 0x993366, 0xFFFFCC,
    0xCCFFFF, 0x660066, 0xFF8080, 0x0066CC, 0xCCCCFF, 0x000080, 0xFF00FF, 0xFFFF00, 0x00FFFF,
    0x800080, 0x800000, 0x008080, 0x0000FF, 0x00CCFF, 0xCCFFFF, 0xCCFFCC, 0xFFFF99, 0x99CCFF,
    0xFF99CC, 0xCC99FF, 0xFFCC99, 0x3366FF, 0x33CCCC, 0x99CC00, 0xFFCC00, 0xFF9900, 0xFF6600,
    0x666699, 0x969696, 0x003366, 0x339966, 0x003300, 0x333300, 0x993300, 0x993366, 0x333399,
    0x333333,
];

pub fn rgb(value: u32) -> Rgba {
    [(value >> 16) as u8, (value >> 8) as u8, value as u8, 0xff]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tints_move_lightness_like_excel() {
        // Excel's "Blue, Accent 1, Lighter 80%" from accent 1 #4472C4.
        assert_eq!(
            tint(rgb(0x4472C4), 0.7999816888943144),
            [0xDA, 0xE3, 0xF3, 0xff]
        );
        // "Darker 25%".
        assert_eq!(
            tint(rgb(0x4472C4), -0.249977111117893),
            [0x2F, 0x55, 0x97, 0xff]
        );
        // White darker 15%, a common header fill.
        assert_eq!(
            tint(rgb(0xFFFFFF), -0.1499984740745262),
            [0xD9, 0xD9, 0xD9, 0xff]
        );
        assert_eq!(tint(rgb(0x123456), 0.0), rgb(0x123456));
        assert_eq!(tint(rgb(0x000000), 1.0), rgb(0xFFFFFF));
        assert_eq!(tint(rgb(0x808080), f64::NAN), rgb(0x808080));
    }

    #[test]
    fn hls_round_trips() {
        for value in [0x4472C4, 0xED7D31, 0x70AD47, 0x000000, 0xFFFFFF, 0x7C5CE0] {
            let (h, l, s) = rgb_to_hls(rgb(value));
            let [r, g, b] = hls_to_rgb(h, l, s);
            assert_eq!([r, g, b, 0xff], rgb(value), "{value:06x}");
        }
    }

    #[test]
    fn line_styles_parse_and_have_weights() {
        assert_eq!(
            LineStyle::parse("mediumDashDot"),
            Some(LineStyle::MediumDashDot)
        );
        assert_eq!(LineStyle::parse("none"), None);
        assert_eq!(LineStyle::Thick.width(), 3.0);
        assert!(LineStyle::Thin.dashes().is_empty());
        assert!(!LineStyle::Dotted.dashes().is_empty());
        let mut style = CellStyle::default();
        assert!(!style.is_visible_when_empty());
        style.borders = Arc::new(Borders {
            bottom: Some(Line {
                style: LineStyle::Thin,
                color: None,
            }),
            ..Borders::default()
        });
        assert!(style.is_visible_when_empty());
    }
}
