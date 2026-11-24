//! Rows and columns inserted and deleted, replayed on the file's own XML.
//!
//! Each change is applied to the worksheet as it was written: rows and
//! cells keep their bytes and get new `r` attributes, deleted ones go, and
//! the ranges other worksheet elements name (the used range, the
//! selection, conditional formats, validations, links, filters, page
//! breaks) move with the cells. What a sheet holds that is not moved here
//! makes [`blocker`] refuse the change before it is made.

use aster_sheets::structure::{Lines, adjust};
use aster_sheets::{CellRange, CellRef};
use quick_xml::Reader;
use quick_xml::events::Event;

use super::{children, escape, scan, set_attribute, split_bom, start_tag, top_level, unescape};
use crate::error::Result;

/// The worksheet children whose references are moved here, or that hold
/// none.
const HANDLED: [&str; 27] = [
    "sheetPr",
    "dimension",
    "sheetViews",
    "sheetFormatPr",
    "cols",
    "sheetData",
    "sheetCalcPr",
    "sheetProtection",
    "mergeCells",
    "phoneticPr",
    "conditionalFormatting",
    "dataValidations",
    "hyperlinks",
    "printOptions",
    "pageMargins",
    "pageSetup",
    "headerFooter",
    "customProperties",
    "rowBreaks",
    "colBreaks",
    "ignoredErrors",
    "drawing",
    "legacyDrawingHF",
    "drawingHF",
    "picture",
    "autoFilter",
    "extLst",
];

/// The first element of a worksheet whose references would not follow
/// inserted and deleted rows and columns, if any: one this file does not
/// move, a filter with criteria or sorting (they count columns from the
/// filter's edge), or newer conditional formats, validations and
/// sparklines in the extension list.
pub(crate) fn blocker(bytes: &[u8]) -> Option<String> {
    let (_, xml) = split_bom(bytes);
    for (name, start, end) in top_level(xml) {
        if name.is_empty() {
            continue;
        }
        let text = &xml[start..end];
        let has = |what: &[u8]| text.windows(what.len()).any(|w| w == what);
        let refused = !HANDLED.contains(&name.as_str())
            || (name == "autoFilter" && (has(b"filterColumn") || has(b"sortState")))
            || (name == "extLst" && has(b"sqref"));
        if refused {
            return Some(name);
        }
    }
    None
}

/// The worksheet with `changes` applied in order. `name` is the sheet's
/// name, for the formulas of conditional formats and validations.
pub(super) fn restructure_sheet(
    bytes: &[u8],
    part: &str,
    name: &str,
    changes: &[Lines],
) -> Result<Vec<u8>> {
    let mut out = bytes.to_vec();
    for lines in changes {
        out = apply(&out, part, name, lines)?;
    }
    Ok(out)
}

