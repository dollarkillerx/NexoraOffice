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

use aster_sheets::style::{self, Rgba};
use aster_sheets::{
    Anchor, Axis, Borders, Cell, CellRange, CellRef, CellStyle, Font, Format, HAlign, Line,
    LineStyle, MAX_COLUMNS, MAX_ROWS, Picture, Placement, Sheet, SheetBuilder, SheetKind, VAlign,
    Value, Workbook,
};

use crate::drawingml::{Theme, parse_hex, read_theme};
use crate::error::{Error, Result};
use crate::package::{Kind, Package};
use crate::xml::{Ev, Tag, XmlReader, decode_escapes, each_child};

/// Pixels per point at 96 dpi.
const PX_PER_POINT: f64 = 96.0 / 72.0;
/// EMU per logical pixel at 96 dpi.
const EMU_PER_PX: f64 = 9_525.0;

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
    let theme = match target_of("theme") {
        // A theme that does not parse is not worth failing the workbook.
        Some(part) if package.has(&part) => {
            read_theme(&package.read(&part)?, &part).unwrap_or_default()
        }
        _ => Theme::default(),
    };
    let (styles, normal_font) = match target_of("styles") {
        Some(part) if package.has(&part) => styles(&package.read(&part)?, &part, &theme)?,
        _ => (Vec::new(), Font::default()),
    };
    let digit = digit_width(&normal_font);

    let book = package.read(&main)?;
    let entries = workbook_entries(&book, &main)?;
    let mut workbook = Workbook {
        sheets: Vec::with_capacity(entries.sheets.len()),
        styles: Vec::new(),
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
                let context = SheetContext {
                    strings: &strings,
                    styles: &styles,
                    digit,
                };
                let (mut builder, drawing) = worksheet(&bytes, &target, entry.name, &context)?;
                if let Some(drawing) = drawing {
                    let rels = package.relationships(&target)?;
                    if let Some(part) = rels
                        .into_iter()
                        .find(|rel| rel.id == drawing)
                        .and_then(|rel| rel.target)
                        .filter(|part| package.has(part))
                    {
                        builder.pictures = pictures(package, &part)?;
                    }
                }
                builder.finish()
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
    workbook.styles = styles;
    Ok(workbook)
}

pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) rel: String,
    pub(crate) hidden: bool,
}

pub(crate) struct Entries {
    pub(crate) sheets: Vec<Entry>,
    date1904: bool,
    active: usize,
}

pub(crate) fn workbook_entries(bytes: &[u8], part: &str) -> Result<Entries> {
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

pub(crate) fn shared_strings(bytes: &[u8], part: &str) -> Result<Vec<Arc<str>>> {
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

/// Fonts, fills and borders by index, and the colours they refer to.
struct StyleParts {
    custom: HashMap<u32, String>,
    fonts: Vec<Arc<Font>>,
    fills: Vec<Option<Rgba>>,
    borders: Vec<Arc<Borders>>,
    xfs: Vec<Xf>,
}

struct Xf {
    format: u32,
    font: usize,
    fill: usize,
    border: usize,
    align: Option<HAlign>,
    valign: VAlign,
    wrap: bool,
    indent: u8,
}

/// What a `<color>` element refers to.
struct Colors<'a> {
    theme: &'a Theme,
    /// The file's own palette, when it replaces the default one.
    indexed: Vec<Rgba>,
}

impl Colors<'_> {
    /// `None` for automatic.
    fn of(&self, tag: &Tag<'_>) -> Option<Rgba> {
        if tag.attr_bool("auto").unwrap_or(false) {
            return None;
        }
        let base = if let Some(rgb) = tag.attr("rgb") {
            parse_hex(&rgb)?
        } else if let Some(index) = tag.attr_i64("theme") {
            self.theme.indexed(usize::try_from(index).ok()?)?
        } else {
            let index = tag.attr_i64("indexed")?;
            let index = usize::try_from(index).ok()?;
            match self.indexed.get(index) {
                Some(color) => *color,
                None if index < 64 => style::rgb(style::INDEXED_COLORS[index]),
                // 64 is "system foreground", 65 "system background".
                None => return None,
            }
        };
        Some(match tag.attr_f64("tint") {
            Some(tint) => style::tint(base, tint),
            None => base,
        })
    }
}

