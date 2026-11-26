//! Pictures, text boxes and other graphics: DrawingML in `w:drawing` and
//! the older VML in `w:pict` and `w:object`.

use std::sync::Arc;

use aster_writer::{
    Border, BorderStyle, Drawing, DrawingContent, Float, FloatAt, FloatAxis, FloatFrom, Picture,
    Rgba, TextBox, Wrap,
};

use super::dom::Node;
use super::{PartState, Reader};
use crate::drawingml::{Modifier, apply, parse_hex};
use crate::error::Error;

/// Text boxes inside text boxes are read this deep.
const MAX_BOX_DEPTH: usize = 6;
/// The largest size taken at its word: about 220 metres.
const MAX_EMU: i64 = 200_000_000;
const EMU_PER_POINT: f64 = 12_700.0;

fn emu(value: Option<i64>) -> i64 {
    value.unwrap_or(0).clamp(0, MAX_EMU)
}

/// The first element under `node` for which `found` is true, to `depth`
/// levels.
fn descendant<'n, 'a>(
    node: &'n Node<'a>,
    depth: usize,
    found: &dyn Fn(&Node<'a>) -> bool,
) -> Option<&'n Node<'a>> {
    for child in &node.children {
        if found(child) {
            return Some(child);
        }
        if depth > 0
            && let Some(hit) = descendant(child, depth - 1, found)
        {
            return Some(hit);
        }
    }
    None
}

impl Reader<'_> {
    /// A colour element's colour (`srgbClr`, `schemeClr`, `sysClr`).
    fn drawing_color(&self, node: &Node<'_>) -> Option<Rgba> {
        let base = match node.local() {
            b"srgbClr" => node.attr("val").as_deref().and_then(parse_hex)?,
            b"schemeClr" => {
                let name = node.attr("val")?;
                let name = match name.as_str() {
                    "tx1" => "dk1",
                    "bg1" => "lt1",
                    "tx2" => "dk2",
                    "bg2" => "lt2",
                    other => other,
                };
                self.env.theme.color(name)
            }
            b"sysClr" => node
                .attr("lastClr")
                .as_deref()
                .and_then(parse_hex)
                .unwrap_or([0, 0, 0, 255]),
            _ => return None,
        };
        let percent = |c: &Node<'_>| c.float("val").unwrap_or(0.0) / 100_000.0;
        let modifiers: Vec<Modifier> = node
            .children
            .iter()
            .filter_map(|c| match c.local() {
                b"lumMod" => Some(Modifier::LumMod(percent(c))),
                b"lumOff" => Some(Modifier::LumOff(percent(c))),
                b"tint" => Some(Modifier::Tint(percent(c))),
                b"shade" => Some(Modifier::Shade(percent(c))),
                _ => None,
            })
            .collect();
        Some(apply(base, &modifiers))
    }

    fn solid(&self, node: &Node<'_>) -> Option<Rgba> {
        node.child("solidFill")?
            .children
            .first()
            .and_then(|c| self.drawing_color(c))
    }

    /// The bytes of the image part a relationship id names, read once.
    fn image(&mut self, part: &PartState, id: &str) -> Option<Arc<[u8]>> {
        let target = part.rels.get(id).and_then(|r| r.target.clone())?;
        if let Some(found) = self.images.get(&target) {
            return Some(found.clone());
        }
        if !self.package.has(&target) {
            return None;
        }
        match self.package.read(&target) {
            Ok(bytes) => {
                let bytes: Arc<[u8]> = Arc::from(bytes);
                self.images.insert(target, bytes.clone());
                Some(bytes)
            }
            Err(error) => {
                // A picture over a limit fails the whole file, as for
                // presentations; one that cannot be inflated is left out.
                if matches!(error, Error::TooLarge(_)) {
                    self.error.get_or_insert(error);
                }
                None
            }
        }
    }

    /// `w:drawing`.
    pub(super) fn drawing(&mut self, node: &Node<'_>, part: &mut PartState) -> Drawing {
        let anchor = node.child("anchor");
        let Some(holder) = anchor.or_else(|| node.child("inline")) else {
            return other(0, 0, "shape");
        };
        let size = holder
            .child("extent")
            .map_or((0, 0), |e| (emu(e.int("cx")), emu(e.int("cy"))));
        let alt = holder
            .child("docPr")
            .and_then(|d| {
                d.attr("descr")
                    .filter(|s| !s.is_empty())
                    .or(d.attr("title"))
            })
            .filter(|s| !s.is_empty());
        let float = anchor.map(float_of);
        let data = holder.child("graphic").and_then(|g| g.child("graphicData"));
        let content = match data.and_then(|d| d.children.first()) {
            Some(content) => self.drawing_content(content, part),
            None => DrawingContent::Other("shape".to_owned()),
        };
        Drawing {
            size,
            content,
            float,
            alt,
        }
    }

    fn drawing_content(&mut self, node: &Node<'_>, part: &mut PartState) -> DrawingContent {
        match node.local() {
            b"pic" => self.picture(node, part),
            b"wsp" => match node.child("txbx").and_then(|t| t.child("txbxContent")) {
                Some(content) => self.text_box(content, node, part),
                None => DrawingContent::Other("shape".to_owned()),
            },
            b"wgp" | b"lockedCanvas" => DrawingContent::Other("group".to_owned()),
            b"chart" | b"chartex" => DrawingContent::Other("chart".to_owned()),
            b"relIds" => DrawingContent::Other("diagram".to_owned()),
            _ => DrawingContent::Other("shape".to_owned()),
        }
    }

    fn picture(&mut self, node: &Node<'_>, part: &mut PartState) -> DrawingContent {
        let Some(fill) = node.child("blipFill") else {
            return DrawingContent::Other("picture".to_owned());
        };
        let embed = fill.child("blip").and_then(|b| b.attr("embed"));
        let Some(image) = embed.and_then(|id| self.image(part, &id)) else {
            return DrawingContent::Other("picture".to_owned());
        };
        let mut crop = [0.0; 4];
        if let Some(src) = fill.child("srcRect") {
            for (slot, name) in crop.iter_mut().zip(["l", "t", "r", "b"]) {
                *slot = (src.float(name).unwrap_or(0.0) / 100_000.0).clamp(0.0, 1.0);
            }
            // Cropping away everything is a damaged file.
            if crop[0] + crop[2] >= 1.0 || crop[1] + crop[3] >= 1.0 {
                crop = [0.0; 4];
            }
        }
        DrawingContent::Picture(Picture { image, crop })
    }

    /// A text box: `content` is its `txbxContent`, `shape` the element with
    /// its fill and line.
    fn text_box(
        &mut self,
        content: &Node<'_>,
        shape: &Node<'_>,
        part: &mut PartState,
    ) -> DrawingContent {
        let blocks = if part.depth < MAX_BOX_DEPTH {
            part.depth += 1;
            let blocks = self.blocks(&content.children, part);
            part.depth -= 1;
            blocks
        } else {
            Vec::new()
        };
        let mut insets = [91_440, 45_720, 91_440, 45_720];
        if let Some(body) = shape.child("bodyPr") {
            for (slot, name) in insets.iter_mut().zip(["lIns", "tIns", "rIns", "bIns"]) {
                if let Some(v) = body.int(name) {
                    *slot = v.clamp(0, MAX_EMU);
                }
            }
        }
        let props = shape.child("spPr");
        let fill = props.and_then(|p| self.solid(p));
        let line = props.and_then(|p| p.child("ln")).and_then(|ln| {
            if ln.child("noFill").is_some() {
                return None;
            }
            let color = self.solid(ln);
            let width = ln.int("w").map_or(9_525, |w| w.clamp(0, MAX_EMU));
            (color.is_some() || ln.int("w").is_some()).then_some(Border {
                style: BorderStyle::Single,
                width: (width as f32 / EMU_PER_POINT as f32).max(0.25),
                color,
                space: 0.0,
            })
        });
        DrawingContent::TextBox(TextBox {
            blocks,
            fill,
            line,
            insets,
        })
    }

    /// `w:pict` and `w:object`: the old VML shapes.
    pub(super) fn vml(&mut self, node: &Node<'_>, part: &mut PartState) -> Drawing {
        let shape = descendant(node, 3, &|n| n.attr("style").is_some() && is_vml_shape(n));
        let style = shape.and_then(|s| s.attr("style")).unwrap_or_default();
        let mut size = (
            css_length(&style, "width").unwrap_or(0),
            css_length(&style, "height").unwrap_or(0),
        );
        if size == (0, 0)
            && node.is("object")
            && let (Some(w), Some(h)) = (node.int("dxaOrig"), node.int("dyaOrig"))
        {
            size = (emu(Some(w * 635)), emu(Some(h * 635)));
        }
        let float = style
            .contains("position:absolute")
            .then(|| vml_float(&style, node));
        let alt = shape
            .and_then(|s| s.attr("alt"))
            .filter(|alt| !alt.is_empty());
        let content = if let Some(content) = descendant(node, 5, &|n| n.is("txbxContent")) {
            let fill = shape
                .and_then(|s| s.attr("fillcolor"))
                .and_then(|c| parse_hex(c.trim_start_matches('#')));
            let stroked = shape
                .and_then(|s| s.attr("stroked"))
                .is_none_or(|v| !matches!(v.as_str(), "f" | "false" | "0"));
            let line = stroked.then(|| Border {
                style: BorderStyle::Single,
                width: shape
                    .and_then(|s| s.attr("strokeweight"))
                    .and_then(|w| css_length(&format!("w:{w}"), "w"))
                    .map_or(0.75, |w| (w as f32 / EMU_PER_POINT as f32).max(0.25)),
                color: shape
                    .and_then(|s| s.attr("strokecolor"))
                    .and_then(|c| parse_hex(c.trim_start_matches('#'))),
                space: 0.0,
            });
            let blocks = if part.depth < MAX_BOX_DEPTH {
                part.depth += 1;
                let blocks = self.blocks(&content.children, part);
                part.depth -= 1;
                blocks
            } else {
                Vec::new()
            };
            DrawingContent::TextBox(TextBox {
                blocks,
                fill,
                line,
                insets: [91_440, 45_720, 91_440, 45_720],
            })
        } else if node.is("pict")
            && let Some(id) = descendant(node, 3, &|n| n.is("imagedata")).and_then(|d| {
                d.attrs("id")
                    .into_iter()
                    .find(|id| part.rels.contains_key(id))
            })
            && let Some(image) = self.image(part, &id)
        {
            DrawingContent::Picture(Picture {
                image,
                crop: [0.0; 4],
            })
        } else {
            DrawingContent::Other("object".to_owned())
        };
        Drawing {
            size,
            content,
            float,
            alt,
        }
    }
}

