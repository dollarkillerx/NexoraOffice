//! Drawings in the line and floating: `wp:inline` and `wp:anchor`.
//!
//! A drawing read from the file keeps its element. When its size, its place
//! or how text wraps around it changed, only the parts of the element that
//! say so are written again (the extent, the position, the wrap, the
//! distances, `behindDoc`); everything else, the graphic and whatever is
//! under it (effects, crop, outlines, shape properties) included, is
//! copied byte for byte. Going from the line to floating or back builds
//! the other kind of element from the same parts.

use aster_writer::{Drawing, Float, FloatAt, FloatAxis, FloatFrom, Wrap};

use crate::xml_edit::{
    Span, attribute, children, find_all, inner, prefix_of, qualified_name, remove_attribute,
    set_attribute,
};

/// What the file said about a drawing's size and place, against what the
/// model says now: both, when they differ in a way that is written.
pub(super) fn changed<'d>(
    then: &'d Drawing,
    now: &'d Drawing,
) -> Option<(&'d Drawing, &'d Drawing)> {
    (then.size != now.size || then.float != now.float).then_some((then, now))
}

/// `content` (the children of a run or one `w:drawing`) with the drawing's
/// element written for `now`, which is `then` changed in size or place.
/// Content without such an element (an old VML shape) is returned as it
/// is.
pub(super) fn patch_drawing(content: &str, then: &Drawing, now: &Drawing) -> String {
    let Some(span) = holder(content) else {
        return content.to_owned();
    };
    let element = &content[span.start..span.end];
    let element = if then.size != now.size {
        super::patch_extent(element, then.size, now.size)
    } else {
        element.to_owned()
    };
    let is_anchor = local(&element) == "anchor";
    let written = match (&now.float, is_anchor) {
        (None, false) => element,
        (None, true) => anchor_to_inline(&element),
        (Some(float), true) => patch_anchor(&element, then.float.as_ref(), float),
        (Some(float), false) => inline_to_anchor(&element, float),
    };
    format!(
        "{}{written}{}",
        &content[..span.start],
        &content[span.end..]
    )
}

/// The first `wp:inline` or `wp:anchor` in `content`.
fn holder(content: &str) -> Option<Span> {
    ["inline", "anchor"]
        .iter()
        .filter_map(|name| find_all(content, name).into_iter().next())
        .min_by_key(|span| span.start)
}

fn local(element: &str) -> &str {
    let name = qualified_name(element);
    name.rsplit(':').next().unwrap_or(name)
}

// ---------------------------------------------------------------------------
// Pieces of an anchor
// ---------------------------------------------------------------------------

fn relative_from(from: FloatFrom, horizontal: bool) -> &'static str {
    match (from, horizontal) {
        (FloatFrom::Page, _) => "page",
        (FloatFrom::Margin, _) => "margin",
        (FloatFrom::Column, true) => "column",
        (FloatFrom::Column, false) => "margin",
        (FloatFrom::Text, true) => "character",
        (FloatFrom::Text, false) => "line",
        (FloatFrom::Paragraph, true) => "column",
        (FloatFrom::Paragraph, false) => "paragraph",
    }
}

/// What goes inside `positionH` or `positionV`.
fn axis_content(wp: &str, axis: &FloatAxis, horizontal: bool) -> String {
    match axis.at {
        FloatAt::Offset(offset) => format!("<{wp}posOffset>{offset}</{wp}posOffset>"),
        FloatAt::Start => align(wp, if horizontal { "left" } else { "top" }),
        FloatAt::Center => align(wp, "center"),
        FloatAt::End => align(wp, if horizontal { "right" } else { "bottom" }),
    }
}

fn align(wp: &str, word: &str) -> String {
    format!("<{wp}align>{word}</{wp}align>")
}

fn axis_xml(wp: &str, axis: &FloatAxis, horizontal: bool) -> String {
    let name = if horizontal { "positionH" } else { "positionV" };
    format!(
        "<{wp}{name} relativeFrom=\"{}\">{}</{wp}{name}>",
        relative_from(axis.from, horizontal),
        axis_content(wp, axis, horizontal)
    )
}

