//! Workbooks, sheets and cells.

use std::sync::Arc;

use crate::address::{CellRange, CellRef, MAX_COLUMNS, MAX_ROWS};
use crate::axis::Axis;
use crate::format::Format;
use crate::style::{CellStyle, HAlign};

/// Default column width and row height of a new Excel sheet, logical pixels.
pub const DEFAULT_COLUMN_WIDTH: f64 = 64.0;
pub const DEFAULT_ROW_HEIGHT: f64 = 20.0;

/// What a cell holds.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Value {
    #[default]
    Empty,
    Number(f64),
    Text(Arc<str>),
    Bool(bool),
    /// `#DIV/0!`, `#N/A` and so on, as written.
    Error(Arc<str>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub col: u32,
    pub value: Value,
    /// Index into [`Workbook::styles`]; out-of-range indices use the default.
    pub style: u32,
    /// Formula text without the leading `=`. The value is the result the file
    /// was saved with; `Value::Empty` with a formula means none was saved.
    pub formula: Option<Arc<str>>,
}

impl Cell {
    pub fn new(col: u32, value: Value) -> Self {
        Self {
            col,
            value,
            style: 0,
            formula: None,
        }
    }

    /// No value and no formula: a cell kept only for its fill or borders.
    pub fn is_blank(&self) -> bool {
        self.value == Value::Empty && self.formula.is_none()
    }
}

/// Where a picture is fastened to the sheet: a cell and an offset into it,
/// logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Anchor {
    pub cell: CellRef,
    pub dx: f64,
    pub dy: f64,
}

/// How a picture is placed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    /// Stretches between two cell positions as columns and rows change.
    TwoCell { from: Anchor, to: Anchor },
    /// Starts at a cell position and keeps its size, logical pixels.
    OneCell {
        from: Anchor,
        width: f64,
        height: f64,
    },
    /// At a fixed position on the sheet, logical pixels.
    Absolute {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
}

/// A picture on a sheet. The image is the file's own bytes (PNG, JPEG or
/// something else), decoded when it is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub placement: Placement,
    pub image: Option<Arc<[u8]>>,
    /// The name the file gives it, such as `Picture 1`.
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub index: u32,
    /// Sorted by column, one per column.
    pub cells: Vec<Cell>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetKind {
    Worksheet,
    /// A sheet that holds one chart. Charts are not drawn yet.
    Chart,
    /// Macro and dialog sheets from old versions of Excel.
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub name: String,
    pub kind: SheetKind,
    pub hidden: bool,
    /// Sorted by index.
    rows: Vec<Row>,
    pub columns: Axis,
    pub row_sizes: Axis,
    pub merges: Vec<CellRange>,
    used: Option<CellRange>,
    cell_count: usize,
    /// Rows and columns frozen at the top and left: they stay put while the
    /// rest scrolls.
    pub frozen: (u32, u32),
    /// Grid lines are drawn (Excel's "View > Gridlines").
    pub show_grid: bool,
    /// Styles of whole columns, sorted runs of `first..=last`, for cells
    /// that are not in the file.
    pub column_styles: Vec<(u32, u32, u32)>,
    /// Styles of whole rows, sorted by row; they win over column styles.
    pub row_styles: Vec<(u32, u32)>,
    /// In drawing order.
    pub pictures: Vec<Picture>,
    /// Something on or about the sheet that inserting or deleting rows and
    /// columns cannot keep right yet (an Excel table, comments, a pivot
    /// table, a chart over its cells): its relationship kind. `None` when
    /// the sheet can be restructured.
    pub structure_blocker: Option<String>,
    /// Something deleting the sheet would leave broken (an Excel table
    /// others may name, a pivot table): its relationship kind.
    pub delete_blocker: Option<String>,
    /// The size hidden lines had, to come back with when shown: columns
    /// (`true`) or rows, and the line.
    pub hidden_sizes: std::collections::BTreeMap<(bool, u32), f64>,
}

