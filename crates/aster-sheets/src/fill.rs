//! The fill handle: a selection dragged out over more cells, as Excel
//! fills them.
//!
//! Each column (or row, filling sideways) of the selection is a pattern
//! for the cells beyond it:
//!
//! - two or more numbers continue as a series, by their average step;
//! - one date goes on a day at a time;
//! - text ending in a number (`Q1`, `Item 3`) counts on, keeping the
//!   number's leading zeros;
//! - anything else repeats, formulas with their relative references moved
//!   as in a paste.
//!
//! Formats come along with the cells they repeat.

use std::sync::Arc;

use crate::address::{CellRange, CellRef};
use crate::model::{Cell, Value, Workbook};

/// Where a filled range goes from the selection it started from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    Down,
    Up,
    Right,
    Left,
}

impl Workbook {
    /// The cells that fill `target` (which holds `source` and goes on from
    /// it in one direction) from the pattern in `source`.
    pub fn fill(
        &self,
        sheet: usize,
        source: CellRange,
        target: CellRange,
    ) -> Vec<(CellRef, Option<Cell>)> {
        let Some(sheet) = self.sheets.get(sheet) else {
            return Vec::new();
        };
        let way = if target.last.row > source.last.row {
            Way::Down
        } else if target.first.row < source.first.row {
            Way::Up
        } else if target.last.col > source.last.col {
            Way::Right
        } else if target.first.col < source.first.col {
            Way::Left
        } else {
            return Vec::new();
        };
        let vertical = matches!(way, Way::Down | Way::Up);
        // The lines across the fill: columns going down, rows going right.
        let (across, along) = if vertical {
            (
                source.first.col..=source.last.col,
                (source.first.row, source.last.row),
            )
        } else {
            (
                source.first.row..=source.last.row,
                (source.first.col, source.last.col),
            )
        };
        let at = |line: u32, step: u32| {
            if vertical {
                CellRef::new(step, line)
            } else {
                CellRef::new(line, step)
            }
        };
        let targets: Vec<u32> = match way {
            Way::Down => (source.last.row + 1..=target.last.row).collect(),
            Way::Up => (target.first.row..source.first.row).rev().collect(),
            Way::Right => (source.last.col + 1..=target.last.col).collect(),
            Way::Left => (target.first.col..source.first.col).rev().collect(),
        };
        let forward = matches!(way, Way::Down | Way::Right);
        let mut out = Vec::new();
        for line in across {
            let pattern: Vec<(CellRef, Option<&Cell>)> = (along.0..=along.1)
                .map(|step| {
                    let at = at(line, step);
                    (at, sheet.cell(at))
                })
                .collect();
            let series = self.series(&pattern);
            for (k, &step) in targets.iter().enumerate() {
                let k = k + 1;
                let to = at(line, step);
                // The pattern cell this one repeats: going on from the end
                // going forward, back from the start going back.
                let n = pattern.len();
                let index = if forward {
                    (k - 1) % n
                } else {
                    n - 1 - (k - 1) % n
                };
                let (from, cell) = pattern[index];
                let filled = match (&series, cell) {
                    (Some(series), Some(cell)) => Some(series.cell(cell, k, forward)),
                    (None, Some(cell)) => Some(moved(cell, from, to)),
                    (_, None) => None,
                };
                let filled = filled.map(|mut c| {
                    c.col = to.col;
                    c
                });
                out.push((to, filled));
            }
        }
        out
    }

