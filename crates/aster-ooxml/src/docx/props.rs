//! Run and paragraph properties as a file layers them.
//!
//! Each layer (document defaults, a style, a table style, direct
//! formatting) says only some things. [`RunProps`] and [`ParaPartial`] hold
//! what one layer says; `merge` puts a higher layer over a lower one; the
//! `finalize` functions turn the merged layers into the final
//! [`RunLook`] and [`ParaProps`] of the model.
//!
//! Toggle properties (bold, italic and the like) are set by the last layer
//! that mentions them. Word instead XORs a toggle that a character style
//! and the paragraph style both set; files rarely depend on it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use aster_writer::{
    Align, Border, BorderStyle, LineSpacing, ListRef, ParaProps, Rgba, RunLook, TabAlign,
    TabLeader, TabStop, Twips, Underline, UnderlineStyle, VertAlign,
};

use super::dom::Node;
use crate::drawingml::{Theme, parse_hex};

/// What colours and fonts are looked up in, and the one place font and
/// style names are interned.
pub struct Env {
    pub theme: Theme,
    /// The language East Asian theme fonts are chosen for (`zh-CN`).
    pub ea_lang: String,
    interned: RefCell<HashMap<String, Arc<str>>>,
}

impl Env {
    pub fn new(theme: Theme, ea_lang: String) -> Self {
        Self {
            theme,
            ea_lang,
            interned: RefCell::new(HashMap::new()),
        }
    }

    pub fn intern(&self, name: &str) -> Arc<str> {
        let mut map = self.interned.borrow_mut();
        if let Some(found) = map.get(name) {
            return found.clone();
        }
        let arc: Arc<str> = Arc::from(name);
        map.insert(name.to_owned(), arc.clone());
        arc
    }

    /// `majorEastAsia` and the like: the theme's face, or for an empty one
    /// the face of the script of the theme font language.
    fn theme_font(&self, name: &str) -> Option<Arc<str>> {
        let major = name.starts_with("major");
        let fonts = if major {
            &self.theme.major
        } else {
            &self.theme.minor
        };
        let face = if name.ends_with("EastAsia") {
            fonts
                .east_asian
                .clone()
                .filter(|face| !face.is_empty())
                .or_else(|| fonts.east_asian_for(&self.ea_lang).map(str::to_owned))
        } else if name.ends_with("Ascii") || name.ends_with("HAnsi") {
            fonts.latin.clone().filter(|face| !face.is_empty())
        } else {
            None
        };
        face.map(|face| self.intern(&face))
    }
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunProps {
    pub style: Option<Arc<str>>,
    pub font: Option<Arc<str>>,
    pub east_asian_font: Option<Arc<str>>,
    pub size: Option<f32>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<Option<Underline>>,
    pub strike: Option<bool>,
    pub double_strike: Option<bool>,
    pub color: Option<Option<Rgba>>,
    pub highlight: Option<Option<Rgba>>,
    pub shading: Option<Option<Rgba>>,
    pub position: Option<VertAlign>,
    pub caps: Option<bool>,
    pub small_caps: Option<bool>,
    pub hidden: Option<bool>,
    pub spacing: Option<Twips>,
    pub scale: Option<f32>,
    pub raise: Option<f32>,
    pub lang: Option<String>,
    pub lang_ea: Option<String>,
}

macro_rules! over {
    ($self:ident, $upper:ident: $($field:ident),+) => {
        $( if $upper.$field.is_some() { $self.$field = $upper.$field.clone(); } )+
    };
}

impl RunProps {
    /// Put `upper` over this: what it sets wins.
    pub fn merge(&mut self, upper: &RunProps) {
        over!(
            self, upper: style, font, east_asian_font, size, bold, italic, underline, strike,
            double_strike, color, highlight, shading, position, caps, small_caps, hidden, spacing,
            scale, raise, lang, lang_ea
        );
    }