/// The wrap element, with the polygon a tight wrap needs: the frame.
pub(super) fn wrap_xml(wp: &str, wrap: Wrap) -> String {
    match wrap {
        Wrap::None => format!("<{wp}wrapNone/>"),
        Wrap::Square => format!("<{wp}wrapSquare wrapText=\"bothSides\"/>"),
        Wrap::TopAndBottom => format!("<{wp}wrapTopAndBottom/>"),
        Wrap::Tight => format!(
            "<{wp}wrapTight wrapText=\"bothSides\"><{wp}wrapPolygon edited=\"0\">\
<{wp}start x=\"0\" y=\"0\"/><{wp}lineTo x=\"0\" y=\"21600\"/><{wp}lineTo x=\"21600\" y=\"21600\"/>\
<{wp}lineTo x=\"21600\" y=\"0\"/><{wp}lineTo x=\"0\" y=\"0\"/></{wp}wrapPolygon></{wp}wrapTight>"
        ),
    }
}

const WRAPS: [&str; 5] = [
    "wrapNone",
    "wrapSquare",
    "wrapTight",
    "wrapThrough",
    "wrapTopAndBottom",
];

/// The start tag of `wp:anchor` for `float`; `extra` are attributes of an
/// element it is made from, written as `" name=\"value\""`.
fn anchor_open(wp: &str, float: &Float, extra: &str) -> String {
    let [left, top, right, bottom] = float.distance;
    format!(
        "<{wp}anchor distT=\"{top}\" distB=\"{bottom}\" distL=\"{left}\" distR=\"{right}\" \
simplePos=\"0\" relativeHeight=\"251658240\" behindDoc=\"{}\" locked=\"0\" layoutInCell=\"1\" \
allowOverlap=\"1\"{extra}>",
        u8::from(float.behind)
    )
}

/// A whole `wp:anchor` for a drawing made here: `extent` and `effect`
/// (the extent elements) and `tail` (`docPr`, `cNvGraphicFramePr` and the
/// graphic).
pub(super) fn anchor_xml(wp: &str, float: &Float, extent: &str, tail: &str) -> String {
    format!(
        "{}<{wp}simplePos x=\"0\" y=\"0\"/>{}{}{extent}{}{tail}</{wp}anchor>",
        anchor_open(wp, float, ""),
        axis_xml(wp, &float.x, true),
        axis_xml(wp, &float.y, false),
        wrap_xml(wp, float.wrap)
    )
}

// ---------------------------------------------------------------------------
// Changing an element
// ---------------------------------------------------------------------------

/// The attributes of the start tag of `element`, as `" name=\"value\""`,
/// without the ones named.
fn attributes_without(element: &str, names: &[&str]) -> String {
    let mut element = element.to_owned();
    for name in names {
        element = remove_attribute(&element, name);
    }
    let (open, _) = inner(&element);
    let tag = &element[..open];
    let rest = &tag[1 + qualified_name(&element).len()..];
    rest.trim_end_matches('>').trim_end_matches('/').to_owned()
}

/// The text of the child of `element` named `name`.
fn child<'e>(element: &'e str, name: &str) -> Option<&'e str> {
    children(element)
        .into_iter()
        .find(|c| c.name == name)
        .map(|c| &element[c.start..c.end])
}

const DEFAULT_EFFECT: &str = "effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"";

/// A `wp:inline` for the parts of a `wp:anchor` that stay, or a `wp:anchor`
/// for those of a `wp:inline` (`new` says which; the parts are the extent,
/// the effect extent, the document properties, the frame locks and the
/// graphic). Attributes the old element had that belong to both (extension
/// ids) stay.
fn inline_to_anchor(element: &str, float: &Float) -> String {
    let wp = prefix_of(element);
    let attrs = attributes_without(element, &["distT", "distB", "distL", "distR"]);
    let extent = child(element, "extent").unwrap_or("").to_owned();
    let effect = child(element, "effectExtent")
        .map_or_else(|| format!("<{wp}{DEFAULT_EFFECT}/>"), str::to_owned);
    let tail: String = ["docPr", "cNvGraphicFramePr", "graphic"]
        .iter()
        .filter_map(|name| child(element, name))
        .collect();
    format!(
        "{}<{wp}simplePos x=\"0\" y=\"0\"/>{}{}{extent}{effect}{}{tail}</{wp}anchor>",
        anchor_open(&wp, float, &attrs),
        axis_xml(&wp, &float.x, true),
        axis_xml(&wp, &float.y, false),
        wrap_xml(&wp, float.wrap)
    )
}

