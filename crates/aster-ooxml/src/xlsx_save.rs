//! Saving an edited workbook back into its package (ADR 0001).
//!
//! The file that was read is the starting point, and as little of it as
//! possible changes:
//!
//! - parts not touched are copied as they were, compressed bytes and all;
//!   relationship parts and `[Content_Types].xml` included;
//! - in a worksheet with changed cells only those `<c>` elements are
//!   rewritten (added, removed); every other byte of the part stays;
//! - a formula whose result changed but whose text did not keeps its `<f>`
//!   element byte for byte, only its `<v>` is new;
//! - new text goes to the end of `sharedStrings.xml` (inline strings when the
//!   workbook has no shared strings part, so no part has to be added);
//! - a date, time, percentage, amount or grouped number typed into a General cell
//!   takes a fitting format, as in Excel; when the workbook had none, a
//!   copy of the cell's `<xf>` with Excel's number format for it goes to
//!   the end of `cellXfs` in `styles.xml` (a currency format, which has no
//!   built-in number, also to `<numFmts>`), nothing else there changes;
//! - `calcChain.xml`, Excel's record of the order it last calculated in,
//!   is removed when formulas were added or removed, with its relationship
//!   and its content type, as Excel itself does when it finds one that no
//!   longer matches. Otherwise it stays.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use aster_sheets::{Cell, CellRef, MAX_COLUMNS, MAX_ROWS, SheetKind, Value, Workbook};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use crate::error::{Error, Result};
use crate::package::{Kind, Package};
use crate::xlsx::{shared_strings, workbook_entries};

/// What changed since the workbook was read.
#[derive(Clone, Debug, Default)]
pub struct Changes {
    /// Cells to write: entered, cleared, pasted, or with a new result.
    pub cells: BTreeSet<(usize, CellRef)>,
    /// Of those, the ones whose content was changed by hand rather than
    /// recalculated; a recalculated formula keeps its `<f>` as it was.
    pub edited: BTreeSet<(usize, CellRef)>,
    /// A formula appeared or went: Excel's calculation chain no longer
    /// matches and is removed.
    pub formulas_changed: bool,
    /// Sheets whose merged areas changed: their `<mergeCells>` is written
    /// again.
    pub merges: BTreeSet<usize>,
    /// Columns and rows resized: sheet and index.
    pub columns: BTreeSet<(usize, u32)>,
    pub rows: BTreeSet<(usize, u32)>,
}

/// The widths and heights to write for sheet `index`, from the model's
/// sizes in pixels.
fn sizes_of(sheet: &aster_sheets::Sheet, index: usize, changes: &Changes, digit: f64) -> Sizes {
    let in_sheet = |set: &BTreeSet<(usize, u32)>| -> Vec<u32> {
        set.range((index, 0)..=(index, u32::MAX))
            .map(|&(_, i)| i)
            .collect()
    };
    Sizes {
        widths: in_sheet(&changes.columns)
            .into_iter()
            .map(|col| (col, sheet.columns.size(col) / digit))
            .collect(),
        heights: in_sheet(&changes.rows)
            .into_iter()
            .map(|row| (row, sheet.row_sizes.size(row) * 0.75))
            .collect(),
    }
}

/// The package `source` (the file as read) with `workbook`'s changed cells
/// written into it.
pub fn save(source: Vec<u8>, workbook: &Workbook, changes: &Changes) -> Result<Vec<u8>> {
    let mut package = Package::open(source)?;
    let main = package.expect(Kind::Xlsx)?;
    let rels = package.relationships(&main)?;
    let entries = workbook_entries(&package.read(&main)?, &main)?;
    let target = |id: &str| {
        rels.iter()
            .find(|rel| rel.id == id)
            .and_then(|rel| rel.target.clone())
    };
    let strings_part = rels
        .iter()
        .find(|rel| rel.kind == "sharedStrings")
        .and_then(|rel| rel.target.clone())
        .filter(|part| package.has(part));
    let mut strings = match &strings_part {
        Some(part) => {
            let bytes = package.read(part)?;
            Strings::shared(&bytes, part)?
        }
        None => Strings::Inline,
    };

    let mut replaced: HashMap<String, Vec<u8>> = HashMap::new();
    let mut removed: HashSet<String> = HashSet::new();
    // Column widths count in the Normal font's digit width.
    let normal = workbook
        .styles
        .first()
        .map(|style| (*style.font).clone())
        .unwrap_or_default();
    let digit = crate::xlsx::digit_width(&normal);
    for (index, entry) in entries.sheets.iter().enumerate() {
        let Some(sheet) = workbook.sheets.get(index) else {
            continue;
        };
        if sheet.kind != SheetKind::Worksheet {
            continue;
        }
        let cells: BTreeMap<CellRef, Mode> = changes
            .cells
            .range((index, CellRef::new(0, 0))..=(index, CellRef::new(MAX_ROWS, MAX_COLUMNS)))
            .map(|&(_, at)| {
                let mode = if changes.edited.contains(&(index, at)) {
                    Mode::Full
                } else {
                    Mode::Value
                };
                (at, mode)
            })
            .collect();
        let merges = changes
            .merges
            .contains(&index)
            .then_some(sheet.merges.as_slice());
        let sizes = sizes_of(sheet, index, changes, digit);
        if cells.is_empty() && merges.is_none() && sizes.is_empty() {
            continue;
        }
        let Some(part) = target(&entry.rel).filter(|part| package.has(part)) else {
            continue;
        };
        let bytes = package.read(&part)?;
        let rewritten = rewrite_sheet(&bytes, &part, sheet, &cells, merges, &sizes, &mut strings)?;
        replaced.insert(key(&part), rewritten);
    }

    // Styles added for typed dates and percentages go to the end of the
    // cell formats.
    if let Some(part) = rels
        .iter()
        .find(|rel| rel.kind == "styles")
        .and_then(|rel| rel.target.clone())
        .filter(|part| package.has(part))
    {
        let bytes = package.read(&part)?;
        if let Some(edited) = append_formats(&bytes, &part, workbook)? {
            replaced.insert(key(&part), edited);
        }
    }

    if let (Some(part), Strings::Shared(shared)) = (&strings_part, &strings)
        && !shared.added.is_empty()
    {
        let bytes = package.read(part)?;
        replaced.insert(key(part), shared.append(&bytes, part)?);
    }

    if changes.formulas_changed
        && let Some(chain) = rels
            .iter()
            .find(|rel| rel.kind == "calcChain")
            .and_then(|rel| rel.target.clone())
    {
        removed.insert(key(&chain));
        let rels_part = crate::package::rels_name(&main);
        let bytes = package.read(&rels_part)?;
        if let Some(edited) = remove_element(&bytes, &rels_part, "Relationship", |attr| {
            attr("Type").is_some_and(|kind| kind.ends_with("/calcChain"))
        })? {
            replaced.insert(key(&rels_part), edited);
        }
        let types = "[Content_Types].xml";
        let bytes = package.read(types)?;
        let chain_name = format!("/{chain}");
        if let Some(edited) = remove_element(&bytes, types, "Override", |attr| {
            attr("PartName").is_some_and(|name| name.eq_ignore_ascii_case(&chain_name))
        })? {
            replaced.insert(key(types), edited);
        }
    }
    package.rewrite(&replaced, &removed)
}

