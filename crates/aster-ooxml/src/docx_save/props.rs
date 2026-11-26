//! Run and paragraph properties as elements: patching the ones a file had
//! with what changed, and writing the ones a new run or paragraph needs.
//!
//! The model's looks and properties are final (what styles and direct
//! formatting add up to). A patch therefore writes explicit values for what
//! differs from what the element was read as; a value that is the one the
//! paragraph's base look already gives is not written but removed, so that
//! turning bold on and off again leaves no `<w:b w:val="0"/>` behind.

use std::sync::Arc;

use aster_writer::{
    Align, Border, BorderStyle, LineSpacing, ListRef, ParaProps, Rgba, RunLook, TabAlign,
    TabLeader, UnderlineStyle, VertAlign,
};

use crate::xml_edit::{
    children, edit_children, escape_attribute, is_bare, remove_attribute, set_attribute,
};

const RPR_ORDER: &[&str] = &[
    "rStyle",
    "rFonts",
    "b",
    "bCs",
    "i",
    "iCs",
    "caps",
    "smallCaps",
    "strike",
    "dstrike",
    "outline",
    "shadow",
    "emboss",
    "imprint",
    "noProof",
    "snapToGrid",
    "vanish",
    "webHidden",
    "color",
    "spacing",
    "w",
    "kern",
    "position",
    "sz",
    "szCs",
    "highlight",
    "u",
    "effect",
    "bdr",
    "shd",
    "fitText",
    "vertAlign",
    "rtl",
    "cs",
    "em",
    "lang",
    "eastAsianLayout",
    "specVanish",
    "oMath",
];

const PPR_ORDER: &[&str] = &[
    "pStyle",
    "keepNext",
    "keepLines",
    "pageBreakBefore",
    "framePr",
    "widowControl",
    "numPr",
    "suppressLineNumbers",
    "pBdr",
    "shd",
    "tabs",
    "suppressAutoHyphens",
    "kinsoku",
    "wordWrap",
    "overflowPunct",
    "topLinePunct",
    "autoSpaceDE",
    "autoSpaceDN",
    "bidi",
    "adjustRightInd",
    "snapToGrid",
    "spacing",
    "ind",
    "contextualSpacing",
    "mirrorIndents",
    "suppressOverlap",
    "jc",
    "textDirection",
    "textAlignment",
    "textboxTightWrap",
    "outlineLvl",
    "divId",
    "cnfStyle",
    "rPr",
    "sectPr",
    "pPrChange",
];

/// `element` (a whole element, or empty to take it out) in place of the
/// child of `xml` named `name`, at the place the schema gives it.
fn put(xml: &str, order: &[&str], name: &str, element: &str) -> String {
    let mut after: Vec<&str> = order
        .iter()
        .position(|n| *n == name)
        .map_or(Vec::new(), |at| order[at + 1..].to_vec());
    // Elements the schema order does not know (extensions) come last.
    let spans = children(xml);
    after.extend(
        spans
            .iter()
            .map(|c| c.name.as_str())
            .filter(|n| !order.contains(n)),
    );
    edit_children(xml, &[name], element, &after)
}

fn child_text<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    children(xml)
        .into_iter()
        .find(|c| c.name == name)
        .map(|c| &xml[c.start..c.end])
}

pub fn hex(color: Rgba) -> String {
    format!("{:02X}{:02X}{:02X}", color[0], color[1], color[2])
}

const HIGHLIGHTS: [(&str, u32); 16] = [
    ("black", 0x000000),
    ("blue", 0x0000FF),
    ("cyan", 0x00FFFF),
    ("darkBlue", 0x000080),
    ("darkCyan", 0x008080),
    ("darkGray", 0x808080),
    ("darkGreen", 0x008000),
    ("darkMagenta", 0x800080),
    ("darkRed", 0x800000),
    ("darkYellow", 0x808000),
    ("green", 0x00FF00),
    ("lightGray", 0xC0C0C0),
    ("magenta", 0xFF00FF),
    ("red", 0xFF0000),
    ("white", 0xFFFFFF),
    ("yellow", 0xFFFF00),
];

