//! DrawingML pieces shared by workbooks and presentations: the theme
//! (colour scheme, fonts, fill and line styles) and colours with their
//! modifiers (`lumMod`, `tint`, `alpha` ...).
//!
//! A colour is read as a [`ColorSpec`] and resolved later, because a scheme
//! colour means different things in different places: `bg1` goes through
//! the slide master's colour map, and `phClr` is whatever colour the shape's
//! style reference brings.

use std::collections::HashMap;

use crate::error::Result;
use crate::xml::{Tag, XmlReader, each_child};

pub type Rgba = [u8; 4];

/// The 12 colours of a theme, by their scheme names.
pub const SCHEME_NAMES: [&str; 12] = [
    "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6",
    "hlink", "folHlink",
];

/// Office's own theme, for files without one.
const OFFICE_COLORS: [u32; 12] = [
    0x000000, 0xFFFFFF, 0x44546A, 0xE7E6E6, 0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5,
    0x70AD47, 0x0563C1, 0x954F72,
];

fn rgb(value: u32) -> Rgba {
    [(value >> 16) as u8, (value >> 8) as u8, value as u8, 0xff]
}

#[derive(Clone, Debug, PartialEq)]
pub enum ColorBase {
    Rgb(Rgba),
    /// `accent1`, `tx1`, `bg2`, `phClr` ...
    Scheme(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Modifier {
    LumMod(f64),
    LumOff(f64),
    SatMod(f64),
    SatOff(f64),
    Tint(f64),
    Shade(f64),
    Alpha(f64),
    AlphaMod(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColorSpec {
    pub base: ColorBase,
    pub modifiers: Vec<Modifier>,
}

impl ColorSpec {
    pub fn rgb(color: Rgba) -> Self {
        Self {
            base: ColorBase::Rgb(color),
            modifiers: Vec::new(),
        }
    }

    pub fn scheme(name: &str) -> Self {
        Self {
            base: ColorBase::Scheme(name.to_owned()),
            modifiers: Vec::new(),
        }
    }
}

/// A fill as the file states it.
#[derive(Clone, Debug, PartialEq)]
pub enum FillSpec {
    None,
    Solid(ColorSpec),
    /// Stops (position 0 to 1, colour) and the angle in degrees, clockwise
    /// from left to right.
    Gradient(Vec<(f64, ColorSpec)>, f64),
    /// A picture: the relationship id of the image.
    Picture(String),
    /// Pattern: foreground and background.
    Pattern(Option<ColorSpec>, Option<ColorSpec>),
    /// Use the group's fill.
    Group,
}

/// An outline as the file states it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct LineSpec {
    /// EMU.
    pub width: Option<i64>,
    pub fill: Option<FillSpec>,
    pub dash: Option<String>,
}

/// Fonts of a theme: `+mj-lt` and `+mn-lt` and their East Asian partners.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ThemeFonts {
    pub latin: Option<String>,
    pub east_asian: Option<String>,
    /// Script (`Jpan`, `Hans`, `Hant`, `Hang`) to typeface.
    pub scripts: Vec<(String, String)>,
}

impl ThemeFonts {
    /// The East Asian face for a language tag, falling back to the theme's
    /// own East Asian face.
    pub fn east_asian_for(&self, language: &str) -> Option<&str> {
        let script = match language {
            l if l.starts_with("ja") => "Jpan",
            l if l.starts_with("ko") => "Hang",
            l if l.contains("Hant") || l.ends_with("TW") || l.ends_with("HK") => "Hant",
            l if l.starts_with("zh") => "Hans",
            _ => "",
        };
        self.scripts
            .iter()
            .find(|(s, face)| s == script && !face.is_empty())
            .map(|(_, face)| face.as_str())
            .or(self.east_asian.as_deref().filter(|face| !face.is_empty()))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub colors: HashMap<String, Rgba>,
    pub major: ThemeFonts,
    pub minor: ThemeFonts,
    /// `fillStyleLst`, `lnStyleLst` and `bgFillStyleLst`, in order; style
    /// references index them from 1 (and background fills from 1001).
    pub fills: Vec<FillSpec>,
    pub lines: Vec<LineSpec>,
    pub background_fills: Vec<FillSpec>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            colors: SCHEME_NAMES
                .iter()
                .zip(OFFICE_COLORS)
                .map(|(name, value)| ((*name).to_owned(), rgb(value)))
                .collect(),
            major: ThemeFonts {
                latin: Some("Calibri Light".to_owned()),
                ..ThemeFonts::default()
            },
            minor: ThemeFonts {
                latin: Some("Calibri".to_owned()),
                ..ThemeFonts::default()
            },
            fills: Vec::new(),
            lines: Vec::new(),
            background_fills: Vec::new(),
        }
    }
}

impl Theme {
    /// A colour of the scheme by its name; unknown names are black.
    pub fn color(&self, name: &str) -> Rgba {
        self.colors.get(name).copied().unwrap_or([0, 0, 0, 0xff])
    }