fn apply(bytes: &[u8], part: &str, name: &str, lines: &Lines) -> Result<Vec<u8>> {
    let (bom, xml) = split_bom(bytes);
    let scan = scan(xml, part)?;
    let text = |start: usize, end: usize| String::from_utf8_lossy(&xml[start..end]).into_owned();
    // The end of the start tag beginning at `start`.
    let tag_end = |start: usize| {
        xml[start..]
            .iter()
            .position(|b| *b == b'>')
            .map_or(xml.len(), |i| start + i + 1)
    };
    let mut ops: Vec<(usize, usize, String)> = Vec::new();
    for row in &scan.rows {
        if lines.columns {
            let tag = text(row.start, row.open_end);
            // `spans` is a hint of where the cells are; it would be wrong.
            if tag.contains(" spans=") {
                ops.push((row.start, row.open_end, drop_attribute(&tag, "spans")));
            }
            for cell in &row.cells {
                match lines.moved(cell.col) {
                    None => ops.push((cell.start, cell.end, String::new())),
                    Some(col) => {
                        let written = super::find(&cell.attributes, "r").is_some();
                        if col != cell.col || !written {
                            let end = tag_end(cell.start);
                            let at = CellRef::new(row.index, col).to_string();
                            ops.push((
                                cell.start,
                                end,
                                set_attribute(&text(cell.start, end), "r", &at),
                            ));
                        }
                    }
                }
            }
        } else {
            match lines.moved(row.index) {
                None => ops.push((row.start, row.end, String::new())),
                Some(index) if index != row.index => {
                    let tag = text(row.start, row.open_end);
                    ops.push((
                        row.start,
                        row.open_end,
                        set_attribute(&tag, "r", &(index + 1).to_string()),
                    ));
                    for cell in &row.cells {
                        if super::find(&cell.attributes, "r").is_some() {
                            let end = tag_end(cell.start);
                            let at = CellRef::new(index, cell.col).to_string();
                            ops.push((
                                cell.start,
                                end,
                                set_attribute(&text(cell.start, end), "r", &at),
                            ));
                        }
                    }
                }
                Some(_) => {}
            }
        }
    }

    for (child, start, end) in top_level(xml) {
        let element = text(start, end);
        let new = match child.as_str() {
            "dimension" => Some(map_attributes(&element, &["ref"], |v| {
                Some(ranges(v, lines).unwrap_or_else(|| point(v, lines)))
            })),
            "sheetViews" => Some(views(&element, lines)),
            "cols" if lines.columns => Some(columns(&element, lines)),
            "conditionalFormatting" => conditional(&element, name, lines),
            "dataValidations" => Some(listed(&element, "dataValidation", |item| {
                validation(item, name, lines)
            })),
            "hyperlinks" => Some(listed(&element, "hyperlink", |item| {
                kept_with(item, "ref", lines)
            })),
            "ignoredErrors" => Some(listed(&element, "ignoredError", |item| {
                kept_with(item, "sqref", lines)
            })),
            "autoFilter" => kept_with(&element, "ref", lines).or(Some(String::new())),
            "rowBreaks" if !lines.columns => Some(breaks(&element, lines)),
            "colBreaks" if lines.columns => Some(breaks(&element, lines)),
            _ => None,
        };
        if let Some(new) = new
            && new != element
        {
            ops.push((start, end, new));
        }
    }

    ops.sort_by_key(|(start, ..)| *start);
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(bom);
    let mut at = 0;
    for (start, end, text) in ops {
        if start < at {
            continue;
        }
        out.extend_from_slice(&xml[at..start]);
        out.extend_from_slice(text.as_bytes());
        at = end;
    }
    out.extend_from_slice(&xml[at..]);
    Ok(out)
}

/// A space-separated list of ranges after the change; `None` if all of it
/// is deleted. What is not a cell range (whole columns, say) is kept.
fn ranges(value: &str, lines: &Lines) -> Option<String> {
    let kept: Vec<String> = value
        .split_whitespace()
        .filter_map(|item| match CellRange::parse(item) {
            Some(range) => lines.moved_range(range).map(|r| r.to_string()),
            None => Some(item.to_owned()),
        })
        .collect();
    (!kept.is_empty()).then(|| kept.join(" "))
}

/// A cell after the change; a deleted one becomes the cell at the first
/// line after the deleted ones, as Excel moves the selection.
fn point(value: &str, lines: &Lines) -> String {
    let first = value.split([' ', ':']).next().unwrap_or(value);
    let Some(at) = CellRef::parse(first) else {
        return value.to_owned();
    };
    let moved = |line: u32| lines.moved(line).unwrap_or(lines.at);
    let at = if lines.columns {
        CellRef::new(at.row, moved(at.col))
    } else {
        CellRef::new(moved(at.row), at.col)
    };
    at.to_string()
}

fn views(element: &str, lines: &Lines) -> String {
    map_attributes(element, &["activeCell", "topLeftCell", "sqref"], |value| {
        Some(if value.contains(' ') || value.contains(':') {
            ranges(value, lines).unwrap_or_else(|| point(value, lines))
        } else {
            point(value, lines)
        })
    })
}

