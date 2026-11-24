//! Copying and pasting cells: tab-separated text for other programs (Excel,
//! Numbers and spreadsheets in browsers all read and write it), and the
//! cells themselves for pasting within the workbook, where formulas follow
//! the paste.

use crate::address::{CellRange, CellRef, MAX_COLUMNS, MAX_ROWS};
use crate::formula::shift;
use crate::model::{Cell, Workbook};

/// Cells copied from a sheet, by their place in the copied range.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub sheet: usize,
    pub range: CellRange,
    /// Relative to the range's top-left; only cells with something in them.
    pub cells: Vec<(CellRef, Cell)>,
    /// What went to the system clipboard, to know a paste comes from here.
    pub text: String,
    /// Cut rather than copied: pasting moves the cells.
    pub cut: bool,
}

impl Workbook {
    /// Copy a range: the cells, and their text as shown, tab-separated.
    /// Whole rows and columns are cut down to the cells in use.
    pub fn copy(&self, sheet: usize, range: CellRange, cut: bool) -> Option<Clip> {
        let source = self.sheets.get(sheet)?;
        let whole = range.rows() == MAX_ROWS || range.cols() == MAX_COLUMNS;
        let range = match source.used_range() {
            Some(used) if whole => CellRange::new(
                range.first,
                CellRef::new(
                    range.last.row.min(used.last.row.max(range.first.row)),
                    range.last.col.min(used.last.col.max(range.first.col)),
                ),
            ),
            None if whole => CellRange::single(range.first),
            _ => range,
        };
        let cells: Vec<(CellRef, Cell)> = source
            .cells_in(range)
            .filter(|(_, cell)| !cell.is_blank() || cell.style != 0)
            .map(|(at, cell)| {
                (
                    CellRef::new(at.row - range.first.row, at.col - range.first.col),
                    cell.clone(),
                )
            })
            .collect();
        let mut text = String::new();
        let mut next = cells.iter().peekable();
        for row in 0..range.rows() {
            if row > 0 {
                text.push_str("\r\n");
            }
            for col in 0..range.cols() {
                if col > 0 {
                    text.push('\t');
                }
                while next
                    .peek()
                    .is_some_and(|(at, _)| (at.row, at.col) < (row, col))
                {
                    next.next();
                }
                if let Some((at, cell)) = next.peek()
                    && (at.row, at.col) == (row, col)
                {
                    let shown = self.display(cell).map(|d| d.text).unwrap_or_default();
                    push_field(&mut text, &shown);
                }
            }
        }
        Some(Clip {
            sheet,
            range,
            cells,
            text,
            cut,
        })
    }
}