    /// The series a pattern makes, if it makes one.
    fn series(&self, pattern: &[(CellRef, Option<&Cell>)]) -> Option<Series> {
        let cells: Vec<&Cell> = pattern.iter().map(|(_, c)| *c).collect::<Option<_>>()?;
        if cells.iter().any(|c| c.formula.is_some()) {
            return None;
        }
        let numbers: Option<Vec<f64>> = cells
            .iter()
            .map(|c| match c.value {
                Value::Number(n) => Some(n),
                _ => None,
            })
            .collect();
        if let Some(numbers) = numbers {
            let first = numbers[0];
            let last = numbers[numbers.len() - 1];
            if numbers.len() >= 2 {
                let step = (last - first) / (numbers.len() - 1) as f64;
                return Some(Series::Number { first, last, step });
            }
            // One date goes on a day at a time; one number is copied.
            if self.style(cells[0].style).format.is_date() {
                return Some(Series::Number {
                    first,
                    last,
                    step: 1.0,
                });
            }
            return None;
        }
        // Text ending in a number, the same text before it each time.
        let parts: Option<Vec<(&str, u64, usize)>> = cells
            .iter()
            .map(|c| match &c.value {
                Value::Text(text) => numbered(text),
                _ => None,
            })
            .collect();
        let parts = parts?;
        let prefix = parts[0].0;
        if parts.iter().any(|(p, _, _)| *p != prefix) {
            return None;
        }
        let first = parts[0].1 as f64;
        let last = parts[parts.len() - 1].1 as f64;
        let step = if parts.len() >= 2 {
            (last - first) / (parts.len() - 1) as f64
        } else {
            1.0
        };
        Some(Series::Numbered {
            prefix: prefix.to_owned(),
            first,
            last,
            step,
            width: parts[0].2,
        })
    }
}

/// A pattern that counts on.
#[derive(Debug)]
enum Series {
    Number {
        first: f64,
        last: f64,
        step: f64,
    },
    Numbered {
        prefix: String,
        first: f64,
        last: f64,
        step: f64,
        /// Digits written, with leading zeros (`001`).
        width: usize,
    },
}

impl Series {
    /// The `k`th cell past the pattern (from 1), going forward or back,
    /// formatted as `like`.
    fn cell(&self, like: &Cell, k: usize, forward: bool) -> Cell {
        let mut cell = like.clone();
        let k = k as f64;
        match self {
            Series::Number { first, last, step } => {
                let n = if forward {
                    last + step * k
                } else {
                    first - step * k
                };
                cell.value = Value::Number(n);
            }
            Series::Numbered {
                prefix,
                first,
                last,
                step,
                width,
            } => {
                let n = if forward {
                    last + step * k
                } else {
                    first - step * k
                };
                // Below zero there is no number to count with: the text
                // stays.
                if n >= 0.0 {
                    let n = n.round() as u64;
                    cell.value = Value::Text(Arc::from(format!("{prefix}{n:0width$}")));
                }
            }
        }
        cell
    }
}

/// Text that ends in a number after something else: `Item 08` is
/// (`Item `, 8, 2 digits). All digits, or digits with text after them, is
/// not.
fn numbered(text: &str) -> Option<(&str, u64, usize)> {
    let end = text.len();
    let digits_end = text
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_ascii_digit())
        .map(|(i, _)| i + 1)?;
    // Only text whose number is at the end.
    if digits_end != end {
        return None;
    }
    let start = text[..digits_end]
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_ascii_digit())
        .map_or(0, |(i, c)| i + c.len_utf8());
    if start == 0 {
        // All digits: a number typed as text repeats.
        return None;
    }
    let digits = &text[start..digits_end];
    Some((&text[..start], digits.parse().ok()?, digits.len()))
}

