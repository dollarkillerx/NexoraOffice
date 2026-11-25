//! Conditional formats: `<conditionalFormatting>` of a worksheet and the
//! `<dxfs>` of the styles they point at. Read for display; the worksheet
//! part is written back as it was.
//!
//! Not read: the `x14` rules in a worksheet's `extLst` (data bars with a
//! negative side, icon sets of the newer kinds, rules over other sheets'
//! ranges). They stay in the file.

use std::sync::Arc;

use aster_sheets::cond::{
    Cfvo, CfvoKind, Compare, CondFormat, Dxf, IconSet, Kind, MAX_STEPS, Rule, TextTest,
};
use aster_sheets::{CellRange, Format, Rgba};

use super::{Colors, read_border};
use crate::error::Result;
use crate::xml::{Tag, XmlReader, each_child};

/// Most conditional formats read for one sheet.
pub(super) const MAX_FORMATS: usize = 50_000;
/// Most rules read for one block of ranges.
const MAX_RULES: usize = 256;
/// Most ranges of one block.
const MAX_RANGES: usize = 2_000;
/// Longest formula or text kept, characters. A longer one is not drawn.
const MAX_TEXT: usize = 8_192;
/// Most differential formats read.
const MAX_DXFS: usize = 65_536;

/// `<dxfs>`: after its `Open`, the formats by index.
pub(super) fn read_dxfs(
    reader: &mut XmlReader<'_>,
    colors: &Colors<'_>,
    custom_formats: &std::collections::HashMap<u32, String>,
) -> Result<Vec<Arc<Dxf>>> {
    let mut dxfs = Vec::new();
    each_child(reader, |reader, child| {
        if !child.is("dxf") || dxfs.len() >= MAX_DXFS {
            return reader.skip();
        }
        let mut dxf = Dxf::default();
        each_child(reader, |reader, part| match part.local() {
            b"font" => each_child(reader, |reader, f| {
                let on = || Some(f.attr_bool("val").unwrap_or(true));
                match f.local() {
                    b"b" => dxf.bold = on(),
                    b"i" => dxf.italic = on(),
                    b"strike" => dxf.strike = on(),
                    b"u" => dxf.underline = Some(f.attr("val").as_deref() != Some("none")),
                    b"color" => dxf.font_color = colors.of(&f),
                    _ => {}
                }
                reader.skip()
            }),
            b"fill" => {
                dxf.fill = read_dxf_fill(reader, colors)?;
                Ok(())
            }
            b"border" => {
                dxf.borders = read_border(reader, colors)?;
                Ok(())
            }
            b"numFmt" => {
                if let Some(code) = part.attr("formatCode") {
                    dxf.format = Some(Arc::new(Format::parse(&code)));
                } else if let Some(id) = part.attr_i64("numFmtId") {
                    let id = id.clamp(0, i64::from(u32::MAX)) as u32;
                    dxf.format = Some(Arc::new(match custom_formats.get(&id) {
                        Some(code) => Format::parse(code),
                        None => Format::builtin(id),
                    }));
                }
                reader.skip()
            }
            _ => reader.skip(),
        })?;
        dxfs.push(Arc::new(dxf));
        Ok(())
    })?;
    Ok(dxfs)
}

/// A differential fill. A solid pattern shows its background colour here
/// (the other way round from a cell's), and a pattern without a type is
/// solid.
fn read_dxf_fill(reader: &mut XmlReader<'_>, colors: &Colors<'_>) -> Result<Option<Rgba>> {
    let mut fill = None;
    each_child(reader, |reader, child| {
        if !child.is("patternFill") {
            return reader.skip();
        }
        let pattern = child.attr("patternType");
        let (mut fg, mut bg) = (None, None);
        each_child(reader, |reader, color| {
            if color.is("fgColor") {
                fg = colors.of(&color);
            } else if color.is("bgColor") {
                bg = colors.of(&color);
            }
            reader.skip()
        })?;
        fill = match pattern.as_deref() {
            Some("none") => None,
            Some("solid") | None => bg.or(fg),
            // Other patterns are drawn as their foreground colour.
            Some(_) => fg.or(bg),
        };
        Ok(())
    })?;
    Ok(fill)
}