/// `<conditionalFormatting>` with its ranges and rule formulas moved, or
/// nothing if all of its cells are deleted.
fn conditional(element: &str, sheet: &str, lines: &Lines) -> Option<String> {
    let (tag, _) = start_tag(element);
    let sqref = super::attribute(tag.as_bytes(), "sqref")?;
    let Some(moved) = ranges(&sqref, lines) else {
        return Some(String::new());
    };
    let element = format!(
        "{}{}",
        set_attribute(tag, "sqref", &moved),
        &element[tag.len()..]
    );
    Some(map_texts(&element, &["formula"], |f| {
        adjust(f, Some(sheet), sheet, lines)
    }))
}

fn validation(item: &str, sheet: &str, lines: &Lines) -> Option<String> {
    let item = kept_with(item, "sqref", lines)?;
    Some(map_texts(&item, &["formula1", "formula2"], |f| {
        adjust(f, Some(sheet), sheet, lines)
    }))
}

/// An element with the ranges in attribute `name` moved; `None` if all of
/// them are deleted.
fn kept_with(element: &str, name: &str, lines: &Lines) -> Option<String> {
    let (tag, _) = start_tag(element);
    let Some(value) = super::attribute(tag.as_bytes(), name) else {
        return Some(element.to_owned());
    };
    let moved = ranges(&value, lines)?;
    Some(format!(
        "{}{}",
        set_attribute(tag, name, &moved),
        &element[tag.len()..]
    ))
}

/// A list element whose `item` children pass through `keep` (which may
/// change them or drop them); `count` is set to what is left, and the
/// list goes when nothing is.
fn listed(element: &str, item: &str, keep: impl Fn(&str) -> Option<String>) -> String {
    let (tag, _) = start_tag(element);
    if tag.ends_with("/>") {
        return element.to_owned();
    }
    let mut kept = Vec::new();
    for (name, raw) in children(element) {
        if name == item {
            if let Some(changed) = keep(&raw) {
                kept.push(changed);
            }
        } else {
            kept.push(raw);
        }
    }
    let close = &element[element.rfind("</").unwrap_or(element.len())..];
    let count = kept.len();
    if count == 0 {
        return String::new();
    }
    let tag = if tag.contains(" count=") {
        set_attribute(tag, "count", &count.to_string())
    } else {
        tag.to_owned()
    };
    format!("{tag}{}{close}", kept.concat())
}

/// `<rowBreaks>` or `<colBreaks>`: each break's line moved, deleted ones
/// gone.
fn breaks(element: &str, lines: &Lines) -> String {
    let manual = |kept: &[String]| kept.iter().filter(|b| b.contains("man=\"1\"")).count();
    let mut kept = Vec::new();
    let (tag, _) = start_tag(element);
    for (name, raw) in children(element) {
        if name != "brk" {
            kept.push(raw);
            continue;
        }
        let (brk, _) = start_tag(&raw);
        let Some(id) = super::attribute(brk.as_bytes(), "id").and_then(|v| v.parse::<u32>().ok())
        else {
            kept.push(raw);
            continue;
        };
        if let Some(line) = lines.moved(id) {
            kept.push(format!(
                "{}{}",
                set_attribute(brk, "id", &line.to_string()),
                &raw[brk.len()..]
            ));
        }
    }
    if kept.is_empty() {
        return String::new();
    }
    let close = &element[element.rfind("</").unwrap_or(element.len())..];
    let mut tag = tag.to_owned();
    if tag.contains(" count=") {
        tag = set_attribute(&tag, "count", &kept.len().to_string());
    }
    if tag.contains(" manualBreakCount=") {
        tag = set_attribute(&tag, "manualBreakCount", &manual(&kept).to_string());
    }
    format!("{tag}{}{close}", kept.concat())
}