fn styles(bytes: &[u8], part: &str, theme: &Theme) -> Result<(Vec<CellStyle>, Font)> {
    // Custom palettes come after the formats in the file but apply to them,
    // so look for one first.
    let indexed = indexed_colors(bytes, part)?;
    let colors = Colors { theme, indexed };
    let mut parts = StyleParts {
        custom: HashMap::new(),
        fonts: Vec::new(),
        fills: Vec::new(),
        borders: Vec::new(),
        xfs: Vec::new(),
    };
    let mut normal_font = 0usize;
    let mut reader = XmlReader::new(bytes, part);
    loop {
        match reader.next()? {
            Ev::Open(tag) if tag.is("numFmt") => {
                if let (Some(id), Some(code)) = (tag.attr_i64("numFmtId"), tag.attr("formatCode")) {
                    parts
                        .custom
                        .insert(id.clamp(0, i64::from(u32::MAX)) as u32, code);
                }
            }
            Ev::Open(tag) if tag.is("fonts") => {
                each_child(&mut reader, |reader, font| {
                    if font.is("font") {
                        parts.fonts.push(Arc::new(read_font(reader, &colors)?));
                        Ok(())
                    } else {
                        reader.skip()
                    }
                })?;
            }
            Ev::Open(tag) if tag.is("fills") => {
                each_child(&mut reader, |reader, fill| {
                    if fill.is("fill") {
                        parts.fills.push(read_fill(reader, &colors)?);
                        Ok(())
                    } else {
                        reader.skip()
                    }
                })?;
            }
            Ev::Open(tag) if tag.is("borders") => {
                each_child(&mut reader, |reader, border| {
                    if border.is("border") {
                        parts.borders.push(Arc::new(read_border(reader, &colors)?));
                        Ok(())
                    } else {
                        reader.skip()
                    }
                })?;
            }
            // The Normal style's font sets the column width unit.
            Ev::Open(tag) if tag.is("cellStyleXfs") => {
                let xfs = cell_xfs(&mut reader)?;
                normal_font = xfs.first().map_or(0, |xf| xf.font);
            }
            Ev::Open(tag) if tag.is("cellXfs") => parts.xfs = cell_xfs(&mut reader)?,
            Ev::Open(tag) if tag.is("dxfs") || tag.is("colors") || tag.is("extLst") => {
                reader.skip()?
            }
            Ev::Eof => break,
            _ => {}
        }
    }
    let mut formats: HashMap<u32, Arc<Format>> = HashMap::new();
    let default_font = Arc::new(Font::default());
    let default_borders = Arc::new(Borders::default());
    let styles = parts
        .xfs
        .iter()
        .map(|xf| {
            let format = formats
                .entry(xf.format)
                .or_insert_with(|| {
                    Arc::new(match parts.custom.get(&xf.format) {
                        Some(code) => Format::parse(code),
                        None => Format::builtin(xf.format),
                    })
                })
                .clone();
            CellStyle {
                format,
                align: xf.align,
                valign: xf.valign,
                wrap: xf.wrap,
                indent: xf.indent,
                font: parts
                    .fonts
                    .get(xf.font)
                    .cloned()
                    .unwrap_or_else(|| default_font.clone()),
                fill: parts.fills.get(xf.fill).copied().flatten(),
                borders: parts
                    .borders
                    .get(xf.border)
                    .cloned()
                    .unwrap_or_else(|| default_borders.clone()),
            }
        })
        .collect();
    let normal = parts
        .fonts
        .get(normal_font)
        .or(parts.fonts.first())
        .map(|font| (**font).clone())
        .unwrap_or_default();
    Ok((styles, normal))
}