    /// The colour SpreadsheetML means by `theme="n"`: the scheme in file
    /// order, except that the first two pairs are swapped (0 is `lt1`).
    pub fn indexed(&self, index: usize) -> Option<Rgba> {
        let name = match index {
            0 => "lt1",
            1 => "dk1",
            2 => "lt2",
            3 => "dk2",
            i => SCHEME_NAMES.get(i)?,
        };
        Some(self.color(name))
    }

    /// A font reference such as `+mn-lt` or `+mj-ea` to a typeface name;
    /// other names come back as they are. `None` for an empty reference.
    pub fn typeface(&self, name: &str, language: &str) -> Option<String> {
        let fonts = match name.get(..4) {
            Some("+mj-") => &self.major,
            Some("+mn-") => &self.minor,
            _ => return (!name.is_empty()).then(|| name.to_owned()),
        };
        match &name[4..] {
            "ea" => fonts.east_asian_for(language).map(str::to_owned),
            _ => fonts.latin.clone().filter(|face| !face.is_empty()),
        }
    }
}

/// How a slide master names scheme colours: `bg1` to `lt1` and so on.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorMap(pub HashMap<String, String>);

impl Default for ColorMap {
    fn default() -> Self {
        Self(
            [
                ("bg1", "lt1"),
                ("tx1", "dk1"),
                ("bg2", "lt2"),
                ("tx2", "dk2"),
            ]
            .into_iter()
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .collect(),
        )
    }
}

impl ColorMap {
    /// From the attributes of `p:clrMap` or `p:overrideClrMapping`.
    pub fn from_tag(tag: &Tag<'_>) -> Self {
        let mut map = Self::default();
        for name in [
            "bg1", "tx1", "bg2", "tx2", "accent1", "accent2", "accent3", "accent4", "accent5",
            "accent6", "hlink", "folHlink",
        ] {
            if let Some(value) = tag.attr(name) {
                map.0.insert(name.to_owned(), value);
            }
        }
        map
    }

    fn get<'a>(&'a self, name: &'a str) -> &'a str {
        self.0.get(name).map_or(name, String::as_str)
    }
}

/// What a colour needs to become pixels.
#[derive(Clone, Copy, Debug)]
pub struct ColorContext<'a> {
    pub theme: &'a Theme,
    pub map: &'a ColorMap,
    /// The colour `phClr` stands for.
    pub placeholder: Option<Rgba>,
}

impl ColorContext<'_> {
    pub fn resolve(&self, spec: &ColorSpec) -> Rgba {
        let base = match &spec.base {
            ColorBase::Rgb(color) => *color,
            ColorBase::Scheme(name) if name == "phClr" => {
                self.placeholder.unwrap_or([0, 0, 0, 0xff])
            }
            ColorBase::Scheme(name) => {
                let mapped = self.map.get(name);
                // A map pointing at itself or at a name the theme lacks
                // falls back to the plain scheme name.
                match self.theme.colors.get(mapped) {
                    Some(color) => *color,
                    None => self.theme.color(match name.as_str() {
                        "bg1" => "lt1",
                        "tx1" => "dk1",
                        "bg2" => "lt2",
                        "tx2" => "dk2",
                        other => other,
                    }),
                }
            }
        };
        apply(base, &spec.modifiers)
    }
}