    /// Set what this layer says in `look`.
    pub fn apply(&self, look: &mut RunLook) {
        if let Some(font) = &self.font {
            look.font = Some(font.clone());
        }
        if let Some(font) = &self.east_asian_font {
            look.east_asian_font = Some(font.clone());
        }
        if let Some(size) = self.size {
            look.size = size;
        }
        if let Some(v) = self.bold {
            look.bold = v;
        }
        if let Some(v) = self.italic {
            look.italic = v;
        }
        if let Some(v) = self.underline {
            look.underline = v;
        }
        if let Some(v) = self.strike {
            look.strike = v;
        }
        if let Some(v) = self.double_strike {
            look.double_strike = v;
        }
        if let Some(v) = self.color {
            look.color = v;
        }
        if let Some(v) = self.highlight {
            look.highlight = v;
        }
        if let Some(v) = self.shading {
            look.shading = v;
        }
        if let Some(v) = self.position {
            look.position = v;
        }
        if let Some(v) = self.caps {
            look.caps = v;
        }
        if let Some(v) = self.small_caps {
            look.small_caps = v;
        }
        if let Some(v) = self.hidden {
            look.hidden = v;
        }
        if let Some(v) = self.spacing {
            look.spacing = v;
        }
        if let Some(v) = self.scale {
            look.scale = v;
        }
        if let Some(v) = self.raise {
            look.raise = v;
        }
    }
}

/// The look the layers add up to, lowest first.
pub fn finalize_run(layers: &[&RunProps]) -> RunLook {
    let mut look = RunLook::default();
    for layer in layers {
        layer.apply(&mut look);
    }
    look
}

/// Names Word uses for theme colours, and the scheme names of the theme.
fn scheme_name(name: &str) -> &str {
    match name {
        "dark1" | "text1" => "dk1",
        "light1" | "background1" => "lt1",
        "dark2" | "text2" => "dk2",
        "light2" | "background2" => "lt2",
        "hyperlink" => "hlink",
        "followedHyperlink" => "folHlink",
        other => other,
    }
}

/// `themeTint` lightens towards white, `themeShade` darkens towards black;
/// both are a byte in hexadecimal.
fn tinted(color: Rgba, tint: Option<&str>, shade: Option<&str>) -> Rgba {
    let mut out = color;
    if let Some(t) = tint.and_then(|t| u8::from_str_radix(t.trim(), 16).ok()) {
        let t = f32::from(t) / 255.0;
        for c in out.iter_mut().take(3) {
            *c = (f32::from(*c) * t + 255.0 * (1.0 - t))
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
    if let Some(s) = shade.and_then(|s| u8::from_str_radix(s.trim(), 16).ok()) {
        let s = f32::from(s) / 255.0;
        for c in out.iter_mut().take(3) {
            *c = (f32::from(*c) * s).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// A colour attribute set (`val`, `themeColor`, `themeTint`, `themeShade`)
/// on `node`, naming them `val_name` and `theme_name`. `Some(None)` is
/// automatic.
fn color_of(node: &Node<'_>, env: &Env, val_name: &str, theme_name: &str) -> Option<Option<Rgba>> {
    let (tint, shade) = if theme_name == "themeColor" {
        (node.attr("themeTint"), node.attr("themeShade"))
    } else {
        (node.attr("themeFillTint"), node.attr("themeFillShade"))
    };
    if let Some(theme) = node.attr(theme_name) {
        let color = env.theme.color(scheme_name(&theme));
        return Some(Some(tinted(color, tint.as_deref(), shade.as_deref())));
    }
    let value = node.attr(val_name)?;
    if value.eq_ignore_ascii_case("auto") {
        return Some(None);
    }
    parse_hex(&value).map(Some)
}

fn highlight_color(name: &str) -> Option<Option<Rgba>> {
    let rgb = |v: u32| Some(Some([(v >> 16) as u8, (v >> 8) as u8, v as u8, 255]));
    match name {
        "none" => Some(None),
        "black" => rgb(0x000000),
        "blue" => rgb(0x0000FF),
        "cyan" => rgb(0x00FFFF),
        "darkBlue" => rgb(0x000080),
        "darkCyan" => rgb(0x008080),
        "darkGray" => rgb(0x808080),
        "darkGreen" => rgb(0x008000),
        "darkMagenta" => rgb(0x800080),
        "darkRed" => rgb(0x800000),
        "darkYellow" => rgb(0x808000),
        "green" => rgb(0x00FF00),
        "lightGray" => rgb(0xC0C0C0),
        "magenta" => rgb(0xFF00FF),
        "red" => rgb(0xFF0000),
        "white" => rgb(0xFFFFFF),
        "yellow" => rgb(0xFFFF00),
        _ => None,
    }
}

fn underline_style(name: &str) -> Option<Option<UnderlineStyle>> {
    Some(Some(match name {
        "none" => return Some(None),
        "single" => UnderlineStyle::Single,
        "words" => UnderlineStyle::Words,
        "double" => UnderlineStyle::Double,
        "thick" => UnderlineStyle::Thick,
        "dotted" | "dottedHeavy" => UnderlineStyle::Dotted,
        "wave" | "wavyHeavy" | "wavyDouble" => UnderlineStyle::Wave,
        n if n.starts_with("dash") || n.starts_with("dotDash") || n.starts_with("dotDotDash") => {
            UnderlineStyle::Dashed
        }
        _ => UnderlineStyle::Single,
    }))
}

/// A shading element: the fill (or, for a solid pattern, the colour).
pub fn shading_of(node: &Node<'_>, env: &Env) -> Option<Option<Rgba>> {
    if node.attr("val").as_deref() == Some("solid")
        && let Some(color) = color_of(node, env, "color", "themeColor")
    {
        return Some(color);
    }
    color_of(node, env, "fill", "themeFill")
}

/// Properties of `w:rPr`.
pub fn parse_rpr(node: &Node<'_>, env: &Env) -> RunProps {
    let mut p = RunProps::default();
    for c in &node.children {
        match c.local() {
            b"rStyle" => p.style = c.val().map(|v| env.intern(&v)),
            b"rFonts" => {
                let ascii = c.attr("asciiTheme").or_else(|| c.attr("hAnsiTheme"));
                if let Some(face) = ascii.and_then(|n| env.theme_font(&n)) {
                    p.font = Some(face);
                } else if let Some(name) = c.attr("ascii").or_else(|| c.attr("hAnsi"))
                    && !name.is_empty()
                {
                    p.font = Some(env.intern(&name));
                }
                if let Some(face) = c.attr("eastAsiaTheme").and_then(|n| env.theme_font(&n)) {
                    p.east_asian_font = Some(face);
                } else if let Some(name) = c.attr("eastAsia")
                    && !name.is_empty()
                {
                    p.east_asian_font = Some(env.intern(&name));
                }
            }
            b"sz" => p.size = c.int("val").map(|v| (v.clamp(1, 4000) as f32) / 2.0),
            b"b" => p.bold = Some(c.on()),
            b"i" => p.italic = Some(c.on()),
            b"strike" => p.strike = Some(c.on()),
            b"dstrike" => p.double_strike = Some(c.on()),
            b"caps" => p.caps = Some(c.on()),
            b"smallCaps" => p.small_caps = Some(c.on()),
            b"vanish" => p.hidden = Some(c.on()),
            b"u" => {
                let style = c.val().as_deref().and_then(underline_style);
                p.underline = match style {
                    None => None,
                    Some(None) => Some(None),
                    Some(Some(style)) => Some(Some(Underline {
                        style,
                        color: color_of(c, env, "color", "themeColor").flatten(),
                    })),
                };
            }
            b"color" => p.color = color_of(c, env, "val", "themeColor"),
            b"highlight" => p.highlight = c.val().and_then(|v| highlight_color(&v)),
            b"shd" => p.shading = shading_of(c, env),
            b"vertAlign" => {
                p.position = match c.val().as_deref() {
                    Some("superscript") => Some(VertAlign::Superscript),
                    Some("subscript") => Some(VertAlign::Subscript),
                    Some("baseline") => Some(VertAlign::Baseline),
                    _ => None,
                }
            }
            b"spacing" => p.spacing = c.int("val").map(|v| v.clamp(-31_680, 31_680) as Twips),
            b"w" => p.scale = c.int("val").map(|v| (v.clamp(1, 600) as f32) / 100.0),
            b"position" => p.raise = c.int("val").map(|v| (v.clamp(-3_000, 3_000) as f32) / 2.0),
            b"lang" => {
                p.lang = c.attr("val");
                p.lang_ea = c.attr("eastAsia");
            }
            _ => {}
        }
    }
    p
}

// ---------------------------------------------------------------------------
// Borders and shading
// ---------------------------------------------------------------------------

/// A border element. `Some(None)` is "no border" (`nil`, `none`).
pub fn parse_border(node: &Node<'_>, env: &Env) -> Option<Option<Border>> {
    let value = node.attr("val")?;
    let style = match value.as_str() {
        "nil" | "none" => return Some(None),
        "single" | "inset" | "outset" | "hairline" => BorderStyle::Single,
        "double" => BorderStyle::Double,
        "dotted" => BorderStyle::Dotted,
        "dashed" | "dashSmallGap" | "dashDotStroked" => BorderStyle::Dashed,
        "dotDash" | "dotDotDash" => BorderStyle::DotDash,
        "wave" | "doubleWave" => BorderStyle::Wave,
        _ => BorderStyle::Other,
    };
    let eighths = node.int("sz").unwrap_or(4).clamp(0, 96);
    Some(Some(Border {
        style,
        width: (eighths as f32 / 8.0).max(0.25),
        color: color_of(node, env, "color", "themeColor").flatten(),
        space: node.int("space").unwrap_or(0).clamp(0, 31) as f32,
    }))
}

/// Left, top, right, bottom: the model's order.
pub type Sides = [Option<Option<Border>>; 4];

/// Merge border sides: what `upper` sets wins.
pub fn merge_sides(lower: &mut Sides, upper: &Sides) {
    for (l, u) in lower.iter_mut().zip(upper) {
        if u.is_some() {
            *l = *u;
        }
    }
}

/// Borders of a `pBdr` or `tcBorders` or `tblBorders`: left (or start), top,
/// right (or end), bottom.
pub fn parse_sides(node: &Node<'_>, env: &Env) -> Sides {
    let mut out: Sides = [None; 4];
    for c in &node.children {
        let at = match c.local() {
            b"left" | b"start" => 0,
            b"top" => 1,
            b"right" | b"end" => 2,
            b"bottom" => 3,
            _ => continue,
        };
        out[at] = parse_border(c, env);
    }
    out
}

// ---------------------------------------------------------------------------
// Paragraphs
// ---------------------------------------------------------------------------

/// A length in twips, or in hundredths of the paragraph's character size;
/// the characters win when they are there and not zero.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Len {
    pub twips: Option<i32>,
    pub chars: Option<i32>,
}

impl Len {
    pub fn resolve(&self, size_pt: f32) -> Twips {
        match self.chars {
            Some(chars) if chars != 0 => {
                (f64::from(chars) / 100.0 * f64::from(size_pt) * 20.0).round() as Twips
            }
            _ => self.twips.unwrap_or(0),
        }
    }
}

/// Space before or after: in twips, in hundredths of a line, or automatic.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Space {
    pub twips: Option<i32>,
    pub lines: Option<i32>,
    pub auto: bool,
}

impl Space {
    fn resolve(&self, pitch: Twips) -> Twips {
        if self.auto {
            return 280;
        }
        match self.lines {
            Some(lines) if lines != 0 => {
                (f64::from(lines) / 100.0 * f64::from(pitch)).round() as Twips
            }
            _ => self.twips.unwrap_or(0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabEntry {
    pub position: Twips,
    /// `None` clears an inherited stop.
    pub stop: Option<(TabAlign, TabLeader)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParaPartial {
    pub style: Option<Arc<str>>,
    pub align: Option<Align>,
    pub left: Option<Len>,
    pub right: Option<Len>,
    pub first: Option<Len>,
    pub before: Option<Space>,
    pub after: Option<Space>,
    pub line: Option<LineSpacing>,
    pub contextual: Option<bool>,
    pub keep_next: Option<bool>,
    pub keep_lines: Option<bool>,
    pub page_break_before: Option<bool>,
    pub widow: Option<bool>,
    pub snap: Option<bool>,
    pub outline: Option<Option<u8>>,
    pub num_id: Option<u32>,
    pub ilvl: Option<u8>,
    pub tabs: Vec<TabEntry>,
    pub borders: Sides,
    pub between: Option<Option<Border>>,
    pub shading: Option<Option<Rgba>>,
}

impl ParaPartial {
    pub fn merge(&mut self, upper: &ParaPartial) {
        over!(
            self, upper: style, align, left, right, first, before, after, line, contextual,
            keep_next, keep_lines, page_break_before, widow, snap, outline, num_id, ilvl, between,
            shading
        );
        self.tabs.extend_from_slice(&upper.tabs);
        merge_sides(&mut self.borders, &upper.borders);
    }
}

pub struct Ppr<'n, 'a> {
    pub props: ParaPartial,
    /// `pPr/rPr`: the paragraph mark's own.
    pub mark: Option<RunProps>,
    pub section: Option<&'n Node<'a>>,
}

fn align_of(value: &str) -> Option<Align> {
    Some(match value {
        "left" | "start" => Align::Left,
        "center" => Align::Center,
        "right" | "end" => Align::Right,
        "both" | "justify" | "mediumKashida" | "highKashida" | "lowKashida" | "thaiDistribute" => {
            Align::Justify
        }
        "distribute" => Align::Distribute,
        _ => return None,
    })
}

fn tab_stop(c: &Node<'_>) -> Option<TabEntry> {
    let position = c.int("pos")?.clamp(-1_000_000, 1_000_000) as Twips;
    let stop = match c.attr("val").as_deref().unwrap_or("left") {
        "clear" => None,
        kind => {
            let align = match kind {
                "center" => TabAlign::Center,
                "right" | "end" => TabAlign::Right,
                "decimal" => TabAlign::Decimal,
                "bar" => TabAlign::Bar,
                _ => TabAlign::Left,
            };
            let leader = match c.attr("leader").as_deref() {
                Some("dot") => TabLeader::Dots,
                Some("hyphen") => TabLeader::Hyphens,
                Some("underscore" | "heavy") => TabLeader::Underscore,
                Some("middleDot") => TabLeader::MiddleDots,
                _ => TabLeader::None,
            };
            Some((align, leader))
        }
    };
    Some(TabEntry { position, stop })
}

/// Properties of `w:pPr`.
pub fn parse_ppr<'n, 'a>(node: &'n Node<'a>, env: &Env) -> Ppr<'n, 'a> {
    let mut p = ParaPartial::default();
    let mut mark = None;
    let mut section = None;
    for c in &node.children {
        match c.local() {
            b"pStyle" => p.style = c.val().map(|v| env.intern(&v)),
            b"jc" => p.align = c.val().as_deref().and_then(align_of),
            b"ind" => {
                let len = |twips: &[&str], chars: &str| {
                    let t = twips.iter().find_map(|name| c.int(name));
                    let ch = c.int(chars);
                    (t.is_some() || ch.is_some()).then(|| Len {
                        twips: t.map(clamp_len),
                        chars: ch.map(|v| v.clamp(-100_000, 100_000) as i32),
                    })
                };
                if let Some(l) = len(&["left", "start"], "leftChars").or(len(&[], "startChars")) {
                    p.left = Some(l);
                }
                if let Some(l) = len(&["right", "end"], "rightChars").or(len(&[], "endChars")) {
                    p.right = Some(l);
                }
                if let Some(l) = len(&["hanging"], "hangingChars")
                    .filter(|l| l.twips.is_some_and(|t| t != 0) || l.chars.is_some_and(|c| c != 0))
                {
                    p.first = Some(Len {
                        twips: l.twips.map(|t| -t),
                        chars: l.chars.map(|c| -c),
                    });
                } else if let Some(l) = len(&["firstLine"], "firstLineChars") {
                    p.first = Some(l);
                } else if c.attr("hanging").is_some() || c.attr("hangingChars").is_some() {
                    p.first = Some(Len {
                        twips: Some(0),
                        chars: None,
                    });
                }
            }
            b"spacing" => {
                let space = |name: &str, lines: &str, auto: &str| {
                    let t = c.int(name);
                    let l = c.int(lines);
                    let a = c
                        .attr(auto)
                        .is_some_and(|v| matches!(v.trim(), "1" | "true" | "on"));
                    (t.is_some() || l.is_some() || c.attr(auto).is_some()).then(|| Space {
                        twips: t.map(clamp_len),
                        lines: l.map(|v| v.clamp(-100_000, 100_000) as i32),
                        auto: a,
                    })
                };
                if let Some(s) = space("before", "beforeLines", "beforeAutospacing") {
                    p.before = Some(s);
                }
                if let Some(s) = space("after", "afterLines", "afterAutospacing") {
                    p.after = Some(s);
                }
                if let Some(line) = c.int("line") {
                    let line = line.clamp(-1_000_000, 1_000_000) as i32;
                    p.line = Some(match c.attr("lineRule").as_deref() {
                        Some("atLeast") => LineSpacing::AtLeast(line),
                        Some("exact") => LineSpacing::Exactly(line),
                        _ => LineSpacing::Multiple(line as f32 / 240.0),
                    });
                }
            }
            b"contextualSpacing" => p.contextual = Some(c.on()),
            b"keepNext" => p.keep_next = Some(c.on()),
            b"keepLines" => p.keep_lines = Some(c.on()),
            b"pageBreakBefore" => p.page_break_before = Some(c.on()),
            b"widowControl" => p.widow = Some(c.on()),
            b"snapToGrid" => p.snap = Some(c.on()),
            b"outlineLvl" => {
                p.outline = c
                    .int("val")
                    .map(|v| (0..=8).contains(&v).then_some(v as u8));
            }
            b"numPr" => {
                if let Some(v) = c.child("numId").and_then(|n| n.int("val")) {
                    p.num_id = Some(v.clamp(0, i64::from(u32::MAX)) as u32);
                }
                if let Some(v) = c.child("ilvl").and_then(|n| n.int("val")) {
                    p.ilvl = Some(v.clamp(0, 8) as u8);
                }
            }
            b"tabs" => p.tabs.extend(c.each("tab").filter_map(tab_stop)),
            b"pBdr" => {
                p.borders = parse_sides(c, env);
                p.between = c.child("between").and_then(|b| parse_border(b, env));
            }
            b"shd" => p.shading = shading_of(c, env),
            b"rPr" => mark = Some(parse_rpr(c, env)),
            b"sectPr" => section = Some(c),
            _ => {}
        }
    }
    Ppr {
        props: p,
        mark,
        section,
    }
}

pub fn clamp_len(v: i64) -> i32 {
    v.clamp(-1_000_000, 1_000_000) as i32
}

/// The final properties the layers add up to. `size_pt` is the paragraph's
/// character size (for indents in characters), `pitch` a line's height in
/// twips (for spacing in lines).
pub fn finalize_para(p: &ParaPartial, size_pt: f32, pitch: Twips) -> ParaProps {
    let mut tabs: Vec<TabStop> = Vec::new();
    for entry in &p.tabs {
        tabs.retain(|t| t.position != entry.position);
        if let Some((align, leader)) = entry.stop {
            tabs.push(TabStop {
                position: entry.position,
                align,
                leader,
            });
        }
    }
    tabs.sort_by_key(|t| t.position);
    let def = ParaProps::default();
    ParaProps {
        style: p.style.clone(),
        align: p.align.unwrap_or(def.align),
        indent_left: p.left.map_or(0, |l| l.resolve(size_pt)),
        indent_right: p.right.map_or(0, |l| l.resolve(size_pt)),
        first_line: p.first.map_or(0, |l| l.resolve(size_pt)),
        space_before: p.before.map_or(0, |s| s.resolve(pitch)),
        space_after: p.after.map_or(0, |s| s.resolve(pitch)),
        line: p.line.unwrap_or(def.line),
        contextual_spacing: p.contextual.unwrap_or(false),
        keep_next: p.keep_next.unwrap_or(false),
        keep_lines: p.keep_lines.unwrap_or(false),
        page_break_before: p.page_break_before.unwrap_or(false),
        widow_control: p.widow.unwrap_or(true),
        outline_level: p.outline.flatten(),
        list: match p.num_id {
            Some(list) if list > 0 => Some(ListRef {
                list,
                level: p.ilvl.unwrap_or(0).min(8),
            }),
            _ => None,
        },
        tabs,
        borders: [
            p.borders[0].flatten(),
            p.borders[1].flatten(),
            p.borders[2].flatten(),
            p.borders[3].flatten(),
        ],
        border_between: p.between.flatten(),
        shading: p.shading.flatten(),
        snap_to_grid: p.snap.unwrap_or(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docx::dom;

    fn env() -> Env {
        Env::new(Theme::default(), String::new())
    }

    fn rpr(xml: &str) -> RunProps {
        let xml = format!(r#"<w:rPr xmlns:w="w">{xml}</w:rPr>"#);
        let root = dom::parse(xml.as_bytes(), "t.xml").unwrap();
        parse_rpr(&root, &env())
    }

    fn ppr(xml: &str) -> ParaPartial {
        let xml = format!(r#"<w:pPr xmlns:w="w">{xml}</w:pPr>"#);
        let root = dom::parse(xml.as_bytes(), "t.xml").unwrap();
        parse_ppr(&root, &env()).props
    }

    #[test]
    fn toggles_and_values() {
        let p = rpr(
            r#"<w:b/><w:i w:val="0"/><w:sz w:val="21"/><w:u w:val="dotDash" w:color="FF0000"/><w:strike w:val="off"/>"#,
        );
        assert_eq!(p.bold, Some(true));
        assert_eq!(p.italic, Some(false));
        assert_eq!(p.size, Some(10.5));
        assert_eq!(p.strike, Some(false));
        let u = p.underline.unwrap().unwrap();
        assert_eq!(u.style, UnderlineStyle::Dashed);
        assert_eq!(u.color, Some([255, 0, 0, 255]));
    }

    #[test]
    fn colours_themes_and_highlights() {
        let p = rpr(r#"<w:color w:val="auto"/><w:highlight w:val="yellow"/>"#);
        assert_eq!(p.color, Some(None));
        assert_eq!(p.highlight, Some(Some([255, 255, 0, 255])));
        let p = rpr(r#"<w:color w:val="000000" w:themeColor="accent1"/>"#);
        assert_eq!(p.color, Some(Some(env().theme.color("accent1"))));
        let p = rpr(r#"<w:color w:val="000000" w:themeColor="background1" w:themeShade="80"/>"#);
        assert_eq!(p.color, Some(Some([128, 128, 128, 255])));
    }

    #[test]
    fn indents_in_characters_win_over_twips() {
        let p = ppr(r#"<w:ind w:left="300" w:firstLineChars="200" w:firstLine="100"/>"#);
        let props = finalize_para(&p, 10.5, 240);
        assert_eq!(props.indent_left, 300);
        assert_eq!(props.first_line, 420);
        let p = ppr(r#"<w:ind w:left="720" w:hanging="360"/>"#);
        assert_eq!(finalize_para(&p, 10.0, 240).first_line, -360);
        let p = ppr(r#"<w:ind w:leftChars="0" w:left="500"/>"#);
        assert_eq!(finalize_para(&p, 10.0, 240).indent_left, 500);
    }

    #[test]
    fn spacing_in_lines_and_automatic() {
        let p = ppr(
            r#"<w:spacing w:beforeLines="50" w:after="100" w:afterAutospacing="1" w:line="360" w:lineRule="auto"/>"#,
        );
        let props = finalize_para(&p, 10.0, 312);
        assert_eq!(props.space_before, 156);
        assert_eq!(props.space_after, 280);
        assert_eq!(props.line, LineSpacing::Multiple(1.5));
        let p = ppr(r#"<w:spacing w:line="400" w:lineRule="exact"/>"#);
        assert_eq!(finalize_para(&p, 10.0, 240).line, LineSpacing::Exactly(400));
    }

    #[test]
    fn tabs_clear_what_a_lower_layer_set() {
        let mut lower = ppr(
            r#"<w:tabs><w:tab w:val="left" w:pos="720"/><w:tab w:val="right" w:leader="dot" w:pos="8000"/></w:tabs>"#,
        );
        let upper = ppr(
            r#"<w:tabs><w:tab w:val="clear" w:pos="720"/><w:tab w:val="center" w:pos="4000"/></w:tabs>"#,
        );
        lower.merge(&upper);
        let tabs = finalize_para(&lower, 10.0, 240).tabs;
        assert_eq!(
            tabs.iter().map(|t| t.position).collect::<Vec<_>>(),
            vec![4000, 8000]
        );
        assert_eq!(tabs[1].leader, TabLeader::Dots);
    }

    #[test]
    fn outline_level_nine_is_body_text() {
        assert_eq!(
            finalize_para(&ppr(r#"<w:outlineLvl w:val="9"/>"#), 10.0, 240).outline_level,
            None
        );
        assert_eq!(
            finalize_para(&ppr(r#"<w:outlineLvl w:val="2"/>"#), 10.0, 240).outline_level,
            Some(2)
        );
    }
}