/// A cell repeated from `from` to `to`: its formula's relative references
/// move with it.
fn moved(cell: &Cell, from: CellRef, to: CellRef) -> Cell {
    let mut out = cell.clone();
    if let Some(formula) = &cell.formula {
        let rows = i64::from(to.row) - i64::from(from.row);
        let cols = i64::from(to.col) - i64::from(from.col);
        out.formula = Some(Arc::from(crate::formula::shift(formula, rows, cols)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Format;
    use crate::model::{SheetBuilder, SheetKind};
    use crate::style::CellStyle;

    fn at(text: &str) -> CellRef {
        CellRef::parse(text).unwrap()
    }

    fn book(cells: &[(&str, Value, Option<&str>, u32)]) -> Workbook {
        let mut sheet = SheetBuilder::new("表", SheetKind::Worksheet);
        for (cell, value, formula, style) in cells {
            let at = at(cell);
            let mut c = Cell::new(at.col, value.clone());
            c.formula = formula.map(Arc::from);
            c.style = *style;
            sheet.push(at.row, c);
        }
        Workbook {
            sheets: vec![sheet.finish()],
            styles: vec![
                CellStyle::default(),
                CellStyle {
                    format: Arc::new(Format::builtin(14)),
                    ..CellStyle::default()
                },
            ],
            ..Workbook::default()
        }
    }

    fn values(filled: &[(CellRef, Option<Cell>)]) -> Vec<(String, Option<Value>)> {
        filled
            .iter()
            .map(|(at, c)| (at.to_string(), c.as_ref().map(|c| c.value.clone())))
            .collect()
    }

    fn text(s: &str) -> Value {
        Value::Text(Arc::from(s))
    }

    #[test]
    fn numbers_dates_and_numbered_text_count_on() {
        let book = book(&[
            ("A1", Value::Number(1.0), None, 0),
            ("A2", Value::Number(3.0), None, 0),
            ("B1", Value::Number(7.0), None, 0),
            ("C1", Value::Number(46000.0), None, 1),
            ("D1", text("Item 08"), None, 0),
            ("E1", text("東京"), None, 0),
        ]);
        // Two numbers: a series by their step.
        let down = book.fill(
            0,
            CellRange::parse("A1:A2").unwrap(),
            CellRange::parse("A1:A4").unwrap(),
        );
        assert_eq!(
            values(&down),
            [
                ("A3".into(), Some(Value::Number(5.0))),
                ("A4".into(), Some(Value::Number(7.0)))
            ]
        );
        // Going up, it counts back.
        let up = book.fill(
            0,
            CellRange::parse("A1:A2").unwrap(),
            CellRange::parse("A1:A2").unwrap(),
        );
        assert!(up.is_empty());
        // One number is copied; one date counts days; numbered text counts on.
        let right = book.fill(
            0,
            CellRange::parse("B1:E1").unwrap(),
            CellRange::parse("B1:E3").unwrap(),
        );
        let found: std::collections::HashMap<String, Option<Value>> =
            values(&right).into_iter().collect();
        assert_eq!(found["B3"], Some(Value::Number(7.0)));
        assert_eq!(found["C2"], Some(Value::Number(46001.0)));
        assert_eq!(found["C3"], Some(Value::Number(46002.0)));
        assert_eq!(found["D2"], Some(text("Item 09")));
        assert_eq!(found["D3"], Some(text("Item 10")));
        assert_eq!(found["E3"], Some(text("東京")));
        // The date keeps its format.
        let c2 = right.iter().find(|(a, _)| *a == at("C2")).unwrap();
        assert_eq!(c2.1.as_ref().unwrap().style, 1);
    }

    #[test]
    fn formulas_move_and_patterns_repeat_either_way() {
        let book = book(&[
            ("A5", Value::Number(1.0), None, 0),
            ("A6", text("x"), None, 0),
            ("B5", Value::Empty, Some("A5*2"), 0),
            ("C5", Value::Number(10.0), None, 0),
            ("C6", Value::Number(20.0), None, 0),
        ]);
        // A formula filled down moves its references.
        let down = book.fill(
            0,
            CellRange::parse("B5").unwrap(),
            CellRange::parse("B5:B7").unwrap(),
        );
        let formulas: Vec<Option<String>> = down
            .iter()
            .map(|(_, c)| {
                c.as_ref()
                    .and_then(|c| c.formula.as_deref().map(str::to_owned))
            })
            .collect();
        assert_eq!(formulas, [Some("A6*2".to_owned()), Some("A7*2".to_owned())]);
        // A mixed pattern repeats; going up, from its end.
        let up = book.fill(
            0,
            CellRange::parse("A5:A6").unwrap(),
            CellRange::parse("A2:A6").unwrap(),
        );
        assert_eq!(
            values(&up),
            [
                ("A4".into(), Some(text("x"))),
                ("A3".into(), Some(Value::Number(1.0))),
                ("A2".into(), Some(text("x"))),
            ]
        );
        // A series going up counts back.
        let back = book.fill(
            0,
            CellRange::parse("C5:C6").unwrap(),
            CellRange::parse("C3:C6").unwrap(),
        );
        assert_eq!(
            values(&back),
            [
                ("C4".into(), Some(Value::Number(0.0))),
                ("C3".into(), Some(Value::Number(-10.0)))
            ]
        );
    }
}