/// Apply colour modifiers in file order.
pub fn apply(color: Rgba, modifiers: &[Modifier]) -> Rgba {
    let mut rgb = [color[0], color[1], color[2]].map(|c| f64::from(c) / 255.0);
    let mut alpha = f64::from(color[3]) / 255.0;
    for modifier in modifiers {
        match *modifier {
            Modifier::LumMod(_)
            | Modifier::LumOff(_)
            | Modifier::SatMod(_)
            | Modifier::SatOff(_) => {
                let (h, mut l, mut s) = hls(rgb);
                match *modifier {
                    Modifier::LumMod(v) => l *= v,
                    Modifier::LumOff(v) => l += v,
                    Modifier::SatMod(v) => s *= v,
                    Modifier::SatOff(v) => s += v,
                    _ => {}
                }
                rgb = from_hls(h, l.clamp(0.0, 1.0), s.clamp(0.0, 1.0));
            }
            Modifier::Tint(v) => rgb = rgb.map(|c| 1.0 - (1.0 - c) * v.clamp(0.0, 1.0)),
            Modifier::Shade(v) => rgb = rgb.map(|c| c * v.clamp(0.0, 1.0)),
            Modifier::Alpha(v) => alpha = v.clamp(0.0, 1.0),
            Modifier::AlphaMod(v) => alpha = (alpha * v).clamp(0.0, 1.0),
        }
    }
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [byte(rgb[0]), byte(rgb[1]), byte(rgb[2]), byte(alpha)]
}