/// `<colors><indexedColors>`: a workbook's own 64-colour palette.
fn indexed_colors(bytes: &[u8], part: &str) -> Result<Vec<Rgba>> {
    let mut reader = XmlReader::new(bytes, part);
    let mut palette = Vec::new();
    loop {
        match reader.next()? {
            Ev::Open(tag) if tag.is("indexedColors") => {
                each_child(&mut reader, |reader, color| {
                    if color.is("rgbColor") {
                        palette.push(
                            color
                                .attr("rgb")
                                .as_deref()
                                .and_then(parse_hex)
                                .unwrap_or([0, 0, 0, 0xff]),
                        );
                    }
                    reader.skip()
                })?;
                return Ok(palette);
            }
            Ev::Open(tag) if tag.is("cellXfs") || tag.is("cellStyleXfs") => reader.skip()?,
            Ev::Eof => return Ok(palette),
            _ => {}
        }
    }
}

/// `<b/>` is on, `<b val="0"/>` is off.
fn flag(tag: &Tag<'_>) -> bool {
    tag.attr_bool("val").unwrap_or(true)
}

fn read_font(reader: &mut XmlReader<'_>, colors: &Colors<'_>) -> Result<Font> {
    let mut font = Font::default();
    each_child(reader, |reader, child| {
        match child.local() {
            b"b" => font.bold = flag(&child),
            b"i" => font.italic = flag(&child),
            b"strike" => font.strike = flag(&child),
            b"u" => font.underline = child.attr("val").as_deref() != Some("none"),
            b"sz" => {
                if let Some(size) = child.attr_f64("val").filter(|s| *s > 0.0 && *s < 1000.0) {
                    font.size = size as f32;
                }
            }
            b"color" => font.color = colors.of(&child),
            b"name" => font.name = child.attr("val").map(Arc::from),
            _ => {}
        }
        reader.skip()
    })?;
    Ok(font)
}

fn read_fill(reader: &mut XmlReader<'_>, colors: &Colors<'_>) -> Result<Option<Rgba>> {
    let mut fill = None;
    each_child(reader, |reader, child| {
        if child.is("patternFill") {
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
                // A pattern without a type is solid when it names a colour
                // (as in conditional formats).
                Some("none") => None,
                Some("solid") | None => fg.or(bg),
                // Other patterns: a dot pattern drawn flat is its mix of the
                // two colours.
                Some(kind) => {
                    let coverage = match kind {
                        "gray0625" => 0.0625,
                        "gray125" => 0.125,
                        "lightGray" => 0.25,
                        "mediumGray" => 0.5,
                        "darkGray" => 0.75,
                        _ => 0.5,
                    };
                    let fg = fg.unwrap_or([0, 0, 0, 0xff]);
                    let bg = bg.unwrap_or([0xff, 0xff, 0xff, 0xff]);
                    Some(mix(fg, bg, coverage))
                }
            };
            Ok(())
        } else if child.is("gradientFill") {
            let mut first = None;
            each_child(reader, |reader, stop| {
                if stop.is("stop") {
                    each_child(reader, |reader, color| {
                        if color.is("color") && first.is_none() {
                            first = colors.of(&color);
                        }
                        reader.skip()
                    })
                } else {
                    reader.skip()
                }
            })?;
            fill = first;
            Ok(())
        } else {
            reader.skip()
        }
    })?;
    Ok(fill)
}

fn mix(fg: Rgba, bg: Rgba, coverage: f64) -> Rgba {
    let channel = |i: usize| {
        (f64::from(fg[i]) * coverage + f64::from(bg[i]) * (1.0 - coverage)).round() as u8
    };
    [channel(0), channel(1), channel(2), 0xff]
}

fn read_border(reader: &mut XmlReader<'_>, colors: &Colors<'_>) -> Result<Borders> {
    let mut borders = Borders::default();
    each_child(reader, |reader, side| {
        let slot = match side.local() {
            b"left" | b"start" => &mut borders.left,
            b"right" | b"end" => &mut borders.right,
            b"top" => &mut borders.top,
            b"bottom" => &mut borders.bottom,
            _ => return reader.skip(),
        };
        let style = side.attr("style").as_deref().and_then(LineStyle::parse);
        let mut color = None;
        each_child(reader, |reader, child| {
            if child.is("color") {
                color = colors.of(&child);
            }
            reader.skip()
        })?;
        *slot = style.map(|style| Line { style, color });
        Ok(())
    })?;
    Ok(borders)
}