/// The highlighter colour nearest to `color`.
fn highlight_name(color: Rgba) -> &'static str {
    let [r, g, b, _] = color;
    HIGHLIGHTS
        .iter()
        .min_by_key(|(_, rgb)| {
            let d = |shift: u32, v: u8| {
                let c = ((rgb >> shift) & 0xFF) as i32;
                (c - i32::from(v)).pow(2)
            };
            d(16, r) + d(8, g) + d(0, b)
        })
        .map_or("yellow", |(name, _)| name)
}

fn underline_name(style: UnderlineStyle) -> &'static str {
    match style {
        UnderlineStyle::Single => "single",
        UnderlineStyle::Double => "double",
        UnderlineStyle::Thick => "thick",
        UnderlineStyle::Dotted => "dotted",
        UnderlineStyle::Dashed => "dash",
        UnderlineStyle::Wave => "wave",
        UnderlineStyle::Words => "words",
    }
}

fn toggle(w: &str, name: &str, on: bool) -> String {
    if on {
        format!("<{w}{name}/>")
    } else {
        format!("<{w}{name} {w}val=\"0\"/>")
    }
}

fn val(w: &str, name: &str, value: impl std::fmt::Display) -> String {
    format!(
        "<{w}{name} {w}val=\"{}\"/>",
        escape_attribute(&value.to_string())
    )
}

/// Patch the typefaces of an `rFonts` element.
fn fonts(w: &str, rpr: &str, latin: Option<Option<&str>>, east: Option<Option<&str>>) -> String {
    let mut element =
        child_text(rpr, "rFonts").map_or_else(|| format!("<{w}rFonts/>"), str::to_owned);
    if let Some(face) = latin {
        element = remove_attribute(&element, &format!("{w}asciiTheme"));
        element = remove_attribute(&element, &format!("{w}hAnsiTheme"));
        element = match face {
            Some(face) => {
                let e = set_attribute(&element, &format!("{w}ascii"), face);
                set_attribute(&e, &format!("{w}hAnsi"), face)
            }
            None => {
                let e = remove_attribute(&element, &format!("{w}ascii"));
                remove_attribute(&e, &format!("{w}hAnsi"))
            }
        };
    }
    if let Some(face) = east {
        element = remove_attribute(&element, &format!("{w}eastAsiaTheme"));
        element = match face {
            Some(face) => set_attribute(&element, &format!("{w}eastAsia"), face),
            None => remove_attribute(&element, &format!("{w}eastAsia")),
        };
    }
    if is_bare(&element) {
        element.clear();
    }
    put(rpr, RPR_ORDER, "rFonts", &element)
}