/// `<cols>` with each `<col>`'s span moved: inserting inside a span widens
/// it, deleting shrinks it, a span all deleted goes.
fn columns(element: &str, lines: &Lines) -> String {
    let (tag, _) = start_tag(element);
    let mut kept = Vec::new();
    for (name, raw) in children(element) {
        if name != "col" {
            kept.push(raw);
            continue;
        }
        let (col, _) = start_tag(&raw);
        let number =
            |key: &str| super::attribute(col.as_bytes(), key).and_then(|v| v.parse::<u32>().ok());
        let (Some(min), Some(max)) = (number("min"), number("max")) else {
            kept.push(raw);
            continue;
        };
        if let Some((first, last)) = lines.moved_span(min.saturating_sub(1), max.saturating_sub(1))
        {
            let col = set_attribute(col, "min", &(first + 1).to_string());
            let col = set_attribute(&col, "max", &(last + 1).to_string());
            kept.push(format!("{col}{}", &raw[start_tag(&raw).0.len()..]));
        }
    }
    if kept.is_empty() {
        return String::new();
    }
    let close = &element[element.rfind("</").unwrap_or(element.len())..];
    format!("{tag}{}{close}", kept.concat())
}

/// A start tag without attribute `name`.
pub(super) fn drop_attribute(tag: &str, name: &str) -> String {
    let pattern = format!(" {name}=");
    let Some(found) = tag.find(&pattern) else {
        return tag.to_owned();
    };
    let at = found + pattern.len();
    let quote = tag[at..].chars().next().unwrap_or('"');
    match tag[at + 1..].find(quote) {
        Some(len) => format!("{}{}", &tag[..found], &tag[at + 1 + len + 1..]),
        None => tag.to_owned(),
    }
}