/// Width of a digit of the workbook's Normal font, pixels, which column
/// widths are counted in. Excel measures the font; we know the usual ones:
/// Calibri, Arial and Aptos 11 are 7 pixels, the East Asian defaults (等线,
/// 宋体, 游ゴシック, ＭＳ Ｐゴシック, 맑은 고딕, 新細明體) 8.
pub fn digit_width(font: &Font) -> f64 {
    let east_asian = font.name.as_deref().is_some_and(|name| {
        !name.is_ascii()
            || [
                "DengXian",
                "SimSun",
                "NSimSun",
                "Microsoft YaHei",
                "Microsoft JhengHei",
                "PMingLiU",
                "MingLiU",
                "Yu Gothic",
                "MS PGothic",
                "MS Gothic",
                "Meiryo",
                "Malgun Gothic",
                "Gulim",
                "Dotum",
                "Batang",
            ]
            .iter()
            .any(|known| name.eq_ignore_ascii_case(known))
    });
    // Digits are about half an em wide in Latin fonts and a little more in
    // the East Asian ones; 11 points is 14.67 pixels.
    let ratio = if east_asian { 0.55 } else { 0.5 };
    (f64::from(font.size) * PX_PER_POINT * ratio)
        .round()
        .max(1.0)
}

/// Excel's column width (in digits, with padding) to logical pixels.
fn column_px(width: f64, digit: f64) -> f64 {
    (width * digit + 0.5).floor().max(0.0)
}

struct SheetContext<'a> {
    strings: &'a [Arc<str>],
    styles: &'a [CellStyle],
    /// Digit width of the Normal font, pixels.
    digit: f64,
}

impl SheetContext<'_> {
    fn decorates(&self, style: u32) -> bool {
        self.styles
            .get(style as usize)
            .is_some_and(CellStyle::is_visible_when_empty)
    }
}

/// The sheet and the relationship id of its drawing, if it has one.
fn worksheet(
    bytes: &[u8],
    part: &str,
    name: String,
    context: &SheetContext<'_>,
) -> Result<(SheetBuilder, Option<String>)> {
    let digit = context.digit;
    let mut drawing = None;
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
                    Some(width) => column_px(width, digit),
                    // Base width plus padding, rounded up to a multiple of 8.
                    None => ((base * digit + 5.0) / 8.0).ceil() * 8.0,
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
                    Some(width) => Some(column_px(width, digit)),
                    None => None,
                };
                if let Some(width) = width {
                    builder.columns.set(min as u32 - 1, max as u32, width);
                }
                let style = tag
                    .attr_i64("style")
                    .unwrap_or(0)
                    .clamp(0, i64::from(u32::MAX)) as u32;
                if style != 0 && context.decorates(style) {
                    builder
                        .column_styles
                        .push((min as u32 - 1, max as u32 - 1, style));
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
                if tag.attr_bool("customFormat").unwrap_or(false) {
                    let style = tag.attr_i64("s").unwrap_or(0).clamp(0, i64::from(u32::MAX)) as u32;
                    if context.decorates(style) || !builder.column_styles.is_empty() {
                        builder.row_styles.push((index, style));
                    }
                }
                read_row(&mut reader, index, &mut builder, context, &mut shared)?;
            }
            Ev::Open(tag) if tag.is("mergeCell") => {
                if let Some(range) = tag.attr("ref").as_deref().and_then(CellRange::parse) {
                    builder.merge(range);
                }
            }
            Ev::Open(tag) if tag.is("sheetView") => {
                builder.show_grid = tag.attr_bool("showGridLines").unwrap_or(true);
                each_child(&mut reader, |reader, child| {
                    if child.is("pane")
                        && matches!(
                            child.attr("state").as_deref(),
                            Some("frozen" | "frozenSplit")
                        )
                    {
                        let rows = child.attr_f64("ySplit").unwrap_or(0.0);
                        let cols = child.attr_f64("xSplit").unwrap_or(0.0);
                        builder.frozen = (
                            rows.clamp(0.0, f64::from(MAX_ROWS - 1)) as u32,
                            cols.clamp(0.0, f64::from(MAX_COLUMNS - 1)) as u32,
                        );
                    }
                    reader.skip()
                })?;
            }
            Ev::Open(tag) if tag.is("drawing") => {
                drawing = tag.attr("id");
                reader.skip()?;
            }
            Ev::Open(tag)
                if tag.is("conditionalFormatting")
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
    Ok((builder, drawing))
}