/// The `rPr` element for a run, from the one it had (`old`) and the look and
/// character style it had then (`src`) to the ones it has now. `base` is
/// what the paragraph gives a run with no formatting. Empty if no
/// properties are left.
#[allow(clippy::too_many_arguments)]
pub fn rpr_patch(
    w: &str,
    old: Option<&str>,
    src: &RunLook,
    src_style: &Option<Arc<str>>,
    new: &RunLook,
    new_style: &Option<Arc<str>>,
    base: &RunLook,
) -> String {
    let mut rpr = old.map_or_else(|| format!("<{w}rPr></{w}rPr>"), str::to_owned);
    // A value that is the base's needs no element, but only where nothing
    // between the base and the direct formatting (a character style) could
    // say otherwise.
    let plain = new_style.is_none();
    if src_style != new_style {
        let element = new_style
            .as_deref()
            .map_or(String::new(), |s| val(w, "rStyle", s));
        rpr = put(&rpr, RPR_ORDER, "rStyle", &element);
    }
    if new.font != src.font || new.east_asian_font != src.east_asian_font {
        let latin = (new.font != src.font).then(|| {
            if plain && new.font == base.font {
                None
            } else {
                new.font.as_deref()
            }
        });
        let east = (new.east_asian_font != src.east_asian_font).then(|| {
            if plain && new.east_asian_font == base.east_asian_font {
                None
            } else {
                new.east_asian_font.as_deref()
            }
        });
        rpr = fonts(w, &rpr, latin, east);
    }
    macro_rules! flag {
        ($field:ident, $name:expr) => {
            if new.$field != src.$field {
                let element = if plain && new.$field == base.$field {
                    String::new()
                } else {
                    toggle(w, $name, new.$field)
                };
                rpr = put(&rpr, RPR_ORDER, $name, &element);
            }
        };
    }
    flag!(bold, "b");
    flag!(italic, "i");
    flag!(caps, "caps");
    flag!(small_caps, "smallCaps");
    flag!(strike, "strike");
    flag!(double_strike, "dstrike");
    flag!(hidden, "vanish");
    macro_rules! other {
        ($field:ident, $name:expr, $element:expr) => {
            if new.$field != src.$field {
                let element = if plain && new.$field == base.$field {
                    String::new()
                } else {
                    $element
                };
                rpr = put(&rpr, RPR_ORDER, $name, &element);
            }
        };
    }
    other!(
        color,
        "color",
        val(w, "color", new.color.map_or("auto".to_owned(), hex))
    );
    other!(spacing, "spacing", val(w, "spacing", new.spacing));
    other!(scale, "w", val(w, "w", (new.scale * 100.0).round() as i64));
    other!(
        raise,
        "position",
        val(w, "position", (new.raise * 2.0).round() as i64)
    );
    other!(
        size,
        "sz",
        val(w, "sz", (new.size * 2.0).round().clamp(1.0, 4000.0) as i64)
    );
    other!(
        highlight,
        "highlight",
        val(w, "highlight", new.highlight.map_or("none", highlight_name))
    );
    other!(
        underline,
        "u",
        match new.underline {
            None => val(w, "u", "none"),
            Some(u) => {
                let mut e = val(w, "u", underline_name(u.style));
                if let Some(color) = u.color {
                    e = set_attribute(&e, &format!("{w}color"), &hex(color));
                }
                e
            }
        }
    );
    other!(
        shading,
        "shd",
        format!(
            "<{w}shd {w}val=\"clear\" {w}color=\"auto\" {w}fill=\"{}\"/>",
            new.shading.map_or("auto".to_owned(), hex)
        )
    );
    other!(
        position,
        "vertAlign",
        val(
            w,
            "vertAlign",
            match new.position {
                VertAlign::Baseline => "baseline",
                VertAlign::Superscript => "superscript",
                VertAlign::Subscript => "subscript",
            }
        )
    );
    if is_bare(&rpr) { String::new() } else { rpr }
}

// ---------------------------------------------------------------------------
// Paragraphs
// ---------------------------------------------------------------------------

/// What happens to the section break in a paragraph's properties.
pub enum SectionEdit {
    Keep,
    Remove,
    /// The `sectPr` element to have there.
    Set(String),
}

fn align_name(align: Align) -> &'static str {
    match align {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
        Align::Justify => "both",
        Align::Distribute => "distribute",
    }
}

fn border_element(w: &str, name: &str, border: Option<&Border>) -> String {
    let Some(b) = border else {
        return format!("<{w}{name} {w}val=\"nil\"/>");
    };
    let style = match b.style {
        BorderStyle::Single => "single",
        BorderStyle::Double => "double",
        BorderStyle::Dotted => "dotted",
        BorderStyle::Dashed => "dashed",
        BorderStyle::DotDash => "dotDash",
        BorderStyle::Wave => "wave",
        BorderStyle::Other => "thick",
    };
    format!(
        "<{w}{name} {w}val=\"{style}\" {w}sz=\"{}\" {w}space=\"{}\" {w}color=\"{}\"/>",
        (b.width * 8.0).round().clamp(2.0, 96.0) as i64,
        b.space.round() as i64,
        b.color.map_or("auto".to_owned(), hex)
    )
}

