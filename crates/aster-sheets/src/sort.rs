//! Sorting rows, as Excel's "Sort A to Z" and "Sort Z to A" do.
//!
//! The rows of a range are put in the order of one column's values:
//! numbers (dates are numbers), then text (letter case does not count),
//! then TRUE and FALSE, then errors; empty cells go last whichever way the
//! sort goes. Rows that compare equal keep their order. A formula moves
//! with its row, its relative references with it, as a paste would move
//! them. One cell selected sorts the block of data around it, without its
//! header row if it seems to have one. Rows an autofilter hides are left
//! where they are.

use std::cmp::Ordering;
use std::sync::Arc;

use crate::address::{CellRange, CellRef, MAX_COLUMNS, MAX_ROWS};
use crate::model::{Cell, Sheet, Value, Workbook};

/// Why rows cannot be sorted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SortRefusal {
    /// Merged cells in the range: their rows cannot be told apart.
    Merged,
    /// A threaded comment in the range: it cannot move with its row.
    Threaded,
    /// Nothing to sort.
    Empty,
}

/// What a sort did.
#[derive(Clone, Debug, PartialEq)]
pub struct Sorted {
    /// The rows sorted, without a header.
    pub range: CellRange,
    /// The cells to put.
    pub cells: Vec<(CellRef, Option<Cell>)>,
    /// The notes of the range as they are after the sort: they move with
    /// their rows. Empty when the range has none.
    pub comments: Vec<(CellRef, Option<crate::comment::Comment>)>,
}

impl Sheet {
    /// The block of data around a cell: grown row by row and column by
    /// column while a row or column next to it has something in it, as
    /// Excel's "current region".
    pub fn current_region(&self, at: CellRef) -> CellRange {
        let mut range = CellRange::single(at);
        let filled = |range: CellRange| self.cells_in(range).any(|(_, c)| !c.is_blank());
        loop {
            let mut grown = range;
            if range.first.row > 0 {
                let above = CellRange::new(
                    CellRef::new(range.first.row - 1, range.first.col),
                    CellRef::new(range.first.row - 1, range.last.col),
                );
                if filled(above) {
                    grown.first.row -= 1;
                }
            }
            if range.last.row + 1 < MAX_ROWS {
                let below = CellRange::new(
                    CellRef::new(range.last.row + 1, range.first.col),
                    CellRef::new(range.last.row + 1, range.last.col),
                );
                if filled(below) {
                    grown.last.row += 1;
                }
            }
            if range.first.col > 0 {
                let left = CellRange::new(
                    CellRef::new(grown.first.row, range.first.col - 1),
                    CellRef::new(grown.last.row, range.first.col - 1),
                );
                if filled(left) {
                    grown.first.col -= 1;
                }
            }
            if range.last.col + 1 < MAX_COLUMNS {
                let right = CellRange::new(
                    CellRef::new(grown.first.row, range.last.col + 1),
                    CellRef::new(grown.last.row, range.last.col + 1),
                );
                if filled(right) {
                    grown.last.col += 1;
                }
            }
            if grown == range {
                return range;
            }
            range = grown;
        }
    }

    /// Whether the first row of a block looks like a header: all text,
    /// over a column whose next value is not.
    pub fn has_header(&self, range: CellRange, key: u32) -> bool {
        if range.rows() < 2 {
            return false;
        }
        let first = range.first.row;
        let header: Vec<&Cell> = (range.first.col..=range.last.col)
            .filter_map(|col| self.cell(CellRef::new(first, col)))
            .filter(|c| !c.is_blank())
            .collect();
        let all_text = !header.is_empty()
            && header
                .iter()
                .all(|c| matches!(c.value, Value::Text(_)) && c.formula.is_none());
        let below = self.cell(CellRef::new(first + 1, key));
        all_text && below.is_some_and(|c| !matches!(c.value, Value::Text(_)))
    }
}

impl Workbook {
    /// Sort the rows of `range` on `sheet` by column `key`. Returns the
    /// cells to put; the sheet is not changed here.
    pub fn sort_rows(
        &self,
        sheet: usize,
        range: CellRange,
        key: u32,
        descending: bool,
    ) -> Result<Sorted, SortRefusal> {
        let Some(sheet) = self.sheets.get(sheet) else {
            return Err(SortRefusal::Empty);
        };
        if sheet.merges.iter().any(|m| m.intersects(&range)) {
            return Err(SortRefusal::Merged);
        }
        // Whole columns sort as far as there is data.
        let last_row = sheet
            .used_range()
            .map_or(range.first.row, |used| used.last.row.min(range.last.row));
        if last_row < range.first.row {
            return Err(SortRefusal::Empty);
        }
        let range = CellRange::new(range.first, CellRef::new(last_row, range.last.col));
        let noted: Vec<CellRef> = sheet
            .comments
            .keys()
            .copied()
            .filter(|at| range.contains(*at))
            .collect();
        if noted.iter().any(|at| sheet.comments[at].threaded) {
            return Err(SortRefusal::Threaded);
        }
        // Rows a filter hides stay where they are, as in Excel.
        let filtered = sheet.auto_filter.as_ref().map(|f| &f.hidden);
        let rows: Vec<u32> = (range.first.row..=range.last.row)
            .filter(|row| !filtered.is_some_and(|hidden| hidden.contains(row)))
            .collect();
        let row_cells = |row: u32| -> Vec<Option<Cell>> {
            (range.first.col..=range.last.col)
                .map(|col| sheet.cell(CellRef::new(row, col)).cloned())
                .collect()
        };
        let mut order: Vec<(u32, Option<Value>)> = rows
            .iter()
            .map(|&row| {
                let value = sheet
                    .cell(CellRef::new(row, key))
                    .map(|c| c.value.clone())
                    .filter(|v| *v != Value::Empty);
                (row, value)
            })
            .collect();
        order.sort_by(|(_, a), (_, b)| compare(a.as_ref(), b.as_ref(), descending));
        let mut cells = Vec::new();
        let mut comments = Vec::new();
        for (to, (from, _)) in rows.iter().zip(&order) {
            for col in range.first.col..=range.last.col {
                let now = sheet.comments.get(&CellRef::new(*to, col));
                let moved = sheet.comments.get(&CellRef::new(*from, col));
                if now != moved {
                    comments.push((CellRef::new(*to, col), moved.cloned()));
                }
            }
            let moved = i64::from(*to) - i64::from(*from);
            for (i, cell) in row_cells(*from).into_iter().enumerate() {
                let col = range.first.col + i as u32;
                let cell = cell.map(|mut c| {
                    if let Some(formula) = &c.formula {
                        c.formula = Some(Arc::from(crate::formula::shift(formula, moved, 0)));
                    }
                    c
                });
                let current = sheet.cell(CellRef::new(*to, col));
                if current != cell.as_ref() {
                    cells.push((CellRef::new(*to, col), cell));
                }
            }
        }
        Ok(Sorted {
            range,
            cells,
            comments,
        })
    }
}

