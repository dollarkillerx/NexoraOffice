//! XLSX: workbooks into [`aster_sheets::Workbook`].
//!
//! Read: the sheets and their order and visibility, the 1904 date system,
//! shared strings (rich text flattened; phonetic guides, the furigana of
//! Japanese names, left out), number formats and horizontal alignment of
//! cell formats, cell values with their types, formulas (shared formulas
//! expanded to each cell), column widths, row heights, hidden rows and
//! columns, and merged cells. Everything else in the file is not read.

use std::collections::HashMap;
use std::sync::Arc;

use aster_sheets::{
    Axis, Cell, CellRange, CellRef, CellStyle, Format, HAlign, MAX_COLUMNS, MAX_ROWS, Sheet,
    SheetBuilder, SheetKind, Value, Workbook,
};

use crate::error::{Error, Result};
use crate::package::{Kind, Package};
use crate::xml::{Ev, Tag, XmlReader, decode_escapes};

/// Pixels per point at 96 dpi.
const PX_PER_POINT: f64 = 96.0 / 72.0;
/// Width of a digit of the default font (Calibri 11) in pixels, which
/// column widths are counted in.
const DIGIT_WIDTH: f64 = 7.0;

pub fn read(bytes: Vec<u8>) -> Result<Workbook> {
    let mut package = Package::open(bytes)?;
    read_package(&mut package)
}

pub fn read_package(package: &mut Package) -> Result<Workbook> {
    let main = package.expect(Kind::Xlsx)?;
    let rels: HashMap<String, (String, Option<String>)> = package
        .relationships(&main)?
        .into_iter()
        .map(|rel| (rel.id, (rel.kind, rel.target)))
        .collect();
    let target_of = |kind: &str| {
        rels.values()
            .find(|(k, _)| k == kind)
            .and_then(|(_, target)| target.clone())
    };

    let strings = match target_of("sharedStrings") {
        Some(part) if package.has(&part) => shared_strings(&package.read(&part)?, &part)?,
        _ => Vec::new(),
    };
    let styles = match target_of("styles") {
        Some(part) if package.has(&part) => styles(&package.read(&part)?, &part)?,
        _ => Vec::new(),
    };

    let book = package.read(&main)?;
    let entries = workbook_entries(&book, &main)?;
    let mut workbook = Workbook {
        sheets: Vec::with_capacity(entries.sheets.len()),
        styles,
        date1904: entries.date1904,
        active_sheet: 0,
    };
    for entry in entries.sheets {
        let (kind, target) = match rels.get(&entry.rel) {
            Some((kind, Some(target))) if package.has(target) => (kind.as_str(), target.clone()),
            // A sheet whose part is missing is shown empty rather than
            // failing the whole workbook.
            _ => {
                let mut sheet = Sheet::empty(entry.name, SheetKind::Worksheet);
                sheet.hidden = entry.hidden;
                workbook.sheets.push(sheet);
                continue;
            }
        };
        let sheet = match kind {
            "worksheet" => {
                let bytes = package.read(&target)?;
                worksheet(&bytes, &target, entry.name, &strings)?.finish()
            }
            "chartsheet" => Sheet::empty(entry.name, SheetKind::Chart),
            _ => Sheet::empty(entry.name, SheetKind::Other),
        };
        let mut sheet = sheet;
        sheet.hidden = entry.hidden;
        workbook.sheets.push(sheet);
    }
    if workbook.sheets.is_empty() {
        return Err(Error::broken(&main, "no sheets"));
    }
    workbook.active_sheet = entries.active.min(workbook.sheets.len() - 1);
    Ok(workbook)
}

struct Entry {
    name: String,
    rel: String,
    hidden: bool,
}

struct Entries {
    sheets: Vec<Entry>,
    date1904: bool,
    active: usize,
}