/// `<w:pBdr>` or `<w:tcBorders>`-style children: left, top, right, bottom
/// and, for paragraphs, between.
pub fn borders_element(
    w: &str,
    name: &str,
    sides: &[Option<Border>; 4],
    between: Option<Option<&Border>>,
    names: [&str; 4],
) -> String {
    let mut out = format!("<{w}{name}>");
    // The schema's order: top, left, bottom, right.
    for at in [1, 0, 3, 2] {
        out.push_str(&border_element(w, names[at], sides[at].as_ref()));
    }
    if let Some(between) = between {
        out.push_str(&border_element(w, "between", between));
    }
    out.push_str(&format!("</{w}{name}>"));
    out
}

fn tabs_element(w: &str, tabs: &[aster_writer::TabStop]) -> String {
    if tabs.is_empty() {
        return String::new();
    }
    let mut out = format!("<{w}tabs>");
    for t in tabs {
        let kind = match t.align {
            TabAlign::Left => "left",
            TabAlign::Center => "center",
            TabAlign::Right => "right",
            TabAlign::Decimal => "decimal",
            TabAlign::Bar => "bar",
        };
        let leader = match t.leader {
            TabLeader::None => "",
            TabLeader::Dots => " dot",
            TabLeader::Hyphens => " hyphen",
            TabLeader::Underscore => " underscore",
            TabLeader::MiddleDots => " middleDot",
        };
        let leader = match leader.trim() {
            "" => String::new(),
            name => format!(" {w}leader=\"{name}\""),
        };
        out.push_str(&format!(
            "<{w}tab {w}val=\"{kind}\"{leader} {w}pos=\"{}\"/>",
            t.position
        ));
    }
    out.push_str(&format!("</{w}tabs>"));
    out
}

fn ind_element(w: &str, p: &ParaProps) -> String {
    let first = if p.first_line < 0 {
        format!(" {w}hanging=\"{}\"", -p.first_line)
    } else {
        format!(" {w}firstLine=\"{}\"", p.first_line)
    };
    format!(
        "<{w}ind {w}left=\"{}\" {w}right=\"{}\"{first}/>",
        p.indent_left, p.indent_right
    )
}

fn spacing_element(w: &str, p: &ParaProps) -> String {
    let (line, rule) = match p.line {
        LineSpacing::Multiple(m) => ((m * 240.0).round() as i64, "auto"),
        LineSpacing::AtLeast(t) => (i64::from(t), "atLeast"),
        LineSpacing::Exactly(t) => (i64::from(t), "exact"),
    };
    format!(
        "<{w}spacing {w}before=\"{}\" {w}after=\"{}\" {w}line=\"{line}\" {w}lineRule=\"{rule}\"/>",
        p.space_before, p.space_after
    )
}

fn num_element(w: &str, list: Option<ListRef>) -> String {
    let (level, id) = list.map_or((0, 0), |l| (l.level, l.list));
    format!("<{w}numPr><{w}ilvl {w}val=\"{level}\"/><{w}numId {w}val=\"{id}\"/></{w}numPr>")
}

fn shading_element(w: &str, shading: Option<Rgba>) -> String {
    format!(
        "<{w}shd {w}val=\"clear\" {w}color=\"auto\" {w}fill=\"{}\"/>",
        shading.map_or("auto".to_owned(), hex)
    )
}

