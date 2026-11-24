//! Changing cells: typed entries made into cells, cells put in place with
//! what they replaced kept for undo, and calculated results written back.

use std::sync::Arc;

use crate::address::CellRef;
use crate::calc::Computed;
use crate::input::{Entry, Look};
use crate::model::{Cell, Sheet, Value, Workbook};

/// One cell before and after a change. `None` is no cell at all.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub sheet: usize,
    pub at: CellRef,
    pub before: Option<Cell>,
    pub after: Option<Cell>,
}

impl Change {
    /// The change that undoes this one.
    pub fn inverse(&self) -> Change {
        Change {
            sheet: self.sheet,
            at: self.at,
            before: self.after.clone(),
            after: self.before.clone(),
        }
    }

    /// Whether the cell's formula appears, goes or changes.
    pub fn touches_formula(&self) -> bool {
        let formula = |cell: &Option<Cell>| cell.as_ref().and_then(|c| c.formula.clone());
        formula(&self.before) != formula(&self.after)
    }
}

impl Sheet {
    /// Put a cell at `at` (or take it away) and return what was there.
    /// Call [`Sheet::refresh`] after a batch of these.
    pub fn put(&mut self, at: CellRef, cell: Option<Cell>) -> Option<Cell> {
        let cell = cell.map(|mut cell| {
            cell.col = at.col;
            cell
        });
        let rows = self.rows_mut();
        match rows.binary_search_by_key(&at.row, |row| row.index) {
            Ok(r) => {
                let row = &mut rows[r];
                let old = match row.cells.binary_search_by_key(&at.col, |c| c.col) {
                    Ok(i) => match cell {
                        Some(cell) => Some(std::mem::replace(&mut row.cells[i], cell)),
                        None => Some(row.cells.remove(i)),
                    },
                    Err(i) => {
                        if let Some(cell) = cell {
                            row.cells.insert(i, cell);
                        }
                        None
                    }
                };
                if row.cells.is_empty() {
                    rows.remove(r);
                }
                old
            }
            Err(r) => {
                if let Some(cell) = cell {
                    rows.insert(
                        r,
                        crate::model::Row {
                            index: at.row,
                            cells: vec![cell],
                        },
                    );
                }
                None
            }
        }
    }
}

impl Workbook {
    /// The cell an entry makes at `at`: the cell keeps its format, or takes
    /// one that suits the entry (a date format for a date) when its own is
    /// General and the workbook already has such a format with the same
    /// look otherwise. `None` for a cleared cell that shows nothing.
    pub fn entry_cell(&self, sheet: usize, at: CellRef, entry: Entry, look: Look) -> Option<Cell> {
        let current = self.sheets.get(sheet).map_or(0, |s| s.style_at(at));
        let style = self.style_for(current, look);
        let (value, formula) = match entry {
            Entry::Empty => (Value::Empty, None),
            Entry::Value(value) => (value, None),
            Entry::Formula(text) => (Value::Empty, Some(Arc::from(text))),
        };
        let cell = Cell {
            col: at.col,
            value,
            style,
            formula,
        };
        if cell.is_blank() && style == 0 {
            return None;
        }
        Some(cell)
    }

    /// A style index for an entry that looks like `look` in a cell of
    /// style `current`.
    fn style_for(&self, current: u32, look: Look) -> u32 {
        let base = self.style(current);
        if look == Look::Plain || base.format.code() != "General" && !base.format.code().is_empty()
        {
            return current;
        }
        let fits = |code: &str, is_date: bool, ordered: bool| match look {
            Look::Percent => code.contains('%'),
            Look::Grouped => code.contains("#,##0") && !code.contains('%'),
            Look::Date => is_date && ordered && !code.contains('h'),
            Look::Time => is_date && !ordered && code.contains('h'),
            Look::DateTime => is_date && ordered && code.contains('h'),
            Look::Plain => false,
        };
        self.styles
            .iter()
            .position(|style| {
                let format = &style.format;
                fits(
                    format.code(),
                    format.is_date(),
                    format.date_order().is_some(),
                ) && style.font == base.font
                    && style.fill == base.fill
                    && style.borders == base.borders
                    && style.align == base.align
            })
            .map_or(current, |i| i as u32)
    }

    /// Put cells in place on one sheet and return the changes made, with
    /// what each replaced. Cells that end up as they were are left out.
    pub fn set_cells(&mut self, sheet: usize, cells: Vec<(CellRef, Option<Cell>)>) -> Vec<Change> {
        let Some(target) = self.sheets.get_mut(sheet) else {
            return Vec::new();
        };
        let mut changes = Vec::with_capacity(cells.len());
        for (at, cell) in cells {
            let before = target.put(at, cell.clone());
            if before != cell {
                changes.push(Change {
                    sheet,
                    at,
                    before,
                    after: cell,
                });
            }
        }
        target.refresh();
        changes
    }

    /// Apply changes as they are recorded (to redo them, or their inverses
    /// to undo).
    pub fn apply(&mut self, changes: &[Change]) {
        let mut touched = Vec::new();
        for change in changes {
            if let Some(sheet) = self.sheets.get_mut(change.sheet) {
                sheet.put(change.at, change.after.clone());
                touched.push(change.sheet);
            }
        }
        touched.sort_unstable();
        touched.dedup();
        for sheet in touched {
            self.sheets[sheet].refresh();
        }
    }