/// After the `Open` of `<conditionalFormatting>`: its ranges and rules.
pub(super) fn read_format(
    reader: &mut XmlReader<'_>,
    tag: &Tag<'_>,
    dxfs: &[Arc<Dxf>],
    colors: &Colors<'_>,
) -> Result<Option<CondFormat>> {
    let ranges: Vec<CellRange> = tag
        .attr("sqref")
        .unwrap_or_default()
        .split_whitespace()
        .take(MAX_RANGES)
        .filter_map(CellRange::parse)
        .collect();
    let mut rules = Vec::new();
    each_child(reader, |reader, child| {
        if child.is("cfRule") && rules.len() < MAX_RULES {
            rules.push(read_rule(reader, &child, dxfs, colors)?);
            Ok(())
        } else {
            reader.skip()
        }
    })?;
    Ok((!ranges.is_empty() && !rules.is_empty()).then_some(CondFormat { ranges, rules }))
}

fn cfvo(tag: &Tag<'_>) -> Option<Cfvo> {
    let kind = match tag.attr("type")?.as_str() {
        "num" => CfvoKind::Num,
        "percent" => CfvoKind::Percent,
        "percentile" => CfvoKind::Percentile,
        "min" => CfvoKind::Min,
        "max" => CfvoKind::Max,
        "formula" => CfvoKind::Formula,
        _ => return None,
    };
    let value = tag.attr("val").unwrap_or_default();
    if value.chars().count() > MAX_TEXT {
        return None;
    }
    Some(Cfvo {
        kind,
        value,
        gte: tag.attr_bool("gte").unwrap_or(true),
    })
}