fn other(w: i64, h: i64, what: &str) -> Drawing {
    Drawing {
        size: (w, h),
        content: DrawingContent::Other(what.to_owned()),
        float: None,
        alt: None,
    }
}

fn is_vml_shape(node: &Node<'_>) -> bool {
    matches!(
        node.local(),
        b"shape" | b"rect" | b"roundrect" | b"oval" | b"line" | b"group" | b"shapetype"
    ) && !node.is("shapetype")
}

/// A CSS length in a style string (`width:12.5pt`), as EMU.
fn css_length(style: &str, name: &str) -> Option<i64> {
    let value = style
        .split(';')
        .filter_map(|decl| decl.split_once(':'))
        .find(|(key, _)| key.trim() == name)?
        .1
        .trim();
    let digits = value
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .unwrap_or(value.len());
    let number: f64 = value[..digits].parse().ok()?;
    let per_unit = match value[digits..].trim() {
        "pt" => EMU_PER_POINT,
        "in" => 914_400.0,
        "cm" => 360_000.0,
        "mm" => 36_000.0,
        "pc" => 152_400.0,
        "" | "px" => 9_525.0,
        _ => return None,
    };
    Some(((number * per_unit).round() as i64).clamp(-MAX_EMU, MAX_EMU))
}

fn style_word(style: &str, name: &str) -> Option<String> {
    style
        .split(';')
        .filter_map(|decl| decl.split_once(':'))
        .find(|(key, _)| key.trim() == name)
        .map(|(_, value)| value.trim().to_owned())
}