impl Sheet {
    pub fn empty(name: impl Into<String>, kind: SheetKind) -> Self {
        SheetBuilder::new(name, kind).finish()
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn row(&self, index: u32) -> Option<&Row> {
        self.rows
            .binary_search_by_key(&index, |row| row.index)
            .ok()
            .map(|at| &self.rows[at])
    }

    pub fn cell(&self, at: CellRef) -> Option<&Cell> {
        let row = self.row(at.row)?;
        row.cells
            .binary_search_by_key(&at.col, |cell| cell.col)
            .ok()
            .map(|i| &row.cells[i])
    }

    /// The cells with something in them, inside `range`, row by row.
    pub fn cells_in(&self, range: CellRange) -> impl Iterator<Item = (CellRef, &Cell)> {
        let start = self.rows.partition_point(|row| row.index < range.first.row);
        self.rows[start..]
            .iter()
            .take_while(move |row| row.index <= range.last.row)
            .flat_map(move |row| {
                let from = row.cells.partition_point(|cell| cell.col < range.first.col);
                row.cells[from..]
                    .iter()
                    .take_while(move |cell| cell.col <= range.last.col)
                    .map(move |cell| (CellRef::new(row.index, cell.col), cell))
            })
    }

    pub(crate) fn rows_mut(&mut self) -> &mut Vec<Row> {
        &mut self.rows
    }

    /// Work out the used range and the cell count again after cells were
    /// put in or taken out.
    pub fn refresh(&mut self) {
        let mut used: Option<CellRange> = None;
        let mut count = 0;
        for row in &self.rows {
            if let (Some(first), Some(last)) = (row.cells.first(), row.cells.last()) {
                let span = CellRange::new(
                    CellRef::new(row.index, first.col),
                    CellRef::new(row.index, last.col),
                );
                used = Some(used.map_or(span, |u| u.union(&span)));
            }
            count += row.cells.iter().filter(|cell| !cell.is_blank()).count();
        }
        for merge in &self.merges {
            used = Some(used.map_or(*merge, |u| u.union(merge)));
        }
        self.used = used;
        self.cell_count = count;
    }

    pub(crate) fn cell_mut(&mut self, at: CellRef) -> Option<&mut Cell> {
        let row = self
            .rows
            .binary_search_by_key(&at.row, |row| row.index)
            .ok()?;
        let row = &mut self.rows[row];
        let i = row
            .cells
            .binary_search_by_key(&at.col, |cell| cell.col)
            .ok()?;
        Some(&mut row.cells[i])
    }

    /// The merged area `at` belongs to.
    pub fn merge_at(&self, at: CellRef) -> Option<&CellRange> {
        self.merges.iter().find(|merge| merge.contains(at))
    }

    /// The smallest range holding every cell with something in it and every
    /// merged area. `None` for an empty sheet.
    pub fn used_range(&self) -> Option<CellRange> {
        self.used
    }

    pub fn cell_count(&self) -> usize {
        self.cell_count
    }

    /// The next cell with something in it to the left of `at` in its row.
    pub fn previous_in_row(&self, at: CellRef) -> Option<&Cell> {
        let row = self.row(at.row)?;
        let i = row.cells.partition_point(|cell| cell.col < at.col);
        row.cells[..i].iter().rev().find(|cell| !cell.is_blank())
    }

    /// The next cell with something in it to the right of `at` in its row.
    pub fn next_in_row(&self, at: CellRef) -> Option<&Cell> {
        let row = self.row(at.row)?;
        let i = row.cells.partition_point(|cell| cell.col <= at.col);
        row.cells[i..].iter().find(|cell| !cell.is_blank())
    }

    /// Whether the cell has a value or a formula.
    pub fn is_filled(&self, at: CellRef) -> bool {
        self.cell(at).is_some_and(|cell| !cell.is_blank())
    }

    /// The style a cell is drawn with: its own if it is in the file, else
    /// its row's, else its column's, else the default (0).
    /// The format of a whole row, if it has one.
    pub fn row_style(&self, row: u32) -> Option<u32> {
        self.row_styles
            .binary_search_by_key(&row, |(r, _)| *r)
            .ok()
            .map(|i| self.row_styles[i].1)
    }

    /// The format of a whole column, if it has one.
    pub fn column_style(&self, col: u32) -> Option<u32> {
        let i = self
            .column_styles
            .partition_point(|(_, last, _)| *last < col);
        match self.column_styles.get(i) {
            Some(&(first, _, style)) if first <= col => Some(style),
            _ => None,
        }
    }

    /// Give a whole row a format, or take it away.
    pub fn set_row_style(&mut self, row: u32, style: Option<u32>) {
        match (
            self.row_styles.binary_search_by_key(&row, |(r, _)| *r),
            style,
        ) {
            (Ok(i), Some(style)) => self.row_styles[i].1 = style,
            (Ok(i), None) => {
                self.row_styles.remove(i);
            }
            (Err(i), Some(style)) => self.row_styles.insert(i, (row, style)),
            (Err(_), None) => {}
        }
    }

    /// Give a whole column a format, or take it away: the run that covers
    /// it is split around it, and equal neighbours join again.
    pub fn set_column_style(&mut self, col: u32, style: Option<u32>) {
        let mut runs: Vec<(u32, u32, u32)> = Vec::with_capacity(self.column_styles.len() + 2);
        for &(first, last, s) in &self.column_styles {
            if first <= col && col <= last {
                if first < col {
                    runs.push((first, col - 1, s));
                }
                if col < last {
                    runs.push((col + 1, last, s));
                }
            } else {
                runs.push((first, last, s));
            }
        }
        if let Some(style) = style {
            runs.push((col, col, style));
        }
        runs.sort_by_key(|(first, _, _)| *first);
        let mut joined: Vec<(u32, u32, u32)> = Vec::with_capacity(runs.len());
        for run in runs {
            match joined.last_mut() {
                Some(last) if last.1 + 1 == run.0 && last.2 == run.2 => last.1 = run.1,
                _ => joined.push(run),
            }
        }
        self.column_styles = joined;
    }

    pub fn style_at(&self, at: CellRef) -> u32 {
        if let Some(cell) = self.cell(at) {
            return cell.style;
        }
        if let Ok(i) = self
            .row_styles
            .binary_search_by_key(&at.row, |(row, _)| *row)
        {
            return self.row_styles[i].1;
        }
        let i = self
            .column_styles
            .partition_point(|(_, last, _)| *last < at.col);
        match self.column_styles.get(i) {
            Some(&(first, _, style)) if first <= at.col => style,
            _ => 0,
        }
    }
}

/// Collects cells in any order and makes a [`Sheet`].
#[derive(Debug)]
pub struct SheetBuilder {
    name: String,
    kind: SheetKind,
    hidden: bool,
    cells: Vec<(u32, Cell)>,
    pub columns: Axis,
    pub row_sizes: Axis,
    merges: Vec<CellRange>,
    pub frozen: (u32, u32),
    pub show_grid: bool,
    pub column_styles: Vec<(u32, u32, u32)>,
    pub row_styles: Vec<(u32, u32)>,
    pub pictures: Vec<Picture>,
    pub hidden_sizes: std::collections::BTreeMap<(bool, u32), f64>,
}

impl SheetBuilder {
    pub fn new(name: impl Into<String>, kind: SheetKind) -> Self {
        Self {
            name: name.into(),
            kind,
            hidden: false,
            cells: Vec::new(),
            columns: Axis::new(DEFAULT_COLUMN_WIDTH, MAX_COLUMNS),
            row_sizes: Axis::new(DEFAULT_ROW_HEIGHT, MAX_ROWS),
            merges: Vec::new(),
            frozen: (0, 0),
            show_grid: true,
            column_styles: Vec::new(),
            row_styles: Vec::new(),
            pictures: Vec::new(),
            hidden_sizes: std::collections::BTreeMap::new(),
        }
    }