    /// Write calculated results into the formula cells, in place. Returns
    /// the cells whose shown value changed.
    pub fn apply_results(&mut self, results: &[Computed]) -> Vec<(usize, CellRef)> {
        let mut changed = Vec::new();
        for result in results {
            let Some(sheet) = self.sheets.get_mut(result.sheet) else {
                continue;
            };
            if let Some(cell) = sheet.cell_mut(result.at)
                && cell.formula.is_some()
                && cell.value != result.value
            {
                cell.value = result.value.clone();
                changed.push((result.sheet, result.at));
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DateOrder;
    use crate::format::Format;
    use crate::input::{Reading, read};
    use crate::model::{SheetBuilder, SheetKind};
    use crate::style::CellStyle;

    fn workbook() -> Workbook {
        let mut sheet = SheetBuilder::new("表", SheetKind::Worksheet);
        sheet.push(0, Cell::new(0, Value::Number(1.0)));
        sheet.push(0, Cell::new(2, Value::Number(3.0)));
        let mut styled = Cell::new(0, Value::Number(5.0));
        styled.style = 1;
        sheet.push(4, styled);
        Workbook {
            sheets: vec![sheet.finish()],
            styles: vec![
                CellStyle::default(),
                CellStyle {
                    format: Arc::new(Format::parse("0.00")),
                    ..CellStyle::default()
                },
                CellStyle {
                    format: Arc::new(Format::builtin(14)),
                    ..CellStyle::default()
                },
                CellStyle {
                    format: Arc::new(Format::builtin(10)),
                    ..CellStyle::default()
                },
            ],
            date1904: false,
            active_sheet: 0,
        }
    }

    fn at(text: &str) -> CellRef {
        CellRef::parse(text).unwrap()
    }

    #[test]
    fn cells_go_in_and_come_out_in_order() {
        let mut book = workbook();
        let changes = book.set_cells(
            0,
            vec![
                (at("B1"), Some(Cell::new(9, Value::Text("中".into())))),
                (at("A1"), None),
                (at("C1"), Some(Cell::new(2, Value::Number(3.0)))),
                (at("D9"), Some(Cell::new(3, Value::Bool(true)))),
            ],
        );
        // C1 was already 3: no change recorded.
        assert_eq!(changes.len(), 3);
        let sheet = &book.sheets[0];
        assert_eq!(sheet.cell(at("B1")).unwrap().col, 1);
        assert!(sheet.cell(at("A1")).is_none());
        assert_eq!(sheet.cell_count(), 4);
        assert_eq!(sheet.used_range().unwrap().to_string(), "A1:D9");
        // Undo puts everything back.
        let undo: Vec<Change> = changes.iter().rev().map(Change::inverse).collect();
        book.apply(&undo);
        assert_eq!(book, workbook());
        book.apply(&changes);
        assert_eq!(book.sheets[0].cell_count(), 4);
        // Clearing the last cell of a row takes the row away.
        book.set_cells(0, vec![(at("D9"), None)]);
        assert!(book.sheets[0].row(8).is_none());
    }

    #[test]
    fn entries_keep_or_take_a_format() {
        let book = workbook();
        let reading = Reading {
            order: DateOrder::Ymd,
            date1904: false,
            this_year: 2026,
        };
        let entry = |text: &str, cell: &str| {
            let (entry, look) = read(text, &reading);
            book.entry_cell(0, at(cell), entry, look)
        };
        // A date in a General cell takes the date format the book has.
        let date = entry("2026/11/24", "B2").unwrap();
        assert_eq!(
            (date.style, date.value.clone()),
            (2, Value::Number(46350.0))
        );
        assert_eq!(book.display(&date).unwrap().text, "2026/11/24");
        assert_eq!(entry("50%", "B2").unwrap().style, 3);
        // No grouped format in this book: General stays.
        assert_eq!(entry("1,000", "B2").unwrap().style, 0);
        // A cell with its own format keeps it.
        assert_eq!(entry("2026/11/24", "A5").unwrap().style, 1);
        let formula = entry("=sum(a1:a3)", "B3").unwrap();
        assert_eq!(formula.formula.as_deref(), Some("SUM(A1:A3)"));
        assert!(entry("", "B3").is_none());
        // A cleared cell with a format keeps the format.
        let kept = entry("", "A5").unwrap();
        assert!(kept.is_blank() && kept.style == 1);
    }

    #[test]
    fn results_are_written_in_place() {
        let mut book = workbook();
        let mut formula = Cell::new(1, Value::Empty);
        formula.formula = Some(Arc::from("A1+C1"));
        book.set_cells(0, vec![(at("B1"), Some(formula))]);
        let results = [
            Computed {
                sheet: 0,
                at: at("B1"),
                value: Value::Number(4.0),
            },
            // Not a formula: ignored.
            Computed {
                sheet: 0,
                at: at("A1"),
                value: Value::Number(9.0),
            },
        ];
        assert_eq!(book.apply_results(&results), [(0, at("B1"))]);
        assert_eq!(book.apply_results(&results), []);
        assert_eq!(
            book.sheets[0].cell(at("B1")).unwrap().value,
            Value::Number(4.0)
        );
        let change = Change {
            sheet: 0,
            at: at("A1"),
            before: None,
            after: book.sheets[0].cell(at("B1")).cloned(),
        };
        assert!(change.touches_formula());
    }
}