fn hls([r, g, b]: [f64; 3]) -> (f64, f64, f64) {
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

fn from_hls(h: f64, l: f64, s: f64) -> [f64; 3] {
    if s == 0.0 {
        return [l, l, l];
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
    [channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0)]
}

/// `RRGGBB` or `AARRGGBB` (SpreadsheetML) to a colour.
pub fn parse_hex(text: &str) -> Option<Rgba> {
    let text = text.trim();
    let value = u32::from_str_radix(text, 16).ok()?;
    match text.len() {
        6 => Some(rgb(value)),
        // Excel writes alpha as FF or 00 and means opaque either way.
        8 => Some(rgb(value & 0xFF_FFFF)),
        _ => None,
    }
}

fn preset(name: &str) -> Option<Rgba> {
    Some(rgb(match name {
        "black" => 0x000000,
        "white" => 0xFFFFFF,
        "red" => 0xFF0000,
        "green" => 0x008000,
        "blue" => 0x0000FF,
        "yellow" => 0xFFFF00,
        "gray" | "grey" => 0x808080,
        "darkGray" | "dkGray" => 0xA9A9A9,
        "lightGray" | "ltGray" => 0xD3D3D3,
        "orange" => 0xFFA500,
        "purple" => 0x800080,
        "navy" => 0x000080,
        "silver" => 0xC0C0C0,
        "cyan" | "aqua" => 0x00FFFF,
        "magenta" | "fuchsia" => 0xFF00FF,
        _ => return None,
    }))
}

/// Whether `tag` is one of the colour elements.
pub fn is_color(tag: &Tag<'_>) -> bool {
    matches!(
        tag.local(),
        b"srgbClr" | b"schemeClr" | b"sysClr" | b"prstClr" | b"scrgbClr" | b"hslClr"
    )
}

fn percent(tag: &Tag<'_>) -> f64 {
    tag.attr_f64("val").unwrap_or(0.0) / 100_000.0
}

/// After the `Open` of a colour element: the colour and its modifiers.
pub fn read_color(reader: &mut XmlReader<'_>, tag: &Tag<'_>) -> Result<Option<ColorSpec>> {
    let base = match tag.local() {
        b"srgbClr" => tag
            .attr("val")
            .as_deref()
            .and_then(parse_hex)
            .map(ColorBase::Rgb),
        b"schemeClr" => tag.attr("val").map(ColorBase::Scheme),
        b"sysClr" => tag
            .attr("lastClr")
            .as_deref()
            .and_then(parse_hex)
            .or_else(|| match tag.attr("val").as_deref() {
                Some("window") => Some(rgb(0xFFFFFF)),
                _ => Some(rgb(0x000000)),
            })
            .map(ColorBase::Rgb),
        b"prstClr" => tag
            .attr("val")
            .as_deref()
            .and_then(preset)
            .map(ColorBase::Rgb),
        b"scrgbClr" => {
            let channel = |name: &str| {
                let linear = (tag.attr_f64(name).unwrap_or(0.0) / 100_000.0).clamp(0.0, 1.0);
                let srgb = if linear <= 0.003_130_8 {
                    linear * 12.92
                } else {
                    1.055 * linear.powf(1.0 / 2.4) - 0.055
                };
                (srgb * 255.0).round() as u8
            };
            Some(ColorBase::Rgb([
                channel("r"),
                channel("g"),
                channel("b"),
                0xff,
            ]))
        }
        b"hslClr" => {
            let h = tag.attr_f64("hue").unwrap_or(0.0) / 21_600_000.0;
            let s = tag.attr_f64("sat").unwrap_or(0.0) / 100_000.0;
            let l = tag.attr_f64("lum").unwrap_or(0.0) / 100_000.0;
            let [r, g, b] = from_hls(h, l.clamp(0.0, 1.0), s.clamp(0.0, 1.0));
            let byte = |v: f64| (v * 255.0).round() as u8;
            Some(ColorBase::Rgb([byte(r), byte(g), byte(b), 0xff]))
        }
        _ => None,
    };
    let mut modifiers = Vec::new();
    each_child(reader, |reader, child| {
        let value = percent(&child);
        let modifier = match child.local() {
            b"lumMod" => Some(Modifier::LumMod(value)),
            b"lumOff" => Some(Modifier::LumOff(value)),
            b"satMod" => Some(Modifier::SatMod(value)),
            b"satOff" => Some(Modifier::SatOff(value)),
            b"tint" => Some(Modifier::Tint(value)),
            b"shade" => Some(Modifier::Shade(value)),
            b"alpha" => Some(Modifier::Alpha(value)),
            b"alphaMod" => Some(Modifier::AlphaMod(value)),
            _ => None,
        };
        modifiers.extend(modifier);
        reader.skip()
    })?;
    Ok(base.map(|base| ColorSpec { base, modifiers }))
}

/// After the `Open` of an element holding one colour (`solidFill`,
/// `fgClr`, `a:dk1`, `fontRef` ...): that colour.
pub fn read_color_in(reader: &mut XmlReader<'_>) -> Result<Option<ColorSpec>> {
    let mut found = None;
    each_child(reader, |reader, child| {
        if is_color(&child) && found.is_none() {
            found = read_color(reader, &child)?;
            Ok(())
        } else {
            reader.skip()
        }
    })?;
    Ok(found)
}

/// Whether `tag` is one of the fill elements.
pub fn is_fill(tag: &Tag<'_>) -> bool {
    matches!(
        tag.local(),
        b"noFill" | b"solidFill" | b"gradFill" | b"blipFill" | b"pattFill" | b"grpFill"
    )
}

/// After the `Open` of a fill element.
pub fn read_fill(reader: &mut XmlReader<'_>, tag: &Tag<'_>) -> Result<Option<FillSpec>> {
    Ok(match tag.local() {
        b"noFill" => {
            reader.skip()?;
            Some(FillSpec::None)
        }
        b"grpFill" => {
            reader.skip()?;
            Some(FillSpec::Group)
        }
        b"solidFill" => read_color_in(reader)?.map(FillSpec::Solid),
        b"gradFill" => {
            let mut stops = Vec::new();
            let mut angle = 90.0;
            each_child(reader, |reader, child| {
                if child.is("gsLst") {
                    each_child(reader, |reader, stop| {
                        if stop.is("gs") {
                            let position = stop.attr_f64("pos").unwrap_or(0.0) / 100_000.0;
                            if let Some(color) = read_color_in(reader)? {
                                stops.push((position.clamp(0.0, 1.0), color));
                            }
                            Ok(())
                        } else {
                            reader.skip()
                        }
                    })
                } else if child.is("lin") {
                    angle = child.attr_f64("ang").unwrap_or(0.0) / 60_000.0;
                    reader.skip()
                } else {
                    reader.skip()
                }
            })?;
            stops.sort_by(|a, b| a.0.total_cmp(&b.0));
            (!stops.is_empty()).then_some(FillSpec::Gradient(stops, angle))
        }
        b"blipFill" => {
            let mut embed = None;
            each_child(reader, |reader, child| {
                if child.is("blip") {
                    embed = child.attr("embed");
                }
                reader.skip()
            })?;
            embed.map(FillSpec::Picture)
        }
        b"pattFill" => {
            let (mut fg, mut bg) = (None, None);
            each_child(reader, |reader, child| {
                if child.is("fgClr") {
                    fg = read_color_in(reader)?;
                    Ok(())
                } else if child.is("bgClr") {
                    bg = read_color_in(reader)?;
                    Ok(())
                } else {
                    reader.skip()
                }
            })?;
            Some(FillSpec::Pattern(fg, bg))
        }
        _ => {
            reader.skip()?;
            None
        }
    })
}

/// After the `Open` of `a:ln`.
pub fn read_line(reader: &mut XmlReader<'_>, tag: &Tag<'_>) -> Result<LineSpec> {
    let mut line = LineSpec {
        width: tag.attr_i64("w"),
        ..LineSpec::default()
    };
    each_child(reader, |reader, child| {
        if is_fill(&child) {
            if let Some(fill) = read_fill(reader, &child)? {
                line.fill = Some(fill);
            }
            Ok(())
        } else if child.is("prstDash") {
            line.dash = child.attr("val");
            reader.skip()
        } else {
            reader.skip()
        }
    })?;
    Ok(line)
}

/// A theme part (`theme1.xml`).
pub fn read_theme(bytes: &[u8], part: &str) -> Result<Theme> {
    let mut theme = Theme::default();
    let mut reader = XmlReader::new(bytes, part);
    loop {
        match reader.next()? {
            crate::xml::Ev::Open(tag) if tag.is("clrScheme") => {
                each_child(&mut reader, |reader, slot| {
                    let name = String::from_utf8_lossy(slot.local()).into_owned();
                    if let Some(spec) = read_color_in(reader)? {
                        let empty = Theme::default();
                        let map = ColorMap::default();
                        let context = ColorContext {
                            theme: &empty,
                            map: &map,
                            placeholder: None,
                        };
                        theme.colors.insert(name, context.resolve(&spec));
                    }
                    Ok(())
                })?;
            }
            crate::xml::Ev::Open(tag) if tag.is("majorFont") || tag.is("minorFont") => {
                let mut fonts = ThemeFonts::default();
                each_child(&mut reader, |reader, child| {
                    match child.local() {
                        b"latin" => fonts.latin = child.attr("typeface"),
                        b"ea" => fonts.east_asian = child.attr("typeface"),
                        b"font" => {
                            if let (Some(script), Some(face)) =
                                (child.attr("script"), child.attr("typeface"))
                            {
                                fonts.scripts.push((script, face));
                            }
                        }
                        _ => {}
                    }
                    reader.skip()
                })?;
                if tag.is("majorFont") {
                    theme.major = fonts;
                } else {
                    theme.minor = fonts;
                }
            }
            crate::xml::Ev::Open(tag) if tag.is("fillStyleLst") || tag.is("bgFillStyleLst") => {
                let mut fills = Vec::new();
                each_child(&mut reader, |reader, child| {
                    if is_fill(&child) {
                        fills.push(read_fill(reader, &child)?.unwrap_or(FillSpec::None));
                        Ok(())
                    } else {
                        reader.skip()
                    }
                })?;
                if tag.is("fillStyleLst") {
                    theme.fills = fills;
                } else {
                    theme.background_fills = fills;
                }
            }
            crate::xml::Ev::Open(tag) if tag.is("lnStyleLst") => {
                each_child(&mut reader, |reader, child| {
                    if child.is("ln") {
                        theme.lines.push(read_line(reader, &child)?);
                        Ok(())
                    } else {
                        reader.skip()
                    }
                })?;
            }
            // Extra colour lists and object defaults hold colours too; only
            // the scheme counts.
            crate::xml::Ev::Open(tag)
                if tag.is("extraClrSchemeLst") || tag.is("objectDefaults") || tag.is("extLst") =>
            {
                reader.skip()?
            }
            crate::xml::Ev::Eof => return Ok(theme),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME: &str = r#"<?xml version="1.0"?>
<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="T">
 <a:themeElements>
  <a:clrScheme name="C">
   <a:dk1><a:sysClr val="windowText" lastClr="111111"/></a:dk1>
   <a:lt1><a:sysClr val="window" lastClr="FEFEFE"/></a:lt1>
   <a:dk2><a:srgbClr val="1F2937"/></a:dk2>
   <a:lt2><a:srgbClr val="E5E7EB"/></a:lt2>
   <a:accent1><a:srgbClr val="7C5CE0"/></a:accent1>
   <a:accent2><a:srgbClr val="ED7D31"/></a:accent2>
  </a:clrScheme>
  <a:fontScheme name="F">
   <a:majorFont><a:latin typeface="Aptos Display"/><a:ea typeface=""/><a:cs typeface=""/>
    <a:font script="Jpan" typeface="游ゴシック Light"/><a:font script="Hans" typeface="等线 Light"/></a:majorFont>
   <a:minorFont><a:latin typeface="Aptos"/><a:ea typeface="Meiryo"/><a:cs typeface=""/></a:minorFont>
  </a:fontScheme>
  <a:fmtScheme name="S">
   <a:fillStyleLst>
    <a:solidFill><a:schemeClr val="phClr"/></a:solidFill>
    <a:gradFill><a:gsLst><a:gs pos="100000"><a:schemeClr val="phClr"><a:shade val="50000"/></a:schemeClr></a:gs>
      <a:gs pos="0"><a:schemeClr val="phClr"><a:tint val="50000"/></a:schemeClr></a:gs></a:gsLst><a:lin ang="5400000"/></a:gradFill>
    <a:noFill/>
   </a:fillStyleLst>
   <a:lnStyleLst><a:ln w="12700"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln></a:lnStyleLst>
   <a:effectStyleLst/>
   <a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst>
  </a:fmtScheme>
 </a:themeElements>
 <a:objectDefaults><a:spDef><a:spPr/><a:style><a:fillRef idx="1"><a:srgbClr val="FF0000"/></a:fillRef></a:style></a:spDef></a:objectDefaults>
</a:theme>"#;

    #[test]
    fn reads_a_theme() {
        let theme = read_theme(THEME.as_bytes(), "theme1.xml").unwrap();
        assert_eq!(theme.color("dk1"), [0x11, 0x11, 0x11, 0xff]);
        assert_eq!(theme.color("lt1"), [0xFE, 0xFE, 0xFE, 0xff]);
        assert_eq!(theme.color("accent1"), [0x7C, 0x5C, 0xE0, 0xff]);
        // Colours the file leaves out keep Office's.
        assert_eq!(theme.color("accent6"), rgb(0x70AD47));
        assert_eq!(theme.indexed(0), Some(theme.color("lt1")));
        assert_eq!(theme.indexed(1), Some(theme.color("dk1")));
        assert_eq!(theme.indexed(4), Some(theme.color("accent1")));
        assert_eq!(theme.indexed(12), None);
        assert_eq!(
            theme.typeface("+mj-lt", "en").as_deref(),
            Some("Aptos Display")
        );
        assert_eq!(
            theme.typeface("+mj-ea", "ja").as_deref(),
            Some("游ゴシック Light")
        );
        assert_eq!(
            theme.typeface("+mj-ea", "zh-Hans").as_deref(),
            Some("等线 Light")
        );
        assert_eq!(theme.typeface("+mj-ea", "en"), None);
        assert_eq!(theme.typeface("+mn-ea", "ko").as_deref(), Some("Meiryo"));
        assert_eq!(theme.typeface("Arial", "en").as_deref(), Some("Arial"));
        assert_eq!(theme.fills.len(), 3);
        assert_eq!(theme.lines[0].width, Some(12_700));
        assert_eq!(theme.background_fills.len(), 1);
        let FillSpec::Gradient(stops, angle) = &theme.fills[1] else {
            panic!("{:?}", theme.fills[1]);
        };
        assert_eq!(stops[0].0, 0.0);
        assert_eq!(*angle, 90.0);
    }

    #[test]
    fn resolves_scheme_colours_through_the_map() {
        let theme = read_theme(THEME.as_bytes(), "theme1.xml").unwrap();
        let map = ColorMap::default();
        let context = ColorContext {
            theme: &theme,
            map: &map,
            placeholder: Some(rgb(0x00FF00)),
        };
        assert_eq!(
            context.resolve(&ColorSpec::scheme("tx1")),
            theme.color("dk1")
        );
        assert_eq!(
            context.resolve(&ColorSpec::scheme("bg1")),
            theme.color("lt1")
        );
        assert_eq!(context.resolve(&ColorSpec::scheme("phClr")), rgb(0x00FF00));
        let mut swapped = ColorMap::default();
        swapped.0.insert("bg1".into(), "dk1".into());
        let context = ColorContext {
            map: &swapped,
            ..context
        };
        assert_eq!(
            context.resolve(&ColorSpec::scheme("bg1")),
            theme.color("dk1")
        );
    }

    #[test]
    fn modifiers_follow_powerpoint() {
        // "Blue, Accent 1, Lighter 40%": lumMod 60%, lumOff 40% of #4472C4.
        let lighter = apply(
            rgb(0x4472C4),
            &[Modifier::LumMod(0.6), Modifier::LumOff(0.4)],
        );
        assert_eq!(lighter, [0x8F, 0xAA, 0xDC, 0xff]);
        // "Darker 50%": lumMod 50%.
        assert_eq!(
            apply(rgb(0x4472C4), &[Modifier::LumMod(0.5)]),
            [0x20, 0x38, 0x64, 0xff]
        );
        assert_eq!(apply(rgb(0x000000), &[Modifier::Tint(0.5)])[0], 0x80);
        assert_eq!(apply(rgb(0xFFFFFF), &[Modifier::Shade(0.5)])[0], 0x80);
        assert_eq!(apply(rgb(0xFFFFFF), &[Modifier::Alpha(0.25)])[3], 64);
    }

    #[test]
    fn hex_colours() {
        assert_eq!(parse_hex("FF7C5CE0"), Some([0x7C, 0x5C, 0xE0, 0xff]));
        assert_eq!(parse_hex("7c5ce0"), Some([0x7C, 0x5C, 0xE0, 0xff]));
        assert_eq!(parse_hex("00FF0000"), Some([0xFF, 0, 0, 0xff]));
        assert_eq!(parse_hex("red"), None);
        assert_eq!(parse_hex(""), None);
    }
}