    pub fn hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    /// Cells outside the sheet are dropped. A later cell at the same address
    /// replaces an earlier one.
    pub fn push(&mut self, row: u32, cell: Cell) {
        if row < MAX_ROWS && cell.col < MAX_COLUMNS {
            self.cells.push((row, cell));
        }
    }

    pub fn merge(&mut self, range: CellRange) {
        if range.last.row < MAX_ROWS && range.last.col < MAX_COLUMNS && range.first != range.last {
            self.merges.push(range);
        }
    }

    pub fn finish(mut self) -> Sheet {
        // Stable, so the last of two cells at one address stays last.
        self.cells.sort_by_key(|(row, cell)| (*row, cell.col));
        let mut rows: Vec<Row> = Vec::new();
        let mut used: Option<CellRange> = None;
        let mut cell_count = 0;
        for (index, cell) in self.cells {
            let at = CellRef::new(index, cell.col);
            match rows.last_mut() {
                Some(row) if row.index == index => {
                    if row.cells.last().is_some_and(|last| last.col == cell.col) {
                        *row.cells.last_mut().expect("checked") = cell;
                        continue;
                    }
                    row.cells.push(cell);
                }
                _ => rows.push(Row {
                    index,
                    cells: vec![cell],
                }),
            }
            let single = CellRange::single(at);
            used = Some(used.map_or(single, |u| u.union(&single)));
        }
        for merge in &self.merges {
            used = Some(used.map_or(*merge, |u| u.union(merge)));
        }
        for row in &rows {
            cell_count += row.cells.iter().filter(|cell| !cell.is_blank()).count();
        }
        self.column_styles.sort_by_key(|(first, _, _)| *first);
        self.row_styles.sort_by_key(|(row, _)| *row);
        self.row_styles.dedup_by_key(|(row, _)| *row);
        Sheet {
            name: self.name,
            kind: self.kind,
            hidden: self.hidden,
            rows,
            columns: self.columns,
            row_sizes: self.row_sizes,
            merges: self.merges,
            used,
            cell_count,
            frozen: (
                self.frozen.0.min(MAX_ROWS - 1),
                self.frozen.1.min(MAX_COLUMNS - 1),
            ),
            show_grid: self.show_grid,
            column_styles: self.column_styles,
            row_styles: self.row_styles,
            pictures: self.pictures,
            structure_blocker: None,
            delete_blocker: None,
            hidden_sizes: self.hidden_sizes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    pub styles: Vec<CellStyle>,
    /// Dates count from 1904 instead of 1900 (old Mac workbooks).
    pub date1904: bool,
    /// The sheet that was in front when the file was saved.
    pub active_sheet: usize,
    /// Defined names formulas can use (`TaxRate`, `Prices`), in file order.
    pub names: Vec<DefinedName>,
}

/// A defined name: a range, a constant or a formula, for the whole
/// workbook or one sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct DefinedName {
    pub name: String,
    /// The sheet it belongs to (`localSheetId`); `None` for the workbook.
    pub local: Option<usize>,
    /// What it stands for, without `=`: `汇总!$B$1`, `0.08`.
    pub formula: String,
}

/// How a cell's content is to be shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayKind {
    Number,
    Text,
    Bool,
    Error,
    /// A formula whose result was not saved in the file: its text is shown,
    /// since there is no engine to work it out yet.
    Uncomputed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub text: String,
    pub kind: DisplayKind,
    pub align: HAlign,
}

impl Workbook {
    pub fn style(&self, index: u32) -> CellStyle {
        self.styles.get(index as usize).cloned().unwrap_or_default()
    }