fn key(part: &str) -> String {
    part.trim_start_matches('/').to_ascii_lowercase()
}

/// How much of a cell is rewritten.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Everything: its formula, value, type and format.
    Full,
    /// Only the result of a formula whose text is unchanged.
    Value,
}

// --- Shared strings ---

enum Strings {
    Shared(Shared),
    /// No shared strings part: text is written inline.
    Inline,
}

struct Shared {
    index: HashMap<Arc<str>, usize>,
    existing: usize,
    added: Vec<Arc<str>>,
    /// References written to cells, for the part's `count`.
    references: usize,
}

impl Strings {
    fn shared(bytes: &[u8], part: &str) -> Result<Self> {
        let list = shared_strings(bytes, part)?;
        let mut index = HashMap::with_capacity(list.len());
        for (i, text) in list.iter().enumerate() {
            index.entry(text.clone()).or_insert(i);
        }
        Ok(Strings::Shared(Shared {
            index,
            existing: list.len(),
            added: Vec::new(),
            references: 0,
        }))
    }
}

impl Shared {
    fn index_of(&mut self, text: &Arc<str>) -> usize {
        self.references += 1;
        if let Some(&i) = self.index.get(text) {
            return i;
        }
        let i = self.existing + self.added.len();
        self.index.insert(text.clone(), i);
        self.added.push(text.clone());
        i
    }

    /// The part with the new strings added before `</sst>` and its counts
    /// brought up to date.
    fn append(&self, bytes: &[u8], part: &str) -> Result<Vec<u8>> {
        let (bom, xml) = split_bom(bytes);
        let mut reader = Reader::from_reader(xml);
        reader.config_mut().check_end_names = false;
        let mut start_tag: Option<(usize, usize, bool, String)> = None;
        let mut close: Option<usize> = None;
        loop {
            let before = reader.buffer_position() as usize;
            let event = reader.read_event().map_err(|e| Error::broken(part, e))?;
            let after = reader.buffer_position() as usize;
            match event {
                Event::Start(e) if local(&e) == "sst" => {
                    start_tag = Some((before, after, false, prefix(&e)));
                }
                Event::Empty(e) if local(&e) == "sst" => {
                    start_tag = Some((before, after, true, prefix(&e)));
                }
                Event::End(e) if e.local_name().as_ref() == "sst" => close = Some(before),
                Event::Eof => break,
                _ => {}
            }
        }
        let (start, open_end, empty, p) =
            start_tag.ok_or_else(|| Error::broken(part, "no <sst>"))?;
        let tag = &xml[start..open_end];
        let count = attribute(tag, "count").and_then(|c| c.parse::<usize>().ok());
        let unique = self.existing + self.added.len();
        let mut new_tag = String::from_utf8_lossy(tag).into_owned();
        new_tag = set_attribute(&new_tag, "uniqueCount", &unique.to_string());
        if let Some(count) = count {
            new_tag = set_attribute(&new_tag, "count", &(count + self.references).to_string());
        }
        let mut items = String::new();
        for text in &self.added {
            items.push_str(&format!("<{p}si>{}</{p}si>", text_element(&p, text)));
        }
        let mut out = Vec::with_capacity(bytes.len() + items.len() + 32);
        out.extend_from_slice(bom);
        out.extend_from_slice(&xml[..start]);
        if empty {
            let open = new_tag
                .trim_end_matches('>')
                .trim_end_matches('/')
                .trim_end();
            out.extend_from_slice(open.as_bytes());
            out.push(b'>');
            out.extend_from_slice(items.as_bytes());
            out.extend_from_slice(format!("</{p}sst>").as_bytes());
            out.extend_from_slice(&xml[open_end..]);
        } else {
            let close = close.ok_or_else(|| Error::broken(part, "no </sst>"))?;
            out.extend_from_slice(new_tag.as_bytes());
            out.extend_from_slice(&xml[open_end..close]);
            out.extend_from_slice(items.as_bytes());
            out.extend_from_slice(&xml[close..]);
        }
        Ok(out)
    }
}

// --- Scanning a worksheet ---

struct FormulaSpan {
    start: usize,
    end: usize,
    /// `si` of a shared formula, and whether this cell holds its text.
    shared: Option<(u32, bool)>,
}

struct CellSpan {
    col: u32,
    start: usize,
    end: usize,
    /// The start tag's attributes, values as written.
    attributes: Vec<(String, String)>,
    formula: Option<FormulaSpan>,
}

struct RowSpan {
    index: u32,
    start: usize,
    /// Where the start tag ends.
    open_end: usize,
    end: usize,
    /// Where `</row>` starts; `None` for `<row/>`.
    close: Option<usize>,
    cells: Vec<CellSpan>,
}

struct DataSpan {
    start: usize,
    open_end: usize,
    end: usize,
    close: Option<usize>,
}

struct Scan {
    /// The namespace prefix the cells are written with, `x:` or nothing.
    prefix: String,
    data: Option<DataSpan>,
    rows: Vec<RowSpan>,
}

fn local<'a>(e: &'a BytesStart<'_>) -> &'a str {
    let name = e.name().into_inner();
    name.rsplit(':').next().unwrap_or(name)
}

fn prefix(e: &BytesStart<'_>) -> String {
    let name = e.name().into_inner();
    match name.rsplit_once(':') {
        Some((p, _)) => format!("{p}:"),
        None => String::new(),
    }
}

fn attributes(e: &BytesStart<'_>) -> Vec<(String, String)> {
    e.attributes()
        .flatten()
        .map(|a| (a.key.into_inner().to_owned(), a.value.to_string()))
        .collect()
}

fn find<'a>(attributes: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|(k, _)| k.rsplit(':').next() == Some(name))
        .map(|(_, v)| v.as_str())
}

fn split_bom(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.strip_prefix(b"\xEF\xBB\xBF") {
        Some(rest) => (&bytes[..3], rest),
        None => (&[], bytes),
    }
}