fn read_row(
    reader: &mut XmlReader<'_>,
    row: u32,
    builder: &mut SheetBuilder,
    context: &SheetContext<'_>,
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
                let cell = read_cell(reader, &tag, CellRef::new(row, col), context, shared)?;
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

fn cell_xfs(reader: &mut XmlReader<'_>) -> Result<Vec<Xf>> {
    let mut xfs = Vec::new();
    let index = |tag: &Tag<'_>, name: &str| {
        tag.attr_i64(name)
            .unwrap_or(0)
            .clamp(0, i64::from(u32::MAX)) as u32
    };
    each_child(reader, |reader, tag| {
        if !tag.is("xf") {
            return reader.skip();
        }
        let mut xf = Xf {
            format: index(&tag, "numFmtId"),
            font: index(&tag, "fontId") as usize,
            fill: index(&tag, "fillId") as usize,
            border: index(&tag, "borderId") as usize,
            align: None,
            valign: VAlign::Bottom,
            wrap: false,
            indent: 0,
        };
        each_child(reader, |reader, child| {
            if child.is("alignment") {
                xf.align = match child.attr("horizontal").as_deref() {
                    Some("left") => Some(HAlign::Left),
                    Some("center" | "centerContinuous") => Some(HAlign::Center),
                    Some("right") => Some(HAlign::Right),
                    _ => None,
                };
                xf.valign = match child.attr("vertical").as_deref() {
                    Some("top") => VAlign::Top,
                    Some("center" | "justify" | "distributed") => VAlign::Center,
                    _ => VAlign::Bottom,
                };
                xf.wrap = child.attr_bool("wrapText").unwrap_or(false)
                    || matches!(
                        child.attr("horizontal").as_deref(),
                        Some("justify" | "distributed")
                    );
                xf.indent = child.attr_i64("indent").unwrap_or(0).clamp(0, 250) as u8;
            }
            reader.skip()
        })?;
        xfs.push(xf);
        Ok(())
    })?;
    Ok(xfs)
}

fn read_cell(
    reader: &mut XmlReader<'_>,
    tag: &Tag<'_>,
    at: CellRef,
    context: &SheetContext<'_>,
    shared: &mut HashMap<u32, (Arc<str>, CellRef)>,
) -> Result<Option<Cell>> {
    let strings = context.strings;
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
    if value == Value::Empty && formula.is_none() && !context.decorates(style) {
        // A formatted but empty cell that shows nothing: no need to keep it.
        return Ok(None);
    }
    Ok(Some(Cell {
        col: at.col,
        value,
        style,
        formula,
    }))
}

/// A cell position in a drawing: `<xdr:from>` or `<xdr:to>`.
fn read_anchor(reader: &mut XmlReader<'_>) -> Result<Anchor> {
    let mut anchor = Anchor::default();
    each_child(reader, |reader, child| {
        let value = reader.text()?.trim().parse::<i64>().unwrap_or(0);
        match child.local() {
            b"col" => anchor.cell.col = value.clamp(0, i64::from(MAX_COLUMNS - 1)) as u32,
            b"row" => anchor.cell.row = value.clamp(0, i64::from(MAX_ROWS - 1)) as u32,
            b"colOff" => anchor.dx = value as f64 / EMU_PER_PX,
            b"rowOff" => anchor.dy = value as f64 / EMU_PER_PX,
            _ => {}
        }
        Ok(())
    })?;
    Ok(anchor)
}