/// The `pPr` element for a paragraph, from the one it had (`old`) and the
/// properties and mark look it had then to the ones it has now; `section` says
/// what to do with a section break in it. `None` if nothing is left.
#[allow(clippy::too_many_arguments)]
pub fn ppr_patch(
    w: &str,
    old: Option<&str>,
    src: &ParaProps,
    new: &ParaProps,
    style: Option<&ParaProps>,
    src_mark: &RunLook,
    new_mark: &RunLook,
    base: &RunLook,
    section: SectionEdit,
    force_layout: bool,
) -> Option<String> {
    let mut ppr = old.map_or_else(|| format!("<{w}pPr></{w}pPr>"), str::to_owned);
    fn set(ppr: &mut String, name: &str, element: String) {
        *ppr = put(ppr, PPR_ORDER, name, &element);
    }
    if new.style != src.style {
        set(
            &mut ppr,
            "pStyle",
            new.style
                .as_deref()
                .map_or(String::new(), |s| val(w, "pStyle", s)),
        );
    }
    // A paragraph that has a new style: what it has of its own is what
    // differs from that style, and what it had of its own before may no
    // longer be wanted. Otherwise: what differs from what it was.
    let restyle = style.filter(|_| new.style != src.style);
    // 0: leave it, 1: write it, 2: take it out.
    let act = |changed: bool, differs_from_style: bool| -> u8 {
        match restyle {
            Some(_) if differs_from_style => 1,
            Some(_) => 2,
            None => u8::from(changed),
        }
    };
    macro_rules! field {
        ($name:expr, $differs:expr, $style:expr, $element:expr) => {
            match act($differs, restyle.is_some_and(|st| $style(st))) {
                1 => set(&mut ppr, $name, $element),
                2 => set(&mut ppr, $name, String::new()),
                _ => {}
            }
        };
    }
    field!(
        "keepNext",
        new.keep_next != src.keep_next,
        |st: &ParaProps| new.keep_next != st.keep_next,
        toggle(w, "keepNext", new.keep_next)
    );
    field!(
        "keepLines",
        new.keep_lines != src.keep_lines,
        |st: &ParaProps| new.keep_lines != st.keep_lines,
        toggle(w, "keepLines", new.keep_lines)
    );
    field!(
        "pageBreakBefore",
        new.page_break_before != src.page_break_before,
        |st: &ParaProps| new.page_break_before != st.page_break_before,
        toggle(w, "pageBreakBefore", new.page_break_before)
    );
    field!(
        "widowControl",
        new.widow_control != src.widow_control,
        |st: &ParaProps| new.widow_control != st.widow_control,
        toggle(w, "widowControl", new.widow_control)
    );
    field!(
        "numPr",
        new.list != src.list,
        |st: &ParaProps| new.list != st.list,
        num_element(w, new.list)
    );
    let borders_empty = new.borders.iter().all(Option::is_none) && new.border_between.is_none();
    field!(
        "pBdr",
        new.borders != src.borders || new.border_between != src.border_between,
        |st: &ParaProps| new.borders != st.borders || new.border_between != st.border_between,
        if borders_empty && src.borders.iter().all(Option::is_none) {
            String::new()
        } else {
            borders_element(
                w,
                "pBdr",
                &new.borders,
                Some(new.border_between.as_ref()),
                ["left", "top", "right", "bottom"],
            )
        }
    );
    field!(
        "shd",
        new.shading != src.shading,
        |st: &ParaProps| new.shading != st.shading,
        shading_element(w, new.shading)
    );
    field!(
        "tabs",
        new.tabs != src.tabs,
        |st: &ParaProps| new.tabs != st.tabs,
        tabs_element(w, &new.tabs)
    );
    field!(
        "snapToGrid",
        new.snap_to_grid != src.snap_to_grid,
        |st: &ParaProps| new.snap_to_grid != st.snap_to_grid,
        toggle(w, "snapToGrid", new.snap_to_grid)
    );
    // `force_layout`: a paragraph in a table cell, where the table style
    // may say something the paragraph's own style does not.
    let spacing = |p: &ParaProps| (p.space_before, p.space_after, p.line);
    field!(
        "spacing",
        force_layout || spacing(new) != spacing(src),
        |st: &ParaProps| force_layout || spacing(new) != spacing(st),
        spacing_element(w, new)
    );
    // The indents of a list level belong to the list: when the list changes
    // the indents are written out, so that they stay what they are.
    let indents = |p: &ParaProps| (p.indent_left, p.indent_right, p.first_line);
    field!(
        "ind",
        force_layout || indents(new) != indents(src) || new.list != src.list,
        |st: &ParaProps| force_layout || indents(new) != indents(st) || new.list.is_some(),
        ind_element(w, new)
    );
    field!(
        "contextualSpacing",
        new.contextual_spacing != src.contextual_spacing,
        |st: &ParaProps| new.contextual_spacing != st.contextual_spacing,
        toggle(w, "contextualSpacing", new.contextual_spacing)
    );
    field!(
        "jc",
        force_layout || new.align != src.align,
        |st: &ParaProps| force_layout || new.align != st.align,
        val(w, "jc", align_name(new.align))
    );
    field!(
        "outlineLvl",
        new.outline_level != src.outline_level,
        |st: &ParaProps| new.outline_level != st.outline_level,
        val(w, "outlineLvl", new.outline_level.unwrap_or(9))
    );
    if new_mark != src_mark {
        let old_mark = child_text(&ppr, "rPr").map(str::to_owned);
        let rpr = rpr_patch(
            w,
            old_mark.as_deref(),
            src_mark,
            &None,
            new_mark,
            &None,
            base,
        );
        set(&mut ppr, "rPr", rpr);
    }
    match section {
        SectionEdit::Keep => {}
        SectionEdit::Remove => set(&mut ppr, "sectPr", String::new()),
        SectionEdit::Set(xml) => set(&mut ppr, "sectPr", xml),
    }
    if is_bare(&ppr) {
        return None;
    }
    // Nothing but what was there: keep the bytes as they were.
    Some(ppr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look() -> RunLook {
        RunLook::default()
    }

    #[test]
    fn a_run_property_is_set_in_its_place_and_taken_out_again() {
        let base = look();
        let bold = RunLook {
            bold: true,
            ..look()
        };
        let rpr = rpr_patch("w:", None, &base, &None, &bold, &None, &base);
        assert_eq!(rpr, "<w:rPr><w:b/></w:rPr>");
        let sized = RunLook {
            size: 12.0,
            ..bold.clone()
        };
        let rpr = rpr_patch("w:", Some(&rpr), &bold, &None, &sized, &None, &base);
        assert_eq!(rpr, "<w:rPr><w:b/><w:sz w:val=\"24\"/></w:rPr>");
        // Back to the base: the elements go.
        let rpr = rpr_patch("w:", Some(&rpr), &sized, &None, &base, &None, &base);
        assert_eq!(rpr, "");
        // Off where the base has it on is written out.
        let heading = RunLook {
            bold: true,
            ..look()
        };
        let rpr = rpr_patch("w:", None, &heading, &None, &look(), &None, &heading);
        assert_eq!(rpr, "<w:rPr><w:b w:val=\"0\"/></w:rPr>");
    }

    #[test]
    fn unknown_children_stay_and_the_order_is_kept() {
        let old = "<w:rPr><w:b/><w14:ligatures w14:val=\"x\"/></w:rPr>";
        let new = RunLook {
            bold: true,
            italic: true,
            color: Some([255, 0, 0, 255]),
            ..look()
        };
        let src = RunLook {
            bold: true,
            ..look()
        };
        let out = rpr_patch("w:", Some(old), &src, &None, &new, &None, &look());
        assert_eq!(
            out,
            "<w:rPr><w:b/><w:i/><w:color w:val=\"FF0000\"/><w14:ligatures w14:val=\"x\"/></w:rPr>"
        );
    }

    #[test]
    fn paragraph_properties_patch_only_what_differs() {
        let src = ParaProps::default();
        let new = ParaProps {
            align: Align::Center,
            indent_left: 720,
            first_line: -360,
            ..ParaProps::default()
        };
        let old = "<w:pPr><w:pStyle w:val=\"X\"/><w:sectPr/></w:pPr>";
        let out = ppr_patch(
            "w:",
            Some(old),
            &src,
            &new,
            None,
            &look(),
            &look(),
            &look(),
            SectionEdit::Remove,
            false,
        )
        .unwrap();
        assert_eq!(
            out,
            "<w:pPr><w:pStyle w:val=\"X\"/><w:ind w:left=\"720\" w:right=\"0\" w:hanging=\"360\"/><w:jc w:val=\"center\"/></w:pPr>"
        );
        let none = ppr_patch(
            "w:",
            None,
            &src,
            &src,
            None,
            &look(),
            &look(),
            &look(),
            SectionEdit::Keep,
            false,
        );
        assert!(none.is_none());
    }
}