/// Every start tag in `element` with the attributes in `names` passed
/// through `change` (`None` keeps the value).
pub(super) fn map_attributes(
    element: &str,
    names: &[&str],
    change: impl Fn(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(element.len());
    let mut rest = element;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];
        let end = tail.find('>').map_or(tail.len(), |i| i + 1);
        let mut tag = tail[..end].to_owned();
        for name in names {
            if let Some(value) = super::attribute(tag.as_bytes(), name)
                && let Some(changed) = change(&unescape(&value))
                && changed != unescape(&value)
            {
                tag = set_attribute(&tag, name, &escape(&changed));
            }
        }
        out.push_str(&tag);
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// `element` with the text of every element named in `names` (local
/// names) passed through `change`.
pub(super) fn map_texts(element: &str, names: &[&str], change: impl Fn(&str) -> String) -> String {
    let mut reader = Reader::from_reader(element.as_bytes());
    reader.config_mut().check_end_names = false;
    // Where the content of a wanted element starts.
    let mut inside: Option<usize> = None;
    let mut ops: Vec<(usize, usize, String)> = Vec::new();
    loop {
        let before = reader.buffer_position() as usize;
        let Ok(event) = reader.read_event() else {
            break;
        };
        let after = reader.buffer_position() as usize;
        match event {
            Event::Start(e) => {
                inside = names.contains(&super::local(&e)).then_some(after);
            }
            Event::End(_) => {
                if let Some(start) = inside.take() {
                    let text = unescape(&element[start..before]);
                    let changed = change(&text);
                    if changed != text {
                        ops.push((start, before, escape(&changed)));
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut out = String::with_capacity(element.len());
    let mut at = 0;
    for (start, end, text) in ops {
        out.push_str(&element[at..start]);
        out.push_str(&text);
        at = end;
    }
    out.push_str(&element[at..]);
    out
}

/// A worksheet whose conditional formats' and validations' formulas name
/// sheets that changed: lines moved on `elsewhere` (target sheet, lines,
/// in order), then `sheets` (renamed and deleted sheets). `host` is this
/// sheet's name in the file. `None` when nothing changed.
pub(super) fn rule_formulas(
    bytes: &[u8],
    host: &str,
    elsewhere: &[(&str, Lines)],
    sheets: &dyn Fn(&str) -> String,
) -> Option<Vec<u8>> {
    let (bom, xml) = split_bom(bytes);
    let has = |what: &[u8]| xml.windows(what.len()).any(|w| w == what);
    if !has(b"formula") {
        return None;
    }
    let text = std::str::from_utf8(xml).ok()?;
    let new = map_texts(text, &["formula", "formula1", "formula2"], |f| {
        let mut out = f.to_owned();
        for (target, lines) in elsewhere {
            out = adjust(&out, Some(host), target, lines);
        }
        sheets(&out)
    });
    (new != text).then(|| [bom, new.as_bytes()].concat())
}

/// A drawing part with its anchors' cells moved; a picture whose cell is
/// deleted goes to the first line after the deleted ones, as the model
/// places it.
pub(super) fn move_anchors(bytes: &[u8], changes: &[Lines]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for lines in changes {
        out = anchors(&out, lines);
    }
    out
}

fn anchors(bytes: &[u8], lines: &Lines) -> Vec<u8> {
    let (bom, xml) = split_bom(bytes);
    let Ok(text) = std::str::from_utf8(xml) else {
        return bytes.to_vec();
    };
    let wanted = if lines.columns { "col" } else { "row" };
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().check_end_names = false;
    let mut in_anchor = 0usize;
    let mut inside = false;
    let mut ops: Vec<(usize, usize, String)> = Vec::new();
    loop {
        let before = reader.buffer_position() as usize;
        let Ok(event) = reader.read_event() else {
            break;
        };
        let after = reader.buffer_position() as usize;
        match event {
            Event::Start(e) => {
                let name = super::local(&e);
                if name == "from" || name == "to" {
                    in_anchor += 1;
                } else if in_anchor > 0 && name == wanted {
                    inside = true;
                }
            }
            Event::Text(_) if inside => {
                if let Ok(line) = text[before..after].trim().parse::<u32>() {
                    let moved = lines.moved(line).unwrap_or(lines.at);
                    if moved != line {
                        ops.push((before, after, moved.to_string()));
                    }
                }
            }
            Event::End(e) => {
                let name = e.local_name();
                if name.as_ref() == "from" || name.as_ref() == "to" {
                    in_anchor = in_anchor.saturating_sub(1);
                }
                inside = false;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(bom);
    let mut at = 0;
    for (start, end, text) in ops {
        out.extend_from_slice(&xml[at..start]);
        out.extend_from_slice(text.as_bytes());
        at = end;
    }
    out.extend_from_slice(&xml[at..]);
    out
}

/// `workbook.xml` with its defined names' references moved. `sheets` are
/// the sheet names in order, for names local to one; `target` is the
/// sheet the lines changed on.
pub(super) fn move_names(
    bytes: &[u8],
    sheets: &[String],
    target: &str,
    changes: &[Lines],
) -> Vec<u8> {
    let (bom, xml) = split_bom(bytes);
    let Ok(text) = std::str::from_utf8(xml) else {
        return bytes.to_vec();
    };
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().check_end_names = false;
    // The sheet a name is local to, and where its formula starts.
    let mut host: Option<(Option<String>, usize)> = None;
    let mut ops: Vec<(usize, usize, String)> = Vec::new();
    loop {
        let before = reader.buffer_position() as usize;
        let Ok(event) = reader.read_event() else {
            break;
        };
        let after = reader.buffer_position() as usize;
        match event {
            Event::Start(e) if e.local_name().as_ref() == "definedName" => {
                let local = super::attributes(&e);
                let sheet = super::find(&local, "localSheetId")
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| sheets.get(i).cloned());
                host = Some((sheet, after));
            }
            Event::End(e) if e.local_name().as_ref() == "definedName" => {
                if let Some((sheet, start)) = host.take() {
                    let written = unescape(&text[start..before]);
                    let mut formula = written.clone();
                    for lines in changes {
                        formula = adjust(&formula, sheet.as_deref(), target, lines);
                    }
                    if formula != written {
                        ops.push((start, before, escape(&formula)));
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(bom);
    let mut at = 0;
    for (start, end, text) in ops {
        out.extend_from_slice(&xml[at..start]);
        out.extend_from_slice(text.as_bytes());
        at = end;
    }
    out.extend_from_slice(&xml[at..]);
    out
}