fn vml_float(style: &str, node: &Node<'_>) -> Float {
    let from_h = match style_word(style, "mso-position-horizontal-relative").as_deref() {
        Some("page") => FloatFrom::Page,
        Some("margin") => FloatFrom::Margin,
        Some("char") => FloatFrom::Text,
        _ => FloatFrom::Column,
    };
    let from_v = match style_word(style, "mso-position-vertical-relative").as_deref() {
        Some("page") => FloatFrom::Page,
        Some("margin") => FloatFrom::Margin,
        Some("line") => FloatFrom::Text,
        _ => FloatFrom::Paragraph,
    };
    let align =
        |word: Option<String>, offset: Option<i64>, start: &str, end: &str| match word.as_deref() {
            Some(w) if w == start || w == "inside" => FloatAt::Start,
            Some("center") => FloatAt::Center,
            Some(w) if w == end || w == "outside" => FloatAt::End,
            _ => FloatAt::Offset(offset.unwrap_or(0)),
        };
    let wrap = descendant(node, 3, &|n| n.is("wrap"))
        .and_then(|w| w.attr("type"))
        .map_or(Wrap::None, |t| match t.as_str() {
            "square" => Wrap::Square,
            "tight" | "through" => Wrap::Tight,
            "topAndBottom" => Wrap::TopAndBottom,
            _ => Wrap::None,
        });
    let behind = style_word(style, "z-index")
        .and_then(|z| z.parse::<i64>().ok())
        .is_some_and(|z| z < 0);
    Float {
        x: FloatAxis {
            from: from_h,
            at: align(
                style_word(style, "mso-position-horizontal"),
                css_length(style, "margin-left"),
                "left",
                "right",
            ),
        },
        y: FloatAxis {
            from: from_v,
            at: align(
                style_word(style, "mso-position-vertical"),
                css_length(style, "margin-top"),
                "top",
                "bottom",
            ),
        },
        wrap,
        behind,
        distance: [0; 4],
    }
}