fn workbook_entries(bytes: &[u8], part: &str) -> Result<Entries> {
    let mut reader = XmlReader::new(bytes, part);
    let mut entries = Entries {
        sheets: Vec::new(),
        date1904: false,
        active: 0,
    };
    loop {
        match reader.next()? {
            Ev::Open(tag) if tag.is("sheet") => {
                let (Some(name), Some(rel)) = (tag.attr("name"), tag.attr("id")) else {
                    continue;
                };
                let hidden = tag
                    .attr("state")
                    .is_some_and(|state| state == "hidden" || state == "veryHidden");
                entries.sheets.push(Entry { name, rel, hidden });
            }
            Ev::Open(tag) if tag.is("workbookPr") => {
                entries.date1904 = tag.attr_bool("date1904").unwrap_or(false);
            }
            Ev::Open(tag) if tag.is("workbookView") => {
                entries.active = tag.attr_i64("activeTab").unwrap_or(0).max(0) as usize;
            }
            Ev::Eof => return Ok(entries),
            _ => {}
        }
    }
}

/// The text of an `<si>` or `<is>`: plain or rich text runs, without
/// phonetic guides.
fn string_item(reader: &mut XmlReader<'_>) -> Result<String> {
    let mut out = String::new();
    let mut depth = 1usize;
    loop {
        match reader.next()? {
            Ev::Open(tag) if tag.is("t") => out.push_str(&reader.text()?),
            Ev::Open(tag) if tag.is("rPh") || tag.is("phoneticPr") || tag.is("rPr") => {
                reader.skip()?
            }
            Ev::Open(_) => depth += 1,
            Ev::Close => {
                depth -= 1;
                if depth == 0 {
                    return Ok(decode_escapes(&out).into_owned());
                }
            }
            Ev::Text(_) => {}
            Ev::Eof => return Err(Error::broken(reader.part(), "ends inside a string")),
        }
    }
}

fn shared_strings(bytes: &[u8], part: &str) -> Result<Vec<Arc<str>>> {
    let mut reader = XmlReader::new(bytes, part);
    let mut strings = Vec::new();
    loop {
        match reader.next()? {
            Ev::Open(tag) if tag.is("sst") => {
                if let Some(count) = tag.attr_i64("uniqueCount") {
                    // Trust the count only as far as the part could hold.
                    strings.reserve((count.max(0) as usize).min(bytes.len() / 8));
                }
            }
            Ev::Open(tag) if tag.is("si") => strings.push(Arc::from(string_item(&mut reader)?)),
            Ev::Eof => return Ok(strings),
            _ => {}
        }
    }
}

fn styles(bytes: &[u8], part: &str) -> Result<Vec<CellStyle>> {
    let mut reader = XmlReader::new(bytes, part);
    let mut custom: HashMap<u32, String> = HashMap::new();
    let mut xfs: Vec<(u32, Option<HAlign>)> = Vec::new();
    loop {
        match reader.next()? {
            Ev::Open(tag) if tag.is("numFmt") => {
                if let (Some(id), Some(code)) = (tag.attr_i64("numFmtId"), tag.attr("formatCode")) {
                    custom.insert(id.clamp(0, i64::from(u32::MAX)) as u32, code);
                }
            }
            // `cellStyleXfs` holds `xf` elements too; only these count.
            Ev::Open(tag) if tag.is("cellXfs") => xfs = cell_xfs(&mut reader)?,
            Ev::Eof => break,
            _ => {}
        }
    }
    let mut formats: HashMap<u32, Arc<Format>> = HashMap::new();
    Ok(xfs
        .into_iter()
        .map(|(id, align)| {
            let format = formats
                .entry(id)
                .or_insert_with(|| {
                    Arc::new(match custom.get(&id) {
                        Some(code) => Format::parse(code),
                        None => Format::builtin(id),
                    })
                })
                .clone();
            CellStyle { format, align }
        })
        .collect())
}

/// Excel's column width (in digits, with padding) to logical pixels.
fn column_px(width: f64) -> f64 {
    (width * DIGIT_WIDTH + 0.5).floor().max(0.0)
}