fn read_rule(
    reader: &mut XmlReader<'_>,
    tag: &Tag<'_>,
    dxfs: &[Arc<Dxf>],
    colors: &Colors<'_>,
) -> Result<Rule> {
    let mut formulas: Vec<String> = Vec::new();
    let mut values: Vec<Cfvo> = Vec::new();
    let mut shades: Vec<Rgba> = Vec::new();
    let mut spec: Option<Tag<'_>> = None;
    // A cfvo or colour that does not parse makes the whole rule unusable.
    let mut broken = false;
    each_child(reader, |reader, child| {
        match child.local() {
            b"formula" => {
                let text = reader.text()?;
                formulas.push(text.trim().trim_start_matches('=').to_owned());
                Ok(())
            }
            b"colorScale" | b"dataBar" | b"iconSet" => {
                each_child(reader, |reader, item| {
                    match item.local() {
                        b"cfvo" if values.len() < MAX_STEPS => match cfvo(&item) {
                            Some(value) => values.push(value),
                            None => broken = true,
                        },
                        b"color" => match colors.of(&item) {
                            Some(color) => shades.push(color),
                            // An automatic colour reads as black.
                            None => shades.push([0, 0, 0, 0xff]),
                        },
                        _ => {}
                    }
                    reader.skip()
                })?;
                spec = Some(child);
                Ok(())
            }
            _ => reader.skip(),
        }
    })?;
    let kind_name = tag.attr("type").unwrap_or_default();
    let formula = |i: usize| formulas.get(i).filter(|f| f.chars().count() <= MAX_TEXT);
    let text = tag.attr("text").filter(|t| t.chars().count() <= MAX_TEXT);
    let kind = if broken {
        Kind::Unsupported(kind_name.clone())
    } else {
        match kind_name.as_str() {
            "cellIs" => match (
                tag.attr("operator").as_deref().and_then(Compare::parse),
                formula(0),
            ) {
                (Some(op), Some(first)) => Kind::CellIs {
                    op,
                    first: Arc::from(first.as_str()),
                    second: formula(1).map(|f| Arc::from(f.as_str())),
                },
                _ => Kind::Unsupported(kind_name.clone()),
            },
            "expression" => match formula(0) {
                Some(f) => Kind::Expression(Arc::from(f.as_str())),
                None => Kind::Unsupported(kind_name.clone()),
            },
            "containsText" | "notContainsText" | "beginsWith" | "endsWith" => {
                let test = match kind_name.as_str() {
                    "containsText" => TextTest::Contains,
                    "notContainsText" => TextTest::NotContains,
                    "beginsWith" => TextTest::Begins,
                    _ => TextTest::Ends,
                };
                match text {
                    Some(text) => Kind::Text {
                        test,
                        text: Arc::from(text.as_str()),
                    },
                    None => Kind::Unsupported(kind_name.clone()),
                }
            }
            "containsBlanks" => Kind::Blanks(true),
            "notContainsBlanks" => Kind::Blanks(false),
            "containsErrors" => Kind::Errors(true),
            "notContainsErrors" => Kind::Errors(false),
            "duplicateValues" => Kind::Duplicates { unique: false },
            "uniqueValues" => Kind::Duplicates { unique: true },
            "top10" => Kind::Top {
                rank: tag.attr_i64("rank").unwrap_or(10).clamp(1, 1_000_000) as u32,
                percent: tag.attr_bool("percent").unwrap_or(false),
                bottom: tag.attr_bool("bottom").unwrap_or(false),
            },
            "aboveAverage" => Kind::Average {
                above: tag.attr_bool("aboveAverage").unwrap_or(true),
                equal: tag.attr_bool("equalAverage").unwrap_or(false),
                std_dev: tag.attr_i64("stdDev").unwrap_or(0).clamp(0, 3) as u32,
            },
            "colorScale" if (2..=3).contains(&values.len()) && shades.len() >= values.len() => {
                Kind::ColorScale(values.iter().cloned().zip(shades.iter().copied()).collect())
            }
            "dataBar" if values.len() == 2 && !shades.is_empty() => {
                let bar = spec.as_ref();
                let length = |name: &str, default: i64| {
                    bar.and_then(|b| b.attr_i64(name))
                        .unwrap_or(default)
                        .clamp(0, 100) as u8
                };
                Kind::DataBar {
                    min: values[0].clone(),
                    max: values[1].clone(),
                    color: shades[0],
                    min_length: length("minLength", 10),
                    max_length: length("maxLength", 90),
                    show_value: bar.and_then(|b| b.attr_bool("showValue")).unwrap_or(true),
                }
            }
            "iconSet" => {
                let set = spec
                    .as_ref()
                    .map(|s| {
                        s.attr("iconSet")
                            .unwrap_or_else(|| "3TrafficLights1".to_owned())
                    })
                    .and_then(|name| IconSet::parse(&name));
                match set {
                    Some(set) if values.len() == usize::from(set.count) => Kind::IconSet {
                        set,
                        steps: values,
                        reverse: spec
                            .as_ref()
                            .and_then(|s| s.attr_bool("reverse"))
                            .unwrap_or(false),
                        show_value: spec
                            .as_ref()
                            .and_then(|s| s.attr_bool("showValue"))
                            .unwrap_or(true),
                    },
                    _ => Kind::Unsupported(kind_name.clone()),
                }
            }
            _ => Kind::Unsupported(kind_name.clone()),
        }
    };
    Ok(Rule {
        priority: tag
            .attr_i64("priority")
            .unwrap_or(i64::from(i32::MAX))
            .clamp(0, i64::from(i32::MAX)) as i32,
        stop_if_true: tag.attr_bool("stopIfTrue").unwrap_or(false),
        kind,
        dxf: tag
            .attr_i64("dxfId")
            .and_then(|id| usize::try_from(id).ok())
            .and_then(|id| dxfs.get(id).cloned()),
    })
}