/// The order of two sort keys. Empty (`None`) goes last either way.
fn compare(a: Option<&Value>, b: Option<&Value>, descending: bool) -> Ordering {
    let (a, b) = match (a, b) {
        (None, None) => return Ordering::Equal,
        (None, Some(_)) => return Ordering::Greater,
        (Some(_), None) => return Ordering::Less,
        (Some(a), Some(b)) => (a, b),
    };
    let rank = |v: &Value| match v {
        Value::Number(_) => 0,
        Value::Text(_) => 1,
        Value::Bool(_) => 2,
        Value::Error(_) | Value::Empty => 3,
    };
    let order = rank(a).cmp(&rank(b)).then_with(|| match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Text(x), Value::Text(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => Ordering::Equal,
    });
    if descending { order.reverse() } else { order }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SheetBuilder, SheetKind};

    fn book(rows: &[&[&str]]) -> Workbook {
        let mut sheet = SheetBuilder::new("表", SheetKind::Worksheet);
        for (r, row) in rows.iter().enumerate() {
            for (c, text) in row.iter().enumerate() {
                if text.is_empty() {
                    continue;
                }
                let mut cell = match text.parse::<f64>() {
                    Ok(n) => Cell::new(c as u32, Value::Number(n)),
                    Err(_) => Cell::new(c as u32, Value::Text(Arc::from(*text))),
                };
                if let Some(formula) = text.strip_prefix('=') {
                    cell = Cell::new(c as u32, Value::Number(0.0));
                    cell.formula = Some(Arc::from(formula));
                }
                sheet.push(r as u32, cell);
            }
        }
        Workbook {
            sheets: vec![sheet.finish()],
            ..Workbook::default()
        }
    }

    fn column(book: &Workbook, sorted: &Sorted, col: u32) -> Vec<String> {
        let mut out = Vec::new();
        let mut copy = book.clone();
        copy.set_cells(0, sorted.cells.clone());
        let sheet = &copy.sheets[0];
        for row in sorted.range.first.row..=sorted.range.last.row {
            out.push(match sheet.cell(CellRef::new(row, col)) {
                Some(Cell {
                    formula: Some(f), ..
                }) => format!("={f}"),
                Some(Cell {
                    value: Value::Number(n),
                    ..
                }) => n.to_string(),
                Some(Cell {
                    value: Value::Text(t),
                    ..
                }) => t.to_string(),
                _ => String::new(),
            });
        }
        out
    }

    #[test]
    fn rows_sort_by_a_column_with_blanks_last() {
        let book = book(&[
            &["名前", "点", "倍"],
            &["b", "3", "=B2*2"],
            &["A", "", "=B3*2"],
            &["c", "1", "=B4*2"],
            &["a", "2", "=B5*2"],
        ]);
        let sheet = &book.sheets[0];
        let region = sheet.current_region(CellRef::parse("B3").unwrap());
        assert_eq!(region, CellRange::parse("A1:C5").unwrap());
        assert!(sheet.has_header(region, 1));
        let data = CellRange::parse("A2:C5").unwrap();
        let up = book.sort_rows(0, data, 1, false).unwrap();
        assert_eq!(column(&book, &up, 1), ["1", "2", "3", ""]);
        assert_eq!(column(&book, &up, 0), ["c", "a", "b", "A"]);
        // Formulas move with their row and keep pointing at it.
        assert_eq!(column(&book, &up, 2), ["=B2*2", "=B3*2", "=B4*2", "=B5*2"]);
        let down = book.sort_rows(0, data, 1, true).unwrap();
        assert_eq!(column(&book, &down, 1), ["3", "2", "1", ""]);
        // Text ignores letter case and keeps the order of equals.
        let by_name = book.sort_rows(0, data, 0, false).unwrap();
        assert_eq!(column(&book, &by_name, 0), ["A", "a", "b", "c"]);
    }

    #[test]
    fn mixed_values_and_merged_cells() {
        let mut book = book(&[&["x"], &["2"], &["TRUE"], &["1"]]);
        book.sheets[0]
            .merges
            .push(CellRange::parse("A1:B1").unwrap());
        assert_eq!(
            book.sort_rows(0, CellRange::parse("A1:A4").unwrap(), 0, false),
            Err(SortRefusal::Merged)
        );
        book.sheets[0].merges.clear();
        let sorted = book
            .sort_rows(0, CellRange::parse("A1:A4").unwrap(), 0, false)
            .unwrap();
        assert_eq!(column(&book, &sorted, 0), ["1", "2", "TRUE", "x"]);
    }
}