/// A field, quoted when it holds a tab, a line break or a quote.
fn push_field(out: &mut String, field: &str) {
    if field.contains(['\t', '\n', '\r', '"']) {
        out.push('"');
        out.push_str(&field.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(field);
    }
}

/// Read tab-separated text into rows of fields. Quoted fields may hold
/// tabs, line breaks and doubled quotes; a last empty line is dropped.
pub fn parse_tsv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars().peekable();
    let mut at_field_start = true;
    while let Some(c) = chars.next() {
        match c {
            '"' if at_field_start => {
                // Quoted until a quote not doubled.
                while let Some(c) = chars.next() {
                    if c == '"' {
                        if chars.peek() == Some(&'"') {
                            chars.next();
                            field.push('"');
                        } else {
                            break;
                        }
                    } else {
                        field.push(c);
                    }
                }
                at_field_start = false;
            }
            '\t' => {
                row.push(std::mem::take(&mut field));
                at_field_start = true;
            }
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                at_field_start = true;
            }
            c => {
                field.push(c);
                at_field_start = false;
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

impl Clip {
    /// The cells to put for pasting at `target` (top-left) on `sheet`:
    /// every place of the copied range, emptied where the copy had nothing.
    /// Copied formulas move their relative references by the distance of
    /// the paste; cut ones do not, as they move.
    pub fn cells_at(&self, target: CellRef) -> Vec<(CellRef, Option<Cell>)> {
        let rows = i64::from(target.row) - i64::from(self.range.first.row);
        let cols = i64::from(target.col) - i64::from(self.range.first.col);
        let mut out = Vec::with_capacity((self.range.rows() * self.range.cols()) as usize);
        let mut next = self.cells.iter().peekable();
        for row in 0..self.range.rows() {
            for col in 0..self.range.cols() {
                let at = CellRef::new(target.row + row, target.col + col);
                if at.row >= MAX_ROWS || at.col >= MAX_COLUMNS {
                    continue;
                }
                let cell = match next.peek() {
                    Some((rel, cell)) if (rel.row, rel.col) == (row, col) => {
                        next.next();
                        let mut cell = cell.clone();
                        if let (Some(formula), false) = (&cell.formula, self.cut) {
                            cell.formula = Some(shift(formula, rows, cols).into());
                        }
                        Some(cell)
                    }
                    _ => None,
                };
                out.push((at, cell));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SheetBuilder, SheetKind, Value};
    use std::sync::Arc;

    fn at(text: &str) -> CellRef {
        CellRef::parse(text).unwrap()
    }

    fn workbook() -> Workbook {
        let mut sheet = SheetBuilder::new("表", SheetKind::Worksheet);
        sheet.push(0, Cell::new(0, Value::Text("名前\tタブ".into())));
        sheet.push(0, Cell::new(1, Value::Number(1234.5)));
        sheet.push(1, Cell::new(0, Value::Text("say \"hi\"\n2行".into())));
        let mut formula = Cell::new(1, Value::Number(2469.0));
        formula.formula = Some(Arc::from("B1*2"));
        sheet.push(1, formula);
        Workbook {
            sheets: vec![sheet.finish()],
            ..Workbook::default()
        }
    }

    #[test]
    fn copies_as_tab_separated_text_with_quotes() {
        let book = workbook();
        let clip = book
            .copy(0, CellRange::parse("A1:C2").unwrap(), false)
            .unwrap();
        assert_eq!(
            clip.text,
            "\"名前\tタブ\"\t1234.5\t\r\n\"say \"\"hi\"\"\n2行\"\t2469\t"
        );
        assert_eq!(
            parse_tsv(&clip.text),
            vec![
                vec!["名前\tタブ".to_owned(), "1234.5".into(), String::new()],
                vec!["say \"hi\"\n2行".to_owned(), "2469".into(), String::new()],
            ]
        );
        // A whole column is cut down to the rows in use.
        let column = book
            .copy(
                0,
                CellRange::new(at("B1"), CellRef::new(MAX_ROWS - 1, 1)),
                false,
            )
            .unwrap();
        assert_eq!(column.range.to_string(), "B1:B2");
        assert_eq!(column.text, "1234.5\r\n2469");
    }

    #[test]
    fn reads_what_excel_and_numbers_write() {
        assert_eq!(
            parse_tsv("a\tb\r\nc\td\r\n"),
            vec![vec!["a", "b"], vec!["c", "d"]]
        );
        assert_eq!(
            parse_tsv("a\tb\nc\td"),
            vec![vec!["a", "b"], vec!["c", "d"]]
        );
        assert_eq!(parse_tsv("x"), vec![vec!["x"]]);
        assert_eq!(parse_tsv("\t\n"), vec![vec!["", ""]]);
        assert_eq!(parse_tsv(""), Vec::<Vec<String>>::new());
        // A quote inside an unquoted field is just a quote.
        assert_eq!(parse_tsv("5\" pipe"), vec![vec!["5\" pipe"]]);
    }

    #[test]
    fn pasted_formulas_follow_and_cut_ones_stay() {
        let book = workbook();
        let clip = book
            .copy(0, CellRange::parse("A2:B2").unwrap(), false)
            .unwrap();
        let cells = clip.cells_at(at("C5"));
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[1].0, at("D5"));
        assert_eq!(
            cells[1].1.as_ref().unwrap().formula.as_deref(),
            Some("D4*2")
        );
        // Moved off the top of the sheet: #REF!.
        let up = clip.cells_at(at("A1"));
        assert_eq!(
            up[1].1.as_ref().unwrap().formula.as_deref(),
            Some("#REF!*2")
        );
        let cut = book.copy(0, CellRange::parse("B2").unwrap(), true).unwrap();
        assert_eq!(
            cut.cells_at(at("E9"))[0]
                .1
                .as_ref()
                .unwrap()
                .formula
                .as_deref(),
            Some("B1*2")
        );
        // Empty places of the copy empty the target.
        let gap = book
            .copy(0, CellRange::parse("B1:C1").unwrap(), false)
            .unwrap();
        assert!(gap.cells_at(at("A9"))[1].1.is_none());
    }
}