fn worksheet(bytes: &[u8], part: &str, name: String, strings: &[Arc<str>]) -> Result<SheetBuilder> {
    let mut reader = XmlReader::new(bytes, part);
    let mut builder = SheetBuilder::new(name, SheetKind::Worksheet);
    let mut row_heights: Vec<(u32, f64)> = Vec::new();
    let mut shared: HashMap<u32, (Arc<str>, CellRef)> = HashMap::new();
    let mut next_row: u32 = 0;
    loop {
        match reader.next()? {
            Ev::Open(tag) if tag.is("sheetFormatPr") => {
                let base = tag.attr_f64("baseColWidth").unwrap_or(8.0);
                let width = match tag.attr_f64("defaultColWidth") {
                    Some(width) => column_px(width),
                    // Base width plus padding, rounded up to a multiple of 8.
                    None => ((base * DIGIT_WIDTH + 5.0) / 8.0).ceil() * 8.0,
                };
                let height = tag.attr_f64("defaultRowHeight").unwrap_or(15.0) * PX_PER_POINT;
                builder.columns = Axis::new(width, MAX_COLUMNS);
                builder.row_sizes = Axis::new(height.round(), MAX_ROWS);
            }
            Ev::Open(tag) if tag.is("col") => {
                let min = tag
                    .attr_i64("min")
                    .unwrap_or(1)
                    .clamp(1, i64::from(MAX_COLUMNS));
                let max = tag
                    .attr_i64("max")
                    .unwrap_or(min)
                    .clamp(min, i64::from(MAX_COLUMNS));
                let hidden = tag.attr_bool("hidden").unwrap_or(false);
                let width = match tag.attr_f64("width") {
                    _ if hidden => Some(0.0),
                    Some(width) => Some(column_px(width)),
                    None => None,
                };
                if let Some(width) = width {
                    builder.columns.set(min as u32 - 1, max as u32, width);
                }
            }
            Ev::Open(tag) if tag.is("row") => {
                let index = match tag.attr_i64("r") {
                    Some(r) if (1..=i64::from(MAX_ROWS)).contains(&r) => r as u32 - 1,
                    Some(_) => {
                        reader.skip()?;
                        continue;
                    }
                    None => next_row,
                };
                next_row = index.saturating_add(1);
                if tag.attr_bool("hidden").unwrap_or(false) {
                    row_heights.push((index, 0.0));
                } else if let Some(points) = tag.attr_f64("ht") {
                    row_heights.push((index, (points * PX_PER_POINT).round()));
                }
                read_row(&mut reader, index, &mut builder, strings, &mut shared)?;
            }
            Ev::Open(tag) if tag.is("mergeCell") => {
                if let Some(range) = tag.attr("ref").as_deref().and_then(CellRange::parse) {
                    builder.merge(range);
                }
            }
            Ev::Open(tag)
                if tag.is("sheetViews")
                    || tag.is("conditionalFormatting")
                    || tag.is("dataValidations")
                    || tag.is("extLst") =>
            {
                reader.skip()?
            }
            Ev::Eof => break,
            _ => {}
        }
    }
    row_heights.sort_by_key(|(row, _)| *row);
    row_heights.dedup_by_key(|(row, _)| *row);
    builder.row_sizes.set_sorted(
        row_heights
            .into_iter()
            .map(|(row, size)| (row, row + 1, size)),
    );
    Ok(builder)
}

fn read_row(
    reader: &mut XmlReader<'_>,
    row: u32,
    builder: &mut SheetBuilder,
    strings: &[Arc<str>],
    shared: &mut HashMap<u32, (Arc<str>, CellRef)>,
) -> Result<()> {
    let mut next_col: u32 = 0;
    loop {
        match reader.next()? {
            Ev::Open(tag) if tag.is("c") => {
                let col = match tag.attr("r").as_deref().and_then(CellRef::parse) {
                    Some(at) => at.col,
                    None if tag.attr("r").is_some() => {
                        reader.skip()?;
                        continue;
                    }
                    None => next_col,
                };
                next_col = col.saturating_add(1);
                let cell = read_cell(reader, &tag, CellRef::new(row, col), strings, shared)?;
                if let Some(cell) = cell {
                    builder.push(row, cell);
                }
            }
            Ev::Open(_) => reader.skip()?,
            Ev::Close => return Ok(()),
            Ev::Text(_) => {}
            Ev::Eof => return Err(Error::broken(reader.part(), "ends inside a row")),
        }
    }
}