fn anchor_to_inline(element: &str) -> String {
    let wp = prefix_of(element);
    let attrs = attributes_without(
        element,
        &[
            "distT",
            "distB",
            "distL",
            "distR",
            "simplePos",
            "relativeHeight",
            "behindDoc",
            "locked",
            "layoutInCell",
            "allowOverlap",
        ],
    );
    let parts: String = [
        "extent",
        "effectExtent",
        "docPr",
        "cNvGraphicFramePr",
        "graphic",
    ]
    .iter()
    .filter_map(|name| child(element, name))
    .collect();
    format!(
        "<{wp}inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\"{attrs}>{parts}</{wp}inline>"
    )
}

/// The `wp:anchor` with the parts that differ between `then` and `now`
/// written again. Where `then` is not known everything is.
fn patch_anchor(element: &str, then: Option<&Float>, now: &Float) -> String {
    let wp = prefix_of(element);
    let mut out = element.to_owned();
    if then.is_none_or(|t| t.behind != now.behind) {
        out = set_attribute(&out, "behindDoc", if now.behind { "1" } else { "0" });
    }
    if then.is_none_or(|t| t.distance != now.distance) {
        let [left, top, right, bottom] = now.distance;
        for (name, value) in [
            ("distT", top),
            ("distB", bottom),
            ("distL", left),
            ("distR", right),
        ] {
            out = set_attribute(&out, name, &value.to_string());
        }
    }
    if then.is_none_or(|t| t.x != now.x) {
        out = replace_axis(&out, &wp, &now.x, true, then.map(|t| t.x.from));
    }
    if then.is_none_or(|t| t.y != now.y) {
        out = replace_axis(&out, &wp, &now.y, false, then.map(|t| t.y.from));
    }
    if then.is_none_or(|t| t.wrap != now.wrap) {
        out = replace_wrap(&out, &wp, now.wrap);
    }
    out
}

/// `positionH` or `positionV` for `axis`. Where the reference is the one
/// the file had, its start tag stays as written (the file may say
/// `leftMargin` where the model says page).
fn replace_axis(
    element: &str,
    wp: &str,
    axis: &FloatAxis,
    horizontal: bool,
    was: Option<FloatFrom>,
) -> String {
    let name = if horizontal { "positionH" } else { "positionV" };
    let found = children(element).into_iter().find(|c| c.name == name);
    let new = match (&found, was) {
        (Some(old), Some(from)) if from == axis.from => {
            let text = &element[old.start..old.end];
            let (open, close) = inner(text);
            if text[..open].ends_with("/>") {
                axis_xml(wp, axis, horizontal)
            } else {
                format!(
                    "{}{}{}",
                    &text[..open],
                    axis_content(wp, axis, horizontal),
                    &text[close..]
                )
            }
        }
        _ => axis_xml(wp, axis, horizontal),
    };
    put(
        element,
        found,
        &new,
        if horizontal {
            &["positionV", "extent"]
        } else {
            &["extent"]
        },
    )
}

fn replace_wrap(element: &str, wp: &str, wrap: Wrap) -> String {
    let found = children(element)
        .into_iter()
        .find(|c| WRAPS.contains(&c.name.as_str()));
    put(
        element,
        found,
        &wrap_xml(wp, wrap),
        &["docPr", "cNvGraphicFramePr", "graphic"],
    )
}

/// `new` in place of the child `found`, or before the first child named in
/// `before` (at the end of the element if none).
fn put(element: &str, found: Option<Span>, new: &str, before: &[&str]) -> String {
    if let Some(old) = found {
        return format!("{}{new}{}", &element[..old.start], &element[old.end..]);
    }
    let at = children(element)
        .into_iter()
        .find(|c| before.contains(&c.name.as_str()))
        .map_or_else(|| inner(element).1, |c| c.start);
    format!("{}{new}{}", &element[..at], &element[at..])
}

/// The attribute `name` of the first element in `element` that has it:
/// for finding a picture's relationship id.
pub(super) fn first_attribute(
    element: &str,
    local_name: &str,
    attribute_name: &str,
) -> Option<String> {
    let span = find_all(element, local_name).into_iter().next()?;
    attribute(&element[span.start..span.end], attribute_name)
}