    /// The cell's content as the grid shows it. `None` for nothing to show.
    pub fn display(&self, cell: &Cell) -> Option<Display> {
        let style = self.styles.get(cell.style as usize);
        let format = style.map(|s| &*s.format);
        let (text, kind) = match &cell.value {
            Value::Empty => (
                format!("={}", cell.formula.as_ref()?),
                DisplayKind::Uncomputed,
            ),
            Value::Number(n) => (
                match format {
                    Some(format) => format.number(*n, self.date1904),
                    None => crate::format::general(*n),
                },
                DisplayKind::Number,
            ),
            Value::Text(text) => (
                match format {
                    Some(format) => format.text(text),
                    None => text.to_string(),
                },
                DisplayKind::Text,
            ),
            Value::Bool(b) => (
                (if *b { "TRUE" } else { "FALSE" }).to_owned(),
                DisplayKind::Bool,
            ),
            Value::Error(e) => (e.to_string(), DisplayKind::Error),
        };
        let align = style.and_then(|s| s.align).unwrap_or(match kind {
            DisplayKind::Number => HAlign::Right,
            DisplayKind::Text | DisplayKind::Uncomputed => HAlign::Left,
            DisplayKind::Bool | DisplayKind::Error => HAlign::Center,
        });
        Some(Display { text, kind, align })
    }