fn cell_xfs(reader: &mut XmlReader<'_>) -> Result<Vec<(u32, Option<HAlign>)>> {
    let mut xfs = Vec::new();
    loop {
        match reader.next()? {
            Ev::Open(xf) if xf.is("xf") => {
                let id = xf
                    .attr_i64("numFmtId")
                    .unwrap_or(0)
                    .clamp(0, i64::from(u32::MAX)) as u32;
                let mut align = None;
                loop {
                    match reader.next()? {
                        Ev::Open(child) if child.is("alignment") => {
                            align = match child.attr("horizontal").as_deref() {
                                Some("left") => Some(HAlign::Left),
                                Some("center" | "centerContinuous") => Some(HAlign::Center),
                                Some("right") => Some(HAlign::Right),
                                _ => None,
                            };
                            reader.skip()?;
                        }
                        Ev::Open(_) => reader.skip()?,
                        Ev::Close => break,
                        Ev::Text(_) => {}
                        Ev::Eof => {
                            return Err(Error::broken(reader.part(), "ends inside a format"));
                        }
                    }
                }
                xfs.push((id, align));
            }
            Ev::Open(_) => reader.skip()?,
            Ev::Close => return Ok(xfs),
            Ev::Text(_) => {}
            Ev::Eof => return Err(Error::broken(reader.part(), "ends inside cellXfs")),
        }
    }
}

fn read_cell(
    reader: &mut XmlReader<'_>,
    tag: &Tag<'_>,
    at: CellRef,
    strings: &[Arc<str>],
    shared: &mut HashMap<u32, (Arc<str>, CellRef)>,
) -> Result<Option<Cell>> {
    let kind = tag.attr("t");
    let style = tag.attr_i64("s").unwrap_or(0).clamp(0, i64::from(u32::MAX)) as u32;
    let mut raw: Option<String> = None;
    let mut inline: Option<String> = None;
    let mut formula: Option<Arc<str>> = None;
    loop {
        match reader.next()? {
            Ev::Open(child) if child.is("v") => raw = Some(reader.text()?),
            Ev::Open(child) if child.is("is") => inline = Some(string_item(reader)?),
            Ev::Open(child) if child.is("f") => {
                let shared_index = (child.attr("t").as_deref() == Some("shared"))
                    .then(|| child.attr_i64("si"))
                    .flatten()
                    .and_then(|si| u32::try_from(si).ok());
                let text = reader.text()?;
                formula = if !text.trim().is_empty() {
                    let text: Arc<str> = Arc::from(text.trim());
                    if let Some(si) = shared_index {
                        shared.insert(si, (text.clone(), at));
                    }
                    Some(text)
                } else if let Some((master, origin)) = shared_index.and_then(|si| shared.get(&si)) {
                    let rows = i64::from(at.row) - i64::from(origin.row);
                    let cols = i64::from(at.col) - i64::from(origin.col);
                    Some(Arc::from(aster_sheets::formula::shift(master, rows, cols)))
                } else {
                    None
                };
            }
            Ev::Open(_) => reader.skip()?,
            Ev::Close => break,
            Ev::Text(_) => {}
            Ev::Eof => return Err(Error::broken(reader.part(), "ends inside a cell")),
        }
    }
    let value = match (kind.as_deref(), raw) {
        (Some("inlineStr"), _) => match inline {
            Some(text) => Value::Text(Arc::from(text)),
            None => Value::Empty,
        },
        (_, None) => Value::Empty,
        (Some("s"), Some(raw)) => match raw
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|i| strings.get(i))
        {
            Some(text) => Value::Text(text.clone()),
            None => Value::Error(Arc::from("#REF!")),
        },
        (Some("str" | "d"), Some(raw)) => Value::Text(Arc::from(decode_escapes(&raw).as_ref())),
        (Some("b"), Some(raw)) => Value::Bool(raw.trim() == "1" || raw.trim() == "true"),
        (Some("e"), Some(raw)) => Value::Error(Arc::from(raw.trim())),
        (_, Some(raw)) => match raw.trim().parse::<f64>() {
            Ok(n) if n.is_finite() => Value::Number(n),
            _ if raw.trim().is_empty() => Value::Empty,
            _ => Value::Text(Arc::from(raw.as_str())),
        },
    };
    if value == Value::Empty && formula.is_none() {
        // A formatted but empty cell: nothing to show, nothing to keep yet.
        return Ok(None);
    }
    Ok(Some(Cell {
        col: at.col,
        value,
        style,
        formula,
    }))
}