/// The position of a `wp:anchor`.
fn float_of(anchor: &Node<'_>) -> Float {
    let axis = |name: &str, horizontal: bool| {
        let node = anchor.child(name);
        let from = match node.and_then(|n| n.attr("relativeFrom")).as_deref() {
            Some("page" | "leftMargin" | "topMargin") => FloatFrom::Page,
            Some("column") => FloatFrom::Column,
            Some("character") => FloatFrom::Text,
            Some("paragraph") => FloatFrom::Paragraph,
            Some("line") => FloatFrom::Text,
            // `margin`, `insideMargin`, `outsideMargin`, `rightMargin`,
            // `bottomMargin`
            _ => FloatFrom::Margin,
        };
        let from = match (horizontal, from) {
            (true, FloatFrom::Paragraph) => FloatFrom::Column,
            (false, FloatFrom::Column) => FloatFrom::Margin,
            (_, other) => other,
        };
        let at = match node.and_then(|n| n.child("align")).map(|a| a.text.trim()) {
            Some("left" | "top" | "inside") => FloatAt::Start,
            Some("center") => FloatAt::Center,
            Some("right" | "bottom" | "outside") => FloatAt::End,
            _ => FloatAt::Offset(
                node.and_then(|n| n.child("posOffset"))
                    .and_then(|o| o.text.trim().parse::<i64>().ok())
                    .unwrap_or(0)
                    .clamp(-MAX_EMU, MAX_EMU),
            ),
        };
        FloatAxis { from, at }
    };
    let wrap = anchor
        .children
        .iter()
        .find_map(|c| match c.local() {
            b"wrapNone" => Some(Wrap::None),
            b"wrapSquare" => Some(Wrap::Square),
            b"wrapTight" | b"wrapThrough" => Some(Wrap::Tight),
            b"wrapTopAndBottom" => Some(Wrap::TopAndBottom),
            _ => None,
        })
        .unwrap_or(Wrap::None);
    let distance = ["distL", "distT", "distR", "distB"].map(|name| emu(anchor.int(name)));
    Float {
        x: axis("positionH", true),
        y: axis("positionV", false),
        wrap,
        behind: anchor
            .attr("behindDoc")
            .is_some_and(|v| matches!(v.trim(), "1" | "true" | "on")),
        distance,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_lengths() {
        let style = "position:absolute;margin-left:10pt;width:2in;height:1cm;z-index:-5";
        assert_eq!(css_length(style, "margin-left"), Some(127_000));
        assert_eq!(css_length(style, "width"), Some(1_828_800));
        assert_eq!(css_length(style, "height"), Some(360_000));
        assert_eq!(css_length(style, "top"), None);
    }
}