/// What an anchor holds: a picture (name and image relationship) or another
/// graphic such as a chart (name only).
#[derive(Default)]
struct Content {
    found: bool,
    name: String,
    embed: Option<String>,
}

fn read_content(reader: &mut XmlReader<'_>, tag: &Tag<'_>, content: &mut Content) -> Result<()> {
    match tag.local() {
        b"pic" | b"graphicFrame" => {
            if content.found {
                return reader.skip();
            }
            content.found = true;
            let mut depth = 1usize;
            loop {
                match reader.next()? {
                    Ev::Open(child) => {
                        if child.is("cNvPr") && content.name.is_empty() {
                            content.name = child.attr("name").unwrap_or_default();
                        } else if child.is("blip") {
                            content.embed = child.attr("embed");
                        }
                        depth += 1;
                    }
                    Ev::Close => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(());
                        }
                    }
                    Ev::Text(_) => {}
                    Ev::Eof => return Err(Error::broken(reader.part(), "ends inside a picture")),
                }
            }
        }
        // Newer content wrapped for older readers: take the first choice.
        b"AlternateContent" | b"Choice" | b"Fallback" => each_child(reader, |reader, child| {
            read_content(reader, &child, content)
        }),
        _ => reader.skip(),
    }
}

/// Pictures (and the frames of charts) of a drawing part.
fn pictures(package: &mut Package, part: &str) -> Result<Vec<Picture>> {
    let bytes = package.read(part)?;
    let rels: HashMap<String, String> = package
        .relationships(part)?
        .into_iter()
        .filter_map(|rel| Some((rel.id, rel.target?)))
        .collect();
    let mut found: Vec<(Placement, Content)> = Vec::new();
    let mut reader = XmlReader::new(&bytes, part);
    loop {
        match reader.next()? {
            Ev::Open(tag)
                if tag.is("twoCellAnchor")
                    || tag.is("oneCellAnchor")
                    || tag.is("absoluteAnchor") =>
            {
                let mut from = None;
                let mut to = None;
                let mut position = (0.0, 0.0);
                let mut size = None;
                let mut content = Content::default();
                each_child(&mut reader, |reader, child| match child.local() {
                    b"from" => {
                        from = Some(read_anchor(reader)?);
                        Ok(())
                    }
                    b"to" => {
                        to = Some(read_anchor(reader)?);
                        Ok(())
                    }
                    b"pos" => {
                        position = (
                            child.attr_f64("x").unwrap_or(0.0) / EMU_PER_PX,
                            child.attr_f64("y").unwrap_or(0.0) / EMU_PER_PX,
                        );
                        reader.skip()
                    }
                    b"ext" => {
                        size = Some((
                            child.attr_f64("cx").unwrap_or(0.0).max(0.0) / EMU_PER_PX,
                            child.attr_f64("cy").unwrap_or(0.0).max(0.0) / EMU_PER_PX,
                        ));
                        reader.skip()
                    }
                    _ => read_content(reader, &child, &mut content),
                })?;
                let placement = match (from, to, size) {
                    (Some(from), Some(to), _) => Placement::TwoCell { from, to },
                    (Some(from), None, Some((width, height))) => Placement::OneCell {
                        from,
                        width,
                        height,
                    },
                    (None, _, Some((width, height))) => Placement::Absolute {
                        x: position.0,
                        y: position.1,
                        width,
                        height,
                    },
                    _ => continue,
                };
                if content.found {
                    found.push((placement, content));
                }
            }
            Ev::Eof => break,
            _ => {}
        }
    }
    let mut out = Vec::with_capacity(found.len());
    for (placement, content) in found {
        let image = match content.embed.and_then(|id| rels.get(&id).cloned()) {
            Some(target) if package.has(&target) => Some(Arc::from(package.read(&target)?)),
            _ => None,
        };
        out.push(Picture {
            placement,
            image,
            name: content.name,
        });
    }
    Ok(out)
}