fn scan(xml: &[u8], part: &str) -> Result<Scan> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().check_end_names = false;
    let mut out = Scan {
        prefix: String::new(),
        data: None,
        rows: Vec::new(),
    };
    let mut in_data = false;
    let mut row: Option<RowSpan> = None;
    let mut cell: Option<CellSpan> = None;
    let mut formula: Option<(usize, Option<(u32, bool)>)> = None;
    let mut next_row = 0u32;
    let mut next_col = 0u32;
    loop {
        let before = reader.buffer_position() as usize;
        let event = reader.read_event().map_err(|e| Error::broken(part, e))?;
        let after = reader.buffer_position() as usize;
        match event {
            Event::Start(e) | Event::Empty(e) if !in_data && local(&e) == "sheetData" => {
                let empty = after >= 2 && xml[after - 2] == b'/';
                out.prefix = prefix(&e);
                out.data = Some(DataSpan {
                    start: before,
                    open_end: after,
                    end: after,
                    close: None,
                });
                in_data = !empty;
            }
            Event::End(e) if in_data && e.local_name().as_ref() == "sheetData" => {
                if let Some(data) = &mut out.data {
                    data.close = Some(before);
                    data.end = after;
                }
                in_data = false;
            }
            Event::Start(e) | Event::Empty(e)
                if in_data && cell.is_none() && local(&e) == "row" =>
            {
                let attrs = attributes(&e);
                let index = find(&attrs, "r")
                    .and_then(|r| r.parse::<u32>().ok())
                    .filter(|r| (1..=MAX_ROWS).contains(r))
                    .map_or(next_row, |r| r - 1);
                next_row = index + 1;
                next_col = 0;
                let empty = xml[after - 2] == b'/';
                let span = RowSpan {
                    index,
                    start: before,
                    open_end: after,
                    end: after,
                    close: None,
                    cells: Vec::new(),
                };
                if empty {
                    out.rows.push(span);
                } else {
                    row = Some(span);
                }
            }
            Event::End(e)
                if row.is_some() && cell.is_none() && e.local_name().as_ref() == "row" =>
            {
                if let Some(mut span) = row.take() {
                    span.close = Some(before);
                    span.end = after;
                    out.rows.push(span);
                }
            }
            Event::Start(e) | Event::Empty(e)
                if row.is_some() && cell.is_none() && local(&e) == "c" =>
            {
                let attrs = attributes(&e);
                let col = find(&attrs, "r")
                    .and_then(CellRef::parse)
                    .map_or(next_col, |at| at.col);
                next_col = col + 1;
                let span = CellSpan {
                    col,
                    start: before,
                    end: after,
                    attributes: attrs,
                    formula: None,
                };
                if xml[after - 2] == b'/' {
                    if let Some(row) = &mut row {
                        row.cells.push(span);
                    }
                } else {
                    cell = Some(span);
                }
            }
            Event::Start(e) | Event::Empty(e) if cell.is_some() && local(&e) == "f" => {
                let attrs = attributes(&e);
                let shared = (find(&attrs, "t") == Some("shared"))
                    .then(|| find(&attrs, "si").and_then(|si| si.parse::<u32>().ok()))
                    .flatten()
                    .map(|si| (si, find(&attrs, "ref").is_some()));
                if xml[after - 2] == b'/' {
                    if let Some(cell) = &mut cell {
                        cell.formula = Some(FormulaSpan {
                            start: before,
                            end: after,
                            shared,
                        });
                    }
                } else {
                    formula = Some((before, shared));
                }
            }
            Event::End(e) if formula.is_some() && e.local_name().as_ref() == "f" => {
                if let (Some((start, shared)), Some(cell)) = (formula.take(), &mut cell) {
                    cell.formula = Some(FormulaSpan {
                        start,
                        end: after,
                        shared,
                    });
                }
            }
            Event::End(e)
                if cell.is_some() && formula.is_none() && e.local_name().as_ref() == "c" =>
            {
                if let (Some(mut span), Some(row)) = (cell.take(), &mut row) {
                    span.end = after;
                    row.cells.push(span);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

// --- Writing cells ---

/// Text for XML content: the five characters XML reserves, and characters
/// XML cannot hold as OOXML's `_xHHHH_`.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' => out.push(c),
            '\r' => out.push_str("_x000D_"),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {
                out.push_str(&format!("_x{:04X}_", c as u32));
            }
            // A literal "_x0041_" must not read as an escape.
            '_' if chars.get(i + 1) == Some(&'x')
                && chars.len() > i + 6
                && chars[i + 2..i + 6].iter().all(char::is_ascii_hexdigit)
                && chars[i + 6] == '_' =>
            {
                out.push_str("_x005F_");
            }
            c => out.push(c),
        }
    }
    out
}

fn text_element(p: &str, text: &str) -> String {
    let space = if text.starts_with(char::is_whitespace) || text.ends_with(char::is_whitespace) {
        " xml:space=\"preserve\""
    } else {
        ""
    };
    format!("<{p}t{space}>{}</{p}t>", escape(text))
}

/// A number as XML Schema writes a double, shortest form that reads back
/// the same.
fn number(value: f64) -> String {
    let magnitude = value.abs();
    if magnitude != 0.0 && !(1e-6..1e20).contains(&magnitude) {
        format!("{value:E}")
    } else {
        format!("{value}")
    }
}

/// The value part of a cell: its `t` attribute and what follows the
/// formula.
fn value_xml(
    p: &str,
    value: &Value,
    has_formula: bool,
    strings: &mut Strings,
) -> (Option<&'static str>, String) {
    match value {
        Value::Empty => (None, String::new()),
        Value::Number(n) => (None, format!("<{p}v>{}</{p}v>", number(*n))),
        Value::Bool(b) => (Some("b"), format!("<{p}v>{}</{p}v>", u8::from(*b))),
        Value::Error(e) => (Some("e"), format!("<{p}v>{}</{p}v>", escape(e))),
        Value::Text(text) if has_formula => (Some("str"), format!("<{p}v>{}</{p}v>", escape(text))),
        Value::Text(text) => match strings {
            Strings::Shared(shared) => {
                (Some("s"), format!("<{p}v>{}</{p}v>", shared.index_of(text)))
            }
            Strings::Inline => (
                Some("inlineStr"),
                format!("<{p}is>{}</{p}is>", text_element(p, text)),
            ),
        },
    }
}

/// A whole `<c>` for `cell` at `at`.
fn cell_xml(p: &str, at: CellRef, cell: &Cell, strings: &mut Strings) -> String {
    let style = if cell.style != 0 {
        format!(" s=\"{}\"", cell.style)
    } else {
        String::new()
    };
    let (kind, value) = value_xml(p, &cell.value, cell.formula.is_some(), strings);
    let kind = kind.map(|t| format!(" t=\"{t}\"")).unwrap_or_default();
    let formula = cell
        .formula
        .as_ref()
        .map(|f| format!("<{p}f>{}</{p}f>", escape(f)))
        .unwrap_or_default();
    if formula.is_empty() && value.is_empty() {
        return format!("<{p}c r=\"{at}\"{style}/>");
    }
    format!("<{p}c r=\"{at}\"{style}{kind}>{formula}{value}</{p}c>")
}

/// A formula cell with a new result: its start tag's attributes (but `t`)
/// and its `<f>` as they were, a new `<v>`.
fn value_only_xml(
    p: &str,
    xml: &[u8],
    span: &CellSpan,
    formula: &FormulaSpan,
    cell: &Cell,
    strings: &mut Strings,
) -> String {
    let (kind, value) = value_xml(p, &cell.value, true, strings);
    let mut out = format!("<{p}c");
    for (k, v) in &span.attributes {
        if k.rsplit(':').next() != Some("t") {
            out.push_str(&format!(" {k}=\"{v}\""));
        }
    }
    if let Some(kind) = kind {
        out.push_str(&format!(" t=\"{kind}\""));
    }
    out.push('>');
    out.push_str(&String::from_utf8_lossy(&xml[formula.start..formula.end]));
    out.push_str(&value);
    out.push_str(&format!("</{p}c>"));
    out
}

fn rewrite_sheet(
    bytes: &[u8],
    part: &str,
    sheet: &aster_sheets::Sheet,
    cells: &BTreeMap<CellRef, Mode>,
    merges: Option<&[aster_sheets::CellRange]>,
    sizes: &Sizes,
    strings: &mut Strings,
) -> Result<Vec<u8>> {
    let (bom, xml) = split_bom(bytes);
    let scan = scan(xml, part)?;
    let data = scan
        .data
        .as_ref()
        .ok_or_else(|| Error::broken(part, "no <sheetData>"))?;
    let p = scan.prefix.clone();

    // A changed cell that holds the text of a shared formula takes it away
    // from the others: they are written out with their own formulas.
    let mut cells = cells.clone();
    let mut broken = BTreeSet::new();
    for row in &scan.rows {
        for span in &row.cells {
            let at = CellRef::new(row.index, span.col);
            if let (
                Some(Mode::Full),
                Some(FormulaSpan {
                    shared: Some((si, true)),
                    ..
                }),
            ) = (cells.get(&at), &span.formula)
            {
                broken.insert(*si);
            }
        }
    }
    if !broken.is_empty() {
        for row in &scan.rows {
            for span in &row.cells {
                if let Some(FormulaSpan {
                    shared: Some((si, _)),
                    ..
                }) = &span.formula
                    && broken.contains(si)
                {
                    cells.insert(CellRef::new(row.index, span.col), Mode::Full);
                }
            }
        }
    }

    // Replacements: (start, end, order, text); insertions have start == end.
    let mut ops: Vec<(usize, usize, usize, String)> = Vec::new();
    let mut order = 0usize;
    let mut push = |ops: &mut Vec<(usize, usize, usize, String)>, start, end, text: String| {
        ops.push((start, end, order, text));
        order += 1;
    };
    let rows: BTreeMap<u32, &RowSpan> = scan.rows.iter().map(|r| (r.index, r)).collect();
    let mut by_row: BTreeMap<u32, Vec<(CellRef, Mode)>> = BTreeMap::new();
    for (&at, &mode) in &cells {
        by_row.entry(at.row).or_default().push((at, mode));
    }
    for row in sizes.heights.keys() {
        by_row.entry(*row).or_default();
    }
    // A row's start tag, with its new height if it has one.
    let row_tag = |tag: String, row: u32| match sizes.heights.get(&row) {
        Some(points) => {
            let tag = set_attribute(&tag, "ht", &number(*points));
            set_attribute(&tag, "customHeight", "1")
        }
        None => tag,
    };
    let mut new_rows: Vec<(u32, String)> = Vec::new();
    for (row_index, targets) in by_row {
        match rows.get(&row_index) {
            Some(row) => {
                let mut appended = String::new();
                for (at, mode) in targets {
                    let model = sheet.cell(at);
                    match row.cells.iter().find(|c| c.col == at.col) {
                        Some(span) => {
                            let text = match (mode, model, &span.formula) {
                                (_, None, _) => String::new(),
                                (Mode::Value, Some(cell), Some(formula))
                                    if cell.formula.is_some() =>
                                {
                                    value_only_xml(&p, xml, span, formula, cell, strings)
                                }
                                (_, Some(cell), _) => cell_xml(&p, at, cell, strings),
                            };
                            push(&mut ops, span.start, span.end, text);
                        }
                        None => {
                            let Some(cell) = model else { continue };
                            let text = cell_xml(&p, at, cell, strings);
                            match row.cells.iter().find(|c| c.col > at.col) {
                                Some(next) => push(&mut ops, next.start, next.start, text),
                                None => appended.push_str(&text),
                            }
                        }
                    }
                }
                let tag = String::from_utf8_lossy(&xml[row.start..row.open_end]).into_owned();
                let resized = sizes.heights.contains_key(&row_index);
                let tag = row_tag(tag, row_index);
                match (row.close, appended.is_empty()) {
                    (Some(close), _) => {
                        if resized {
                            push(&mut ops, row.start, row.open_end, tag);
                        }
                        if !appended.is_empty() {
                            push(&mut ops, close, close, appended);
                        }
                    }
                    // <row .../> becomes <row ...>cells</row>.
                    (None, false) => {
                        let open = tag.trim_end_matches('>').trim_end_matches('/').trim_end();
                        push(
                            &mut ops,
                            row.start,
                            row.end,
                            format!("{open}>{appended}</{p}row>"),
                        );
                    }
                    (None, true) if resized => push(&mut ops, row.start, row.end, tag),
                    (None, true) => {}
                }
            }
            None => {
                let mut content = String::new();
                for (at, _) in targets {
                    if let Some(cell) = sheet.cell(at) {
                        content.push_str(&cell_xml(&p, at, cell, strings));
                    }
                }
                let open = row_tag(format!("<{p}row r=\"{}\">", row_index + 1), row_index);
                if !content.is_empty() {
                    new_rows.push((row_index, format!("{open}{content}</{p}row>")));
                } else if sizes.heights.contains_key(&row_index) {
                    let open = open.trim_end_matches('>');
                    new_rows.push((row_index, format!("{open}/>")));
                }
            }
        }
    }
    if !new_rows.is_empty() {
        match data.close {
            Some(close) => {
                for (index, text) in new_rows {
                    let at = scan
                        .rows
                        .iter()
                        .find(|r| r.index > index)
                        .map_or(close, |r| r.start);
                    push(&mut ops, at, at, text);
                }
            }
            None => {
                let tag = String::from_utf8_lossy(&xml[data.start..data.open_end]);
                let open = tag.trim_end_matches('>').trim_end_matches('/').trim_end();
                let rows: String = new_rows.into_iter().map(|(_, text)| text).collect();
                push(
                    &mut ops,
                    data.start,
                    data.end,
                    format!("{open}>{rows}</{p}sheetData>"),
                );
            }
        }
    }
    if let Some(merges) = merges {
        let (start, end, text) = merge_cells(xml, &p, merges);
        push(&mut ops, start, end, text);
    }
    if !sizes.widths.is_empty() {
        let (start, end, text) = columns_xml(xml, &p, &sizes.widths, data.start);
        push(&mut ops, start, end, text);
    }
    ops.sort_by_key(|(start, _, order, _)| (*start, *order));
    let mut out = Vec::with_capacity(bytes.len() + 256);
    out.extend_from_slice(bom);
    let mut at = 0;
    for (start, end, _, text) in ops {
        if start < at {
            return Err(Error::broken(part, "overlapping changes"));
        }
        out.extend_from_slice(&xml[at..start]);
        out.extend_from_slice(text.as_bytes());
        at = end;
    }
    out.extend_from_slice(&xml[at..]);
    Ok(out)
}

/// Column widths (in digits, as the file counts) and row heights (points)
/// set here, by index.
#[derive(Clone, Debug, Default)]
pub struct Sizes {
    pub widths: Vec<(u32, f64)>,
    pub heights: BTreeMap<u32, f64>,
}

impl Sizes {
    pub fn is_empty(&self) -> bool {
        self.widths.is_empty() && self.heights.is_empty()
    }
}

/// A `<col>` element: the columns it covers (from 1) and its other
/// attributes as written.
struct Col {
    min: u32,
    max: u32,
    attributes: Vec<(String, String)>,
}

/// The sheet's `<cols>` with new widths: the `<col>` that covers a resized
/// column is split around it, keeping its attributes on every part; a
/// column no `<col>` covers gets one. A sheet without `<cols>` gets it
/// right before `<sheetData>`.
fn columns_xml(
    xml: &[u8],
    p: &str,
    widths: &[(u32, f64)],
    data_start: usize,
) -> (usize, usize, String) {
    let children_list = top_level(xml);
    let found = children_list.iter().find(|(name, ..)| name == "cols");
    let mut cols: Vec<Col> = Vec::new();
    if let Some((_, start, end)) = found {
        let element = String::from_utf8_lossy(&xml[*start..*end]).into_owned();
        for (name, raw) in children(&element) {
            if name != "col" {
                continue;
            }
            let mut reader = Reader::from_reader(raw.as_bytes());
            if let Ok(Event::Empty(e) | Event::Start(e)) = reader.read_event() {
                let all = attributes(&e);
                let min = find(&all, "min").and_then(|v| v.parse().ok()).unwrap_or(1);
                let max = find(&all, "max")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(min);
                let attributes = all
                    .into_iter()
                    .filter(|(k, _)| k != "min" && k != "max")
                    .collect();
                cols.push(Col {
                    min,
                    max,
                    attributes,
                });
            }
        }
    }
    for &(index, width) in widths {
        let column = index + 1;
        let set = |attributes: &mut Vec<(String, String)>| {
            attributes.retain(|(k, _)| k != "width" && k != "customWidth" && k != "hidden");
            attributes.push(("width".into(), number(width)));
            attributes.push(("customWidth".into(), "1".into()));
        };
        match cols.iter().position(|c| c.min <= column && column <= c.max) {
            Some(i) => {
                let old = cols.remove(i);
                let mut parts = Vec::new();
                if old.min < column {
                    parts.push(Col {
                        min: old.min,
                        max: column - 1,
                        attributes: old.attributes.clone(),
                    });
                }
                let mut changed = old.attributes.clone();
                set(&mut changed);
                parts.push(Col {
                    min: column,
                    max: column,
                    attributes: changed,
                });
                if old.max > column {
                    parts.push(Col {
                        min: column + 1,
                        max: old.max,
                        attributes: old.attributes,
                    });
                }
                for (offset, part) in parts.into_iter().enumerate() {
                    cols.insert(i + offset, part);
                }
            }
            None => {
                let mut fresh = Vec::new();
                set(&mut fresh);
                let at = cols
                    .iter()
                    .position(|c| c.min > column)
                    .unwrap_or(cols.len());
                cols.insert(
                    at,
                    Col {
                        min: column,
                        max: column,
                        attributes: fresh,
                    },
                );
            }
        }
    }
    let items: String = cols
        .iter()
        .map(|c| {
            let rest: String = c
                .attributes
                .iter()
                .map(|(k, v)| format!(" {k}=\"{v}\""))
                .collect();
            format!("<{p}col min=\"{}\" max=\"{}\"{rest}/>", c.min, c.max)
        })
        .collect();
    let text = format!("<{p}cols>{items}</{p}cols>");
    match found {
        Some((_, start, end)) => (*start, *end, text),
        None => (data_start, data_start, text),
    }
}

/// The worksheet's own children: local name and span.
fn top_level(xml: &[u8]) -> Vec<(String, usize, usize)> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().check_end_names = false;
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut name = String::new();
    loop {
        let before = reader.buffer_position() as usize;
        let Ok(event) = reader.read_event() else {
            break;
        };
        let after = reader.buffer_position() as usize;
        match event {
            Event::Start(e) => {
                if depth == 1 {
                    start = before;
                    name = local(&e).to_owned();
                }
                depth += 1;
            }
            Event::Empty(e) if depth == 1 => out.push((local(&e).to_owned(), before, after)),
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 1 {
                    out.push((name.clone(), start, after));
                }
                if depth == 0 {
                    // Where </worksheet> starts.
                    out.push((String::new(), before, before));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// The replacement for a sheet's `<mergeCells>`: where it goes and what.
/// A sheet without one gets it where the schema puts it: after the
/// elements that come before it, before those that come after.
fn merge_cells(xml: &[u8], p: &str, merges: &[aster_sheets::CellRange]) -> (usize, usize, String) {
    const AFTER: [&str; 23] = [
        "phoneticPr",
        "conditionalFormatting",
        "dataValidations",
        "hyperlinks",
        "printOptions",
        "pageMargins",
        "pageSetup",
        "headerFooter",
        "rowBreaks",
        "colBreaks",
        "customProperties",
        "cellWatches",
        "ignoredErrors",
        "smartTags",
        "drawing",
        "legacyDrawing",
        "legacyDrawingHF",
        "drawingHF",
        "picture",
        "oleObjects",
        "controls",
        "webPublishItems",
        "tableParts",
    ];
    let text = if merges.is_empty() {
        String::new()
    } else {
        let items: String = merges
            .iter()
            .map(|range| format!("<{p}mergeCell ref=\"{range}\"/>"))
            .collect();
        format!(
            "<{p}mergeCells count=\"{}\">{items}</{p}mergeCells>",
            merges.len()
        )
    };
    let children = top_level(xml);
    if let Some((_, start, end)) = children.iter().find(|(name, ..)| name == "mergeCells") {
        return (*start, *end, text);
    }
    let at = children
        .iter()
        .find(|(name, ..)| AFTER.contains(&name.as_str()) || name == "extLst" || name.is_empty())
        .map_or(xml.len(), |(_, start, _)| *start);
    (at, at, text)
}

// --- Cell formats ---

/// A list element of `styles.xml` (`numFmts`, `fonts`, `cellXfs` ...): where
/// its start tag is, where its end tag starts (`None` for `<x/>`), and the
/// spans of its items.
#[derive(Default)]
struct List {
    open: Option<(usize, usize)>,
    close: Option<usize>,
    items: Vec<(usize, usize)>,
}

/// Find the lists of `styles.xml` and their items.
/// Where `<styleSheet>`'s start tag ends and its prefix, and the lists.
type StyleLists = (Option<(usize, String)>, HashMap<String, List>);

fn style_lists(xml: &[u8], part: &str) -> Result<StyleLists> {
    const LISTS: [(&str, &str); 5] = [
        ("numFmts", "numFmt"),
        ("fonts", "font"),
        ("fills", "fill"),
        ("borders", "border"),
        ("cellXfs", "xf"),
    ];
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().check_end_names = false;
    let mut sheet: Option<(usize, String)> = None;
    let mut lists: HashMap<String, List> = HashMap::new();
    // The list being read, how deep inside it, and where its item started.
    let mut current: Option<(&str, &str)> = None;
    let mut depth = 0usize;
    let mut item_start = 0usize;
    let mut skip_depth = 0usize;
    loop {
        let before = reader.buffer_position() as usize;
        let event = reader.read_event().map_err(|e| Error::broken(part, e))?;
        let after = reader.buffer_position() as usize;
        match event {
            Event::Start(e) if sheet.is_none() && local(&e) == "styleSheet" => {
                sheet = Some((after, prefix(&e)));
            }
            Event::Start(e) if current.is_none() => {
                let name = local(&e).to_owned();
                if let Some(&(list, item)) = LISTS.iter().find(|(l, _)| *l == name)
                    && skip_depth == 0
                    && !lists.contains_key(list)
                {
                    lists.insert(
                        list.to_owned(),
                        List {
                            open: Some((before, after)),
                            ..List::default()
                        },
                    );
                    current = Some((list, item));
                    depth = 0;
                } else if sheet.is_some() {
                    // Lists inside other elements (dxfs, cellStyleXfs) are
                    // not the ones wanted.
                    skip_depth += 1;
                }
            }
            Event::Empty(e) if current.is_none() && skip_depth == 0 => {
                let name = local(&e).to_owned();
                if let Some(&(list, _)) = LISTS.iter().find(|(l, _)| *l == name)
                    && !lists.contains_key(list)
                {
                    lists.insert(
                        list.to_owned(),
                        List {
                            open: Some((before, after)),
                            ..List::default()
                        },
                    );
                }
            }
            Event::End(_) if current.is_none() && skip_depth > 0 => skip_depth -= 1,
            Event::Start(e) if current.is_some() => {
                if depth == 0 && local(&e) == current.map_or("", |c| c.1) {
                    item_start = before;
                }
                depth += 1;
            }
            Event::Empty(e) if current.is_some() => {
                if depth == 0
                    && local(&e) == current.map_or("", |c| c.1)
                    && let Some(list) = lists.get_mut(current.map_or("", |c| c.0))
                {
                    list.items.push((before, after));
                }
            }
            Event::End(e) if current.is_some() => {
                let (list_name, item) = current.unwrap_or(("", ""));
                if depth == 0 && e.local_name().as_ref() == list_name {
                    if let Some(list) = lists.get_mut(list_name) {
                        list.close = Some(before);
                    }
                    current = None;
                    continue;
                }
                depth = depth.saturating_sub(1);
                if depth == 0
                    && e.local_name().as_ref() == item
                    && let Some(list) = lists.get_mut(list_name)
                {
                    list.items.push((item_start, after));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((sheet, lists))
}

/// The children of an element as raw text, with their local names.
fn children(element: &str) -> Vec<(String, String)> {
    let mut reader = Reader::from_reader(element.as_bytes());
    reader.config_mut().check_end_names = false;
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut name = String::new();
    loop {
        let before = reader.buffer_position() as usize;
        let Ok(event) = reader.read_event() else {
            break;
        };
        let after = reader.buffer_position() as usize;
        match event {
            Event::Start(e) => {
                if depth == 1 {
                    start = before;
                    name = local(&e).to_owned();
                }
                depth += 1;
            }
            Event::Empty(e) if depth == 1 => {
                out.push((local(&e).to_owned(), element[before..after].to_owned()));
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 1 {
                    out.push((name.clone(), element[start..after].to_owned()));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// The start tag of an element and its name with prefix.
fn start_tag(element: &str) -> (&str, &str) {
    let end = element.find('>').map_or(element.len(), |i| i + 1);
    let tag = &element[..end];
    let name = tag[1..]
        .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .next()
        .unwrap_or("");
    (tag, name)
}

/// A font element made from `base` with the style's bold, italic,
/// underline, strikethrough and size; its colour, name and the rest stay.
fn font_xml(base: &str, p: &str, font: &aster_sheets::Font) -> String {
    let kept: String = children(base)
        .into_iter()
        .filter(|(name, _)| !matches!(name.as_str(), "b" | "i" | "u" | "strike" | "sz"))
        .map(|(_, raw)| raw)
        .collect();
    let mut out = format!("<{p}font>");
    if font.bold {
        out.push_str(&format!("<{p}b/>"));
    }
    if font.italic {
        out.push_str(&format!("<{p}i/>"));
    }
    if font.strike {
        out.push_str(&format!("<{p}strike/>"));
    }
    if font.underline {
        out.push_str(&format!("<{p}u/>"));
    }
    out.push_str(&format!(
        "<{p}sz val=\"{}\"/>",
        number(f64::from(font.size))
    ));
    out.push_str(&kept);
    out.push_str(&format!("</{p}font>"));
    out
}

fn color_xml(p: &str, color: Option<aster_sheets::Rgba>) -> String {
    match color {
        Some([r, g, b, _]) => format!("<{p}color rgb=\"FF{r:02X}{g:02X}{b:02X}\"/>"),
        None => format!("<{p}color auto=\"1\"/>"),
    }
}

fn fill_xml(p: &str, fill: Option<aster_sheets::Rgba>) -> String {
    match fill {
        Some([r, g, b, _]) => format!(
            "<{p}fill><{p}patternFill patternType=\"solid\"><{p}fgColor rgb=\"FF{r:02X}{g:02X}{b:02X}\"/><{p}bgColor indexed=\"64\"/></{p}patternFill></{p}fill>"
        ),
        None => format!("<{p}fill><{p}patternFill patternType=\"none\"/></{p}fill>"),
    }
}

fn border_xml(p: &str, borders: &aster_sheets::Borders) -> String {
    let side = |name: &str, line: &Option<aster_sheets::Line>| match line {
        Some(line) => format!(
            "<{p}{name} style=\"{}\">{}</{p}{name}>",
            line_style(line.style),
            color_xml(p, line.color)
        ),
        None => format!("<{p}{name}/>"),
    };
    format!(
        "<{p}border>{}{}{}{}<{p}diagonal/></{p}border>",
        side("left", &borders.left),
        side("right", &borders.right),
        side("top", &borders.top),
        side("bottom", &borders.bottom)
    )
}

fn line_style(style: aster_sheets::LineStyle) -> &'static str {
    use aster_sheets::LineStyle as L;
    match style {
        L::Hair => "hair",
        L::Thin => "thin",
        L::Medium => "medium",
        L::Thick => "thick",
        L::Double => "double",
        L::Dotted => "dotted",
        L::Dashed => "dashed",
        L::DashDot => "dashDot",
        L::DashDotDot => "dashDotDot",
        L::MediumDashed => "mediumDashed",
        L::MediumDashDot => "mediumDashDot",
        L::MediumDashDotDot => "mediumDashDotDot",
        L::SlantDashDot => "slantDashDot",
    }
}

/// An `<alignment>` for a style, `None` when everything is the default.
fn alignment_xml(p: &str, style: &aster_sheets::CellStyle) -> Option<String> {
    let mut attrs = String::new();
    if let Some(align) = style.align {
        let name = match align {
            aster_sheets::HAlign::Left => "left",
            aster_sheets::HAlign::Center => "center",
            aster_sheets::HAlign::Right => "right",
        };
        attrs.push_str(&format!(" horizontal=\"{name}\""));
    }
    match style.valign {
        aster_sheets::VAlign::Top => attrs.push_str(" vertical=\"top\""),
        aster_sheets::VAlign::Center => attrs.push_str(" vertical=\"center\""),
        aster_sheets::VAlign::Bottom => {}
    }
    if style.wrap {
        attrs.push_str(" wrapText=\"1\"");
    }
    if style.indent > 0 {
        attrs.push_str(&format!(" indent=\"{}\"", style.indent));
    }
    (!attrs.is_empty()).then(|| format!("<{p}alignment{attrs}/>"))
}

/// `styles.xml` with what the styles of `workbook` past the file's own
/// need: number formats in `<numFmts>`, fonts, fills and borders changed
/// here in their lists, and an `<xf>` for each at the end of `cellXfs`,
/// counts updated. Nothing is added twice; nothing else changes. `None`
/// when there is nothing to add.
fn append_formats(bytes: &[u8], part: &str, workbook: &Workbook) -> Result<Option<Vec<u8>>> {
    let (bom, xml) = split_bom(bytes);
    let (sheet, lists) = style_lists(xml, part)?;
    let Some((sheet_end, p)) = sheet else {
        return Ok(None);
    };
    let empty = List::default();
    let list = |name: &str| lists.get(name).unwrap_or(&empty);
    let xfs = list("cellXfs");
    if workbook.styles.len() <= xfs.items.len() || xfs.items.is_empty() || xfs.close.is_none() {
        return Ok(None);
    }
    let text = |(from, to): (usize, usize)| String::from_utf8_lossy(&xml[from..to]).into_owned();

    // Existing number formats, and what is added to each list.
    let mut formats: Vec<(u32, String)> = list("numFmts")
        .items
        .iter()
        .filter_map(|&span| {
            let element = text(span);
            let (tag, _) = start_tag(&element);
            let id = attribute(tag.as_bytes(), "numFmtId")?.parse().ok()?;
            Some((id, unescape(&attribute(tag.as_bytes(), "formatCode")?)))
        })
        .collect();
    let mut next_format = formats
        .iter()
        .map(|(id, _)| id + 1)
        .max()
        .unwrap_or(164)
        .max(164);
    let mut new_formats: Vec<String> = Vec::new();
    let mut added: HashMap<&str, Vec<String>> = HashMap::new();
    let mut add = |list_name: &'static str, existing: usize, element: String| -> u32 {
        let items = added.entry(list_name).or_default();
        let at = items.iter().position(|e| *e == element).unwrap_or_else(|| {
            items.push(element);
            items.len() - 1
        });
        (existing + at) as u32
    };
    let mut new_xfs = String::new();
    for style in &workbook.styles[xfs.items.len()..] {
        let base = style.xf.map_or(0, |i| i as usize).min(xfs.items.len() - 1);
        let base_xf = text(xfs.items[base]);
        let (base_tag, xf_name) = start_tag(&base_xf);
        let base_id = |name: &str| {
            attribute(base_tag.as_bytes(), name)
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0)
        };
        let format_id = if style.format_id == aster_sheets::style::UNNUMBERED_FORMAT {
            let code = style.format.code().to_owned();
            match formats.iter().find(|(_, c)| *c == code) {
                Some((id, _)) => *id,
                None => {
                    let id = next_format;
                    next_format += 1;
                    new_formats.push(format!(
                        "<{p}numFmt numFmtId=\"{id}\" formatCode=\"{}\"/>",
                        escape(&code)
                    ));
                    formats.push((id, code));
                    id
                }
            }
        } else {
            style.format_id
        };
        let fonts = list("fonts");
        let font_id = match style.font_id {
            Some(id) => id,
            None => {
                let base_font = fonts
                    .items
                    .get(base_id("fontId"))
                    .or(fonts.items.first())
                    .map(|&span| text(span))
                    .unwrap_or_else(|| format!("<{p}font/>"));
                add(
                    "fonts",
                    fonts.items.len(),
                    font_xml(&base_font, &p, &style.font),
                )
            }
        };
        let fills = list("fills");
        let fill_id = match (style.fill_id, style.fill) {
            (Some(id), _) => id,
            // Every file's first fill is "none".
            (None, None) => 0,
            (None, fill) => add("fills", fills.items.len(), fill_xml(&p, fill)),
        };
        let borders = list("borders");
        let border_id = match style.border_id {
            Some(id) => id,
            None if style.borders.is_empty() => 0,
            None => add(
                "borders",
                borders.items.len(),
                border_xml(&p, &style.borders),
            ),
        };
        let mut tag = base_tag.to_owned();
        for (name, value) in [
            ("numFmtId", format_id),
            ("fontId", font_id),
            ("fillId", fill_id),
            ("borderId", border_id),
        ] {
            tag = set_attribute(&tag, name, &value.to_string());
        }
        for (name, on) in [
            ("applyNumberFormat", format_id != 0),
            ("applyFont", style.font_id.is_none()),
            ("applyFill", style.fill_id.is_none()),
            ("applyBorder", style.border_id.is_none()),
        ] {
            if on {
                tag = set_attribute(&tag, name, "1");
            }
        }
        // A changed alignment is the style's own; otherwise, and for the
        // other children (protection), the base's stay as they were.
        let aligned_alike = workbook.styles.get(base).is_some_and(|b| {
            (b.align, b.valign, b.wrap, b.indent)
                == (style.align, style.valign, style.wrap, style.indent)
        });
        let inner: String = if aligned_alike {
            children(&base_xf).into_iter().map(|(_, raw)| raw).collect()
        } else {
            tag = set_attribute(&tag, "applyAlignment", "1");
            let others: String = children(&base_xf)
                .into_iter()
                .filter(|(name, _)| name != "alignment")
                .map(|(_, raw)| raw)
                .collect();
            format!("{}{others}", alignment_xml(&p, style).unwrap_or_default())
        };
        let open = tag
            .trim_end_matches('>')
            .trim_end_matches('/')
            .trim_end()
            .to_owned();
        if inner.is_empty() {
            new_xfs.push_str(&format!("{open}/>"));
        } else {
            new_xfs.push_str(&format!("{open}>{inner}</{xf_name}>"));
        }
    }

    // Edits, in the order of the file.
    let mut ops: Vec<(usize, usize, String)> = Vec::new();
    let extend = |ops: &mut Vec<(usize, usize, String)>,
                  name: &str,
                  list: &List,
                  items: &[String],
                  fallback: usize| {
        if items.is_empty() {
            return;
        }
        let total = list.items.len() + items.len();
        let joined: String = items.concat();
        match (list.open, list.close) {
            (Some((start, end)), Some(close)) => {
                let tag = text((start, end));
                ops.push((start, end, set_attribute(&tag, "count", &total.to_string())));
                ops.push((close, close, joined));
            }
            (Some((start, end)), None) => {
                let tag = set_attribute(&text((start, end)), "count", &total.to_string());
                let open = tag
                    .trim_end_matches('>')
                    .trim_end_matches('/')
                    .trim_end()
                    .to_owned();
                ops.push((start, end, format!("{open}>{joined}</{p}{name}>")));
            }
            (None, _) => ops.push((
                fallback,
                fallback,
                format!("<{p}{name} count=\"{total}\">{joined}</{p}{name}>"),
            )),
        }
    };
    extend(
        &mut ops,
        "numFmts",
        list("numFmts"),
        &new_formats,
        sheet_end,
    );
    for name in ["fonts", "fills", "borders"] {
        let items = added.get(name).cloned().unwrap_or_default();
        let fallback = list("cellXfs").open.map_or(sheet_end, |(start, _)| start);
        extend(&mut ops, name, list(name), &items, fallback);
    }
    if let (Some((start, end)), Some(close)) = (xfs.open, xfs.close) {
        let tag = text((start, end));
        ops.push((
            start,
            end,
            set_attribute(&tag, "count", &workbook.styles.len().to_string()),
        ));
        ops.push((close, close, new_xfs));
    }
    ops.sort_by_key(|(start, ..)| *start);
    let mut out = Vec::with_capacity(bytes.len() + 512);
    out.extend_from_slice(bom);
    let mut at = 0;
    for (start, end, text) in ops {
        if start < at {
            return Err(Error::broken(part, "overlapping changes"));
        }
        out.extend_from_slice(&xml[at..start]);
        out.extend_from_slice(text.as_bytes());
        at = end;
    }
    out.extend_from_slice(&xml[at..]);
    Ok(Some(out))
}

/// An attribute value as written, with XML's five entities resolved.
fn unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

// --- Small edits of other parts ---

/// The part without the first element named `name` whose attributes pass
/// `pick`. `None` when there is no such element.
fn remove_element(
    bytes: &[u8],
    part: &str,
    name: &str,
    pick: impl Fn(&dyn Fn(&str) -> Option<String>) -> bool,
) -> Result<Option<Vec<u8>>> {
    let (bom, xml) = split_bom(bytes);
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().check_end_names = false;
    let mut open: Option<usize> = None;
    loop {
        let before = reader.buffer_position() as usize;
        let event = reader.read_event().map_err(|e| Error::broken(part, e))?;
        let after = reader.buffer_position() as usize;
        match event {
            Event::Empty(e) | Event::Start(e) if open.is_none() && local(&e) == name => {
                let attrs = attributes(&e);
                let get = |key: &str| find(&attrs, key).map(str::to_owned);
                if pick(&get) {
                    if xml[after - 2] == b'/' {
                        return Ok(Some(splice(bom, xml, before, after)));
                    }
                    open = Some(before);
                }
            }
            Event::End(e) if e.local_name().as_ref() == name => {
                if let Some(start) = open {
                    return Ok(Some(splice(bom, xml, start, after)));
                }
            }
            Event::Eof => return Ok(None),
            _ => {}
        }
    }
}

fn splice(bom: &[u8], xml: &[u8], start: usize, end: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(xml.len());
    out.extend_from_slice(bom);
    out.extend_from_slice(&xml[..start]);
    out.extend_from_slice(&xml[end..]);
    out
}

/// The value of attribute `name` in a start tag, as written.
fn attribute(tag: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(tag);
    let pattern = format!(" {name}=");
    let at = text.find(&pattern)? + pattern.len();
    let quote = text[at..].chars().next()?;
    let rest = &text[at + 1..];
    Some(rest[..rest.find(quote)?].to_owned())
}

/// A start tag with attribute `name` set to `value`, added if missing.
fn set_attribute(tag: &str, name: &str, value: &str) -> String {
    let pattern = format!(" {name}=");
    if let Some(found) = tag.find(&pattern) {
        let at = found + pattern.len();
        let quote = tag[at..].chars().next().unwrap_or('"');
        if let Some(len) = tag[at + 1..].find(quote) {
            return format!(
                "{}{quote}{value}{quote}{}",
                &tag[..at],
                &tag[at + 1 + len + 1..]
            );
        }
    }
    let end = tag.trim_end_matches('>').trim_end_matches('/');
    let closing = &tag[end.len()..];
    format!("{end} {name}=\"{value}\"{closing}")
}

/// A new workbook with one empty sheet, as a package. `font` is the Normal
/// style's font (`Calibri`, `等线` ...), which sets the column width unit;
/// the formats new entries may take (dates, times, percentages, thousands)
/// are there from the start, so typing a date gives a date format without
/// changing the styles part.
pub fn blank(sheet_name: &str, font: &str) -> Vec<u8> {
    const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const PKG_REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
    const CT: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml";
    let types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="{CT}.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="{CT}.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="{CT}.styles+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="{CT}.sharedStrings+xml"/></Types>"#
    );
    let root_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PKG_REL}"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#
    );
    let workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{MAIN}" xmlns:r="{REL}"><bookViews><workbookView/></bookViews><sheets><sheet name="{}" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        escape(sheet_name)
    );
    let workbook_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PKG_REL}"><Relationship Id="rId1" Type="{REL}/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="{REL}/styles" Target="styles.xml"/><Relationship Id="rId3" Type="{REL}/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#
    );
    let sheet = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{MAIN}" xmlns:r="{REL}"><sheetViews><sheetView tabSelected="1" workbookViewId="0"/></sheetViews><sheetFormatPr defaultRowHeight="15"/><sheetData/><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/></worksheet>"#
    );
    // General, then the formats an entry may take: date, date and time,
    // time, short time, percent, percent with decimals, thousands.
    let formats = [0, 14, 22, 21, 20, 9, 10, 3, 4];
    let xfs: String = formats
        .iter()
        .map(|id| {
            let apply = if *id == 0 {
                ""
            } else {
                r#" applyNumberFormat="1""#
            };
            format!(r#"<xf numFmtId="{id}" fontId="0" fillId="0" borderId="0" xfId="0"{apply}/>"#)
        })
        .collect();
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{MAIN}"><fonts count="1"><font><sz val="11"/><color theme="1"/><name val="{}"/><family val="2"/><scheme val="minor"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="{}">{xfs}</cellXfs><cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles></styleSheet>"#,
        escape(font),
        formats.len()
    );
    let strings = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="{MAIN}" count="0" uniqueCount="0"/>"#
    );
    let parts: [(&str, &str); 7] = [
        ("[Content_Types].xml", &types),
        ("_rels/.rels", &root_rels),
        ("xl/workbook.xml", &workbook),
        ("xl/_rels/workbook.xml.rels", &workbook_rels),
        ("xl/worksheets/sheet1.xml", &sheet),
        ("xl/styles.xml", &styles),
        ("xl/sharedStrings.xml", &strings),
    ];
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, text) in parts {
        // Writing to memory does not fail.
        let _ = writer.start_file(name, options);
        let _ = std::io::Write::write_all(&mut writer, text.as_bytes());
    }
    writer
        .finish()
        .map(|cursor| cursor.into_inner())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_escaped_for_xml_and_ooxml() {
        assert_eq!(escape("a<b & \"c\""), "a&lt;b &amp; &quot;c&quot;");
        assert_eq!(escape("行1\r\n行2\u{1}"), "行1_x000D_\n行2_x0001_");
        assert_eq!(escape("_x0041_ and _x"), "_x005F_x0041_ and _x");
        assert_eq!(
            text_element("", " pad "),
            "<t xml:space=\"preserve\"> pad </t>"
        );
        assert_eq!(text_element("x:", "東京"), "<x:t>東京</x:t>");
    }

    #[test]
    fn numbers_read_back_the_same() {
        for value in [0.0, 1.0, -2.5, 0.1, 1e-7, 1.5e21, 46350.75, 123456789012.0] {
            assert_eq!(number(value).parse::<f64>().unwrap(), value, "{value}");
        }
        assert_eq!(number(3.0), "3");
        assert_eq!(number(0.1), "0.1");
    }

    #[test]
    fn attributes_are_set_in_place() {
        let tag = r#"<sst xmlns="x" count="10" uniqueCount="4">"#;
        assert_eq!(attribute(tag.as_bytes(), "count").as_deref(), Some("10"));
        assert_eq!(
            set_attribute(tag, "uniqueCount", "6"),
            r#"<sst xmlns="x" count="10" uniqueCount="6">"#
        );
        assert_eq!(
            set_attribute("<sst a='1'/>", "count", "2"),
            "<sst a='1' count=\"2\"/>"
        );
    }

    #[test]
    fn elements_are_taken_out_byte_exactly() {
        let rels = br#"<?xml version="1.0"?>
<Relationships xmlns="r"><Relationship Id="rId1" Type="t/worksheet" Target="a"/><Relationship Id="rId9" Type="t/calcChain" Target="calcChain.xml"/></Relationships>"#;
        let out = remove_element(rels, "rels", "Relationship", |attr| {
            attr("Type").is_some_and(|t| t.ends_with("/calcChain"))
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "<?xml version=\"1.0\"?>\n<Relationships xmlns=\"r\"><Relationship Id=\"rId1\" Type=\"t/worksheet\" Target=\"a\"/></Relationships>"
        );
        assert!(
            remove_element(rels, "rels", "Override", |_| true)
                .unwrap()
                .is_none()
        );
    }
}