    /// What the formula bar shows for a cell: the formula with its `=`, or
    /// the value as entered (numbers unformatted, dates as dates).
    pub fn input_text(&self, cell: &Cell) -> String {
        if let Some(formula) = &cell.formula {
            return format!("={formula}");
        }
        match &cell.value {
            Value::Empty => String::new(),
            Value::Number(n) => {
                let style = self.style(cell.style);
                if style.format.is_date() {
                    let date = Format::parse(if n.fract() == 0.0 {
                        "yyyy/m/d"
                    } else if *n < 1.0 {
                        "h:mm:ss"
                    } else {
                        "yyyy/m/d h:mm:ss"
                    });
                    date.number(*n, self.date1904)
                } else {
                    crate::format::general(*n)
                }
            }
            Value::Text(text) => text.to_string(),
            Value::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_owned(),
            Value::Error(e) => e.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Value {
        Value::Text(Arc::from(s))
    }

    #[test]
    fn cells_are_sorted_and_found() {
        let mut builder = SheetBuilder::new("売上", SheetKind::Worksheet);
        builder.push(5, Cell::new(2, Value::Number(1.0)));
        builder.push(0, Cell::new(0, text("名前")));
        builder.push(5, Cell::new(0, Value::Number(2.0)));
        builder.push(5, Cell::new(2, Value::Number(3.0)));
        builder.push(MAX_ROWS, Cell::new(0, Value::Number(9.0)));
        builder.merge(CellRange::parse("A10:C11").unwrap());
        let sheet = builder.finish();
        assert_eq!(sheet.cell_count(), 3);
        assert_eq!(
            sheet.cell(CellRef::new(5, 2)).unwrap().value,
            Value::Number(3.0)
        );
        assert_eq!(sheet.cell(CellRef::new(5, 1)), None);
        assert_eq!(sheet.used_range().unwrap().to_string(), "A1:C11");
        let found: Vec<String> = sheet
            .cells_in(CellRange::parse("A1:B6").unwrap())
            .map(|(at, _)| at.to_string())
            .collect();
        assert_eq!(found, ["A1", "A6"]);
        assert_eq!(
            sheet
                .merge_at(CellRef::parse("B11").unwrap())
                .unwrap()
                .to_string(),
            "A10:C11"
        );
        assert_eq!(sheet.previous_in_row(CellRef::new(5, 2)).unwrap().col, 0);
        assert_eq!(sheet.next_in_row(CellRef::new(5, 0)).unwrap().col, 2);
    }

    #[test]
    fn blank_styled_cells_draw_but_do_not_count() {
        let mut builder = SheetBuilder::new("表", SheetKind::Worksheet);
        builder.push(0, Cell::new(0, text("左")));
        let mut blank = Cell::new(1, Value::Empty);
        blank.style = 3;
        builder.push(0, blank);
        builder.push(0, Cell::new(4, text("右")));
        builder.column_styles = vec![(10, 12, 7), (2, 3, 5)];
        builder.row_styles = vec![(9, 6)];
        let sheet = builder.finish();
        assert_eq!(sheet.cell_count(), 2);
        assert_eq!(sheet.used_range().unwrap().to_string(), "A1:E1");
        // Overflow and Ctrl+arrow look past the blank cell.
        assert_eq!(sheet.next_in_row(CellRef::new(0, 0)).unwrap().col, 4);
        assert_eq!(sheet.previous_in_row(CellRef::new(0, 4)).unwrap().col, 0);
        assert!(!sheet.is_filled(CellRef::new(0, 1)));
        assert!(sheet.is_filled(CellRef::new(0, 0)));
        // Own style, then the row's, then the column's.
        assert_eq!(sheet.style_at(CellRef::new(0, 1)), 3);
        assert_eq!(sheet.style_at(CellRef::new(9, 2)), 6);
        assert_eq!(sheet.style_at(CellRef::new(5, 2)), 5);
        assert_eq!(sheet.style_at(CellRef::new(5, 3)), 5);
        assert_eq!(sheet.style_at(CellRef::new(5, 4)), 0);
        assert_eq!(sheet.style_at(CellRef::new(5, 12)), 7);
        assert_eq!(sheet.style_at(CellRef::new(5, 13)), 0);
    }

    #[test]
    fn an_empty_sheet() {
        let sheet = Sheet::empty("Sheet1", SheetKind::Worksheet);
        assert_eq!(sheet.used_range(), None);
        assert_eq!(sheet.cell_count(), 0);
        assert_eq!(
            sheet
                .cells_in(CellRange::parse("A1:XFD1048576").unwrap())
                .count(),
            0
        );
    }

    #[test]
    fn display_follows_type_format_and_alignment() {
        let workbook = Workbook {
            sheets: Vec::new(),
            styles: vec![
                CellStyle::default(),
                CellStyle {
                    format: Arc::new(Format::parse("#,##0.00")),
                    ..CellStyle::default()
                },
                CellStyle {
                    format: Arc::new(Format::builtin(14)),
                    align: Some(HAlign::Center),
                    ..CellStyle::default()
                },
            ],
            date1904: false,
            active_sheet: 0,
            names: Vec::new(),
        };
        let mut cell = Cell::new(0, Value::Number(1234.5));
        cell.style = 1;
        let shown = workbook.display(&cell).unwrap();
        assert_eq!(
            (shown.text.as_str(), shown.align),
            ("1,234.50", HAlign::Right)
        );
        assert_eq!(workbook.input_text(&cell), "1234.5");

        let mut date = Cell::new(0, Value::Number(46_348.0));
        date.style = 2;
        assert_eq!(workbook.display(&date).unwrap().align, HAlign::Center);
        assert_eq!(
            workbook.input_text(&date),
            workbook.display(&date).unwrap().text
        );

        let mut uncomputed = Cell::new(0, Value::Empty);
        uncomputed.formula = Some(Arc::from("SUM(A1:A3)"));
        let shown = workbook.display(&uncomputed).unwrap();
        assert_eq!(
            (shown.text.as_str(), shown.kind),
            ("=SUM(A1:A3)", DisplayKind::Uncomputed)
        );

        let shown = workbook.display(&Cell::new(0, text("東京都"))).unwrap();
        assert_eq!(shown.align, HAlign::Left);
        let shown = workbook
            .display(&Cell::new(0, Value::Error(Arc::from("#N/A"))))
            .unwrap();
        assert_eq!((shown.text.as_str(), shown.align), ("#N/A", HAlign::Center));
        assert_eq!(workbook.display(&Cell::new(0, Value::Empty)), None);
        // A style index the file did not define falls back to General.
        let mut odd = Cell::new(0, Value::Number(0.5));
        odd.style = 99;
        assert_eq!(workbook.display(&odd).unwrap().text, "0.5");
    }
}
