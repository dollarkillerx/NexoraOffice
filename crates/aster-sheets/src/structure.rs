//! Inserting and deleting rows and columns: what moves, in the sheet and in
//! every formula that refers to it.
//!
//! References follow the cells they point at, as in Excel: inserting moves
//! every reference at or past the insertion point, absolute or not;
//! deleting makes a reference to a deleted cell `#REF!` and shrinks a range
//! that loses some of its lines.

use crate::address::{CellRange, CellRef, MAX_COLUMNS, MAX_ROWS, column_index, column_name};

/// Rows or columns inserted or deleted on one sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lines {
    pub columns: bool,
    /// The first line, from 0.
    pub at: u32,
    pub count: u32,
    /// Inserted (`true`) or deleted.
    pub insert: bool,
}

impl Lines {
    fn limit(&self) -> u32 {
        if self.columns { MAX_COLUMNS } else { MAX_ROWS }
    }

    /// Where line `index` goes; `None` if it is deleted or pushed off the
    /// sheet.
    pub fn moved(&self, index: u32) -> Option<u32> {
        if self.insert {
            if index < self.at {
                return Some(index);
            }
            let moved = index.checked_add(self.count)?;
            (moved < self.limit()).then_some(moved)
        } else if index < self.at {
            Some(index)
        } else if index < self.at + self.count {
            None
        } else {
            Some(index - self.count)
        }
    }

    /// A span of lines `first..=last` after the change; `None` if all of it
    /// is deleted. Inserting inside the span widens it.
    pub fn moved_span(&self, first: u32, last: u32) -> Option<(u32, u32)> {
        if self.insert {
            let first = self.moved(first).unwrap_or(first);
            let last = self.moved(last).unwrap_or(self.limit() - 1);
            return Some((first, last));
        }
        let end = self.at + self.count;
        if first >= self.at && last < end {
            return None;
        }
        let first = if first >= self.at && first < end {
            self.at
        } else {
            self.moved(first)?
        };
        let last = if last >= self.at && last < end {
            self.at.checked_sub(1)?
        } else {
            self.moved(last)?
        };
        Some((first, last))
    }

    /// A range after the change; `None` if it is deleted.
    pub fn moved_range(&self, range: CellRange) -> Option<CellRange> {
        if self.columns {
            let (first, last) = self.moved_span(range.first.col, range.last.col)?;
            Some(CellRange::new(
                CellRef::new(range.first.row, first),
                CellRef::new(range.last.row, last),
            ))
        } else {
            let (first, last) = self.moved_span(range.first.row, range.last.row)?;
            Some(CellRange::new(
                CellRef::new(first, range.first.col),
                CellRef::new(last, range.last.col),
            ))
        }
    }
}

/// One end of a reference as written: a cell (`$B3`), a whole column
/// (`B`) or a whole row (`3`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct End {
    col: Option<(u32, bool)>,
    row: Option<(u32, bool)>,
}

impl End {
    fn text(&self) -> String {
        let mut out = String::new();
        if let Some((col, absolute)) = self.col {
            if absolute {
                out.push('$');
            }
            out.push_str(&column_name(col));
        }
        if let Some((row, absolute)) = self.row {
            if absolute {
                out.push('$');
            }
            out.push_str(&(row + 1).to_string());
        }
        out
    }

    fn line(&self, columns: bool) -> Option<u32> {
        if columns {
            self.col.map(|(c, _)| c)
        } else {
            self.row.map(|(r, _)| r)
        }
    }

    fn with_line(mut self, columns: bool, line: u32) -> Self {
        if columns {
            if let Some((_, absolute)) = self.col {
                self.col = Some((line, absolute));
            }
        } else if let Some((_, absolute)) = self.row {
            self.row = Some((line, absolute));
        }
        self
    }
}

/// `$B$3`, `B3`, `B` (a column), `3` (a row).
fn parse_end(text: &str) -> Option<End> {
    let (col_abs, rest) = match text.strip_prefix('$') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    let col = if letters > 0 {
        Some((column_index(&rest[..letters])?, col_abs))
    } else {
        None
    };
    let rest = &rest[letters..];
    let (row_abs, digits) = match rest.strip_prefix('$') {
        Some(digits) => (true, digits),
        None if letters == 0 => (col_abs, rest),
        None => (false, rest),
    };
    let row = if digits.is_empty() {
        None
    } else {
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let row: u32 = digits.parse().ok()?;
        if !(1..=MAX_ROWS).contains(&row) {
            return None;
        }
        Some((row - 1, row_abs))
    };
    let col = if letters == 0 { None } else { col };
    if col.is_none() && row.is_none() {
        return None;
    }
    Some(End { col, row })
}

/// A reference or range after the change, as text; `#REF!` if deleted.
fn adjust_reference(first: End, last: Option<End>, lines: &Lines) -> String {
    let columns = lines.columns;
    let applies = |end: &End| end.line(columns).is_some();
    let Some(last) = last else {
        if !applies(&first) {
            return first.text();
        }
        return match first.line(columns).and_then(|l| lines.moved(l)) {
            Some(line) => first.with_line(columns, line).text(),
            None => "#REF!".to_owned(),
        };
    };
    match (first.line(columns), last.line(columns)) {
        (Some(a), Some(b)) => match lines.moved_span(a.min(b), a.max(b)) {
            Some((x, y)) => {
                let (x, y) = if a <= b { (x, y) } else { (y, x) };
                format!(
                    "{}:{}",
                    first.with_line(columns, x).text(),
                    last.with_line(columns, y).text()
                )
            }
            None => "#REF!".to_owned(),
        },
        _ => format!("{}:{}", first.text(), last.text()),
    }
}

pub(crate) fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b >= 0x80
}

/// `formula`, in a cell of sheet `host`, with its references to sheet
/// `target` moved for `lines`. References without a sheet name are to
/// `host`; pass `None` for formulas outside any sheet (defined names).
pub fn adjust(formula: &str, host: Option<&str>, target: &str, lines: &Lines) -> String {
    let bytes = formula.as_bytes();
    let mut out = String::with_capacity(formula.len() + 8);
    let mut i = 0;
    // The sheet named right before the next reference, if any, and where
    // its name starts in `out`.
    let mut named: Option<String> = None;
    let mut named_at = 0usize;
    let same = |a: &str| a.eq_ignore_ascii_case(target);
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'"' {
                    if bytes.get(i + 1) == Some(&b'"') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push_str(&formula[start..i]);
            named = None;
            continue;
        }
        if b == b'\'' {
            // A quoted sheet name, then '!'.
            let start = i;
            named_at = out.len();
            i += 1;
            let mut name = String::new();
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        name.push('\'');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                let len = formula[i..].chars().next().map_or(1, char::len_utf8);
                name.push_str(&formula[i..i + len]);
                i += len;
            }
            out.push_str(&formula[start..i]);
            if bytes.get(i) == Some(&b'!') {
                out.push('!');
                i += 1;
                named = Some(name);
            }
            continue;
        }
        if b == b'[' {
            let end = formula[i..].find(']').map_or(bytes.len(), |at| i + at + 1);
            out.push_str(&formula[i..end]);
            i = end;
            named = None;
            continue;
        }
        if is_name_byte(b) || b == b'$' {
            let start = i;
            let preceded =
                start > 0 && (is_name_byte(bytes[start - 1]) || bytes[start - 1] == b']');
            let mut end = i;
            while end < bytes.len() && (is_name_byte(bytes[end]) || bytes[end] == b'$') {
                end += 1;
            }
            let word = &formula[start..end];
            if bytes.get(end) == Some(&b'!') {
                named_at = out.len();
                out.push_str(word);
                out.push('!');
                i = end + 1;
                named = Some(word.to_owned());
                continue;
            }
            let called = bytes.get(end) == Some(&b'(');
            let first = (!preceded && !called).then(|| parse_end(word)).flatten();
            let Some(first) = first else {
                out.push_str(word);
                i = end;
                named = None;
                continue;
            };
            // A range: ':' and a second end.
            let mut last = None;
            let mut stop = end;
            if bytes.get(end) == Some(&b':') {
                let mut second = end + 1;
                while second < bytes.len() && (is_name_byte(bytes[second]) || bytes[second] == b'$')
                {
                    second += 1;
                }
                if let Some(end2) = parse_end(&formula[end + 1..second])
                    && end2.col.is_some() == first.col.is_some()
                    && end2.row.is_some() == first.row.is_some()
                {
                    last = Some(end2);
                    stop = second;
                }
            }
            // A bare number is not a row unless it is a row range.
            let bare_row = first.col.is_none() && last.is_none();
            let qualified = named.is_some();
            let sheet = named.take().or_else(|| host.map(str::to_owned));
            if !bare_row && sheet.as_deref().is_some_and(same) {
                let adjusted = adjust_reference(first, last, lines);
                // A deleted reference is #REF! alone, without its sheet.
                if qualified && adjusted == "#REF!" {
                    out.truncate(named_at);
                }
                out.push_str(&adjusted);
            } else {
                out.push_str(&formula[start..stop]);
            }
            i = stop;
            continue;
        }
        let len = formula[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&formula[i..i + len]);
        i += len;
        named = None;
    }
    out
}

impl crate::model::Workbook {
    /// Insert or delete rows or columns on sheet `sheet`: its cells, merged
    /// areas, sizes, row and column formats and pictures move, and every
    /// formula in the workbook that refers to it follows. Returns the
    /// cells on other sheets whose formulas changed.
    pub fn restructure(&mut self, sheet: usize, lines: &Lines) -> Vec<(usize, CellRef)> {
        let Some(target) = self.sheets.get(sheet).map(|s| s.name.clone()) else {
            return Vec::new();
        };
        let mut others = Vec::new();
        // Defined names follow too; one of a sheet reads like its formulas.
        for name in &mut self.names {
            let host = name
                .local
                .and_then(|i| self.sheets.get(i))
                .map(|s| s.name.clone());
            name.formula = adjust(&name.formula, host.as_deref(), &target, lines);
        }
        for (index, host) in self.sheets.iter_mut().enumerate() {
            let name = host.name.clone();
            for row in host.rows_mut().iter_mut() {
                for cell in &mut row.cells {
                    let Some(formula) = &cell.formula else {
                        continue;
                    };
                    let adjusted = adjust(formula, Some(&name), &target, lines);
                    if adjusted != **formula {
                        cell.formula = Some(adjusted.into());
                        if index != sheet {
                            others.push((index, CellRef::new(row.index, cell.col)));
                        }
                    }
                }
            }
        }
        let host = &mut self.sheets[sheet];
        let rows = std::mem::take(host.rows_mut());
        let mut moved = Vec::with_capacity(rows.len());
        for mut row in rows {
            if lines.columns {
                row.cells = row
                    .cells
                    .into_iter()
                    .filter_map(|mut cell| {
                        cell.col = lines.moved(cell.col)?;
                        Some(cell)
                    })
                    .collect();
                if !row.cells.is_empty() {
                    moved.push(row);
                }
            } else if let Some(index) = lines.moved(row.index) {
                row.index = index;
                moved.push(row);
            }
        }
        *host.rows_mut() = moved;
        host.merges = host
            .merges
            .iter()
            .filter_map(|m| lines.moved_range(*m))
            .filter(|m| m.first != m.last)
            .collect();
        let axis = if lines.columns {
            &mut host.columns
        } else {
            &mut host.row_sizes
        };
        if lines.insert {
            axis.insert(lines.at, lines.count);
        } else {
            axis.remove(lines.at, lines.count);
        }
        if lines.columns {
            host.column_styles = host
                .column_styles
                .iter()
                .filter_map(|&(first, last, style)| {
                    let (first, last) = lines.moved_span(first, last)?;
                    Some((first, last, style))
                })
                .collect();
        } else {
            host.row_styles = host
                .row_styles
                .iter()
                .filter_map(|&(row, style)| Some((lines.moved(row)?, style)))
                .collect();
        }
        let place = |anchor: &mut crate::model::Anchor| {
            let line = if lines.columns {
                &mut anchor.cell.col
            } else {
                &mut anchor.cell.row
            };
            *line = lines.moved(*line).unwrap_or(lines.at);
        };
        for picture in &mut host.pictures {
            match &mut picture.placement {
                crate::model::Placement::TwoCell { from, to } => {
                    place(from);
                    place(to);
                }
                crate::model::Placement::OneCell { from, .. } => place(from),
                crate::model::Placement::Absolute { .. } => {}
            }
        }
        host.refresh();
        others
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(at: u32, count: u32, insert: bool) -> Lines {
        Lines {
            columns: false,
            at,
            count,
            insert,
        }
    }

    fn cols(at: u32, count: u32, insert: bool) -> Lines {
        Lines {
            columns: true,
            at,
            count,
            insert,
        }
    }

    #[test]
    fn inserting_rows_moves_what_is_below() {
        let two_at_row_3 = rows(2, 2, true);
        let host = Some("表");
        assert_eq!(
            adjust("A1+A3+$A$5", host, "表", &two_at_row_3),
            "A1+A5+$A$7"
        );
        assert_eq!(
            adjust("SUM(A1:A10)", host, "表", &two_at_row_3),
            "SUM(A1:A12)"
        );
        assert_eq!(
            adjust("SUM(A3:A10)", host, "表", &two_at_row_3),
            "SUM(A5:A12)"
        );
        // Another sheet's references move only when they name this one.
        assert_eq!(
            adjust("A3+表!A3", Some("他"), "表", &two_at_row_3),
            "A3+表!A5"
        );
        assert_eq!(
            adjust("'My 表'!B4*2", Some("他"), "My 表", &two_at_row_3),
            "'My 表'!B6*2"
        );
        assert_eq!(adjust("SUM(3:4)", host, "表", &two_at_row_3), "SUM(5:6)");
        assert_eq!(adjust("SUM(C:C)", host, "表", &two_at_row_3), "SUM(C:C)");
        // Text, functions and numbers are left alone.
        assert_eq!(
            adjust("IF(A3>3,\"A3\",LOG10(A3))+3", host, "表", &two_at_row_3),
            "IF(A5>3,\"A3\",LOG10(A5))+3"
        );
        assert_eq!(adjust("A3", None, "表", &two_at_row_3), "A3");
        assert_eq!(adjust("表!A3", None, "表", &two_at_row_3), "表!A5");
    }

    #[test]
    fn deleting_rows_shrinks_ranges_and_breaks_lone_references() {
        let row_3_and_4 = rows(2, 2, false);
        let host = Some("表");
        assert_eq!(adjust("A1+A3+A6", host, "表", &row_3_and_4), "A1+#REF!+A4");
        assert_eq!(
            adjust("SUM(A1:A10)", host, "表", &row_3_and_4),
            "SUM(A1:A8)"
        );
        assert_eq!(
            adjust("SUM(A3:A10)", host, "表", &row_3_and_4),
            "SUM(A3:A8)"
        );
        assert_eq!(adjust("SUM(A1:A4)", host, "表", &row_3_and_4), "SUM(A1:A2)");
        assert_eq!(adjust("SUM(A3:B4)", host, "表", &row_3_and_4), "SUM(#REF!)");
        assert_eq!(adjust("SUM(5:6)", host, "表", &row_3_and_4), "SUM(3:4)");
    }

    #[test]
    fn columns_move_the_same_way() {
        let b_inserted = cols(1, 1, true);
        let host = Some("S");
        assert_eq!(adjust("A1+B1+$C$1", host, "S", &b_inserted), "A1+C1+$D$1");
        assert_eq!(adjust("SUM(A:C)", host, "S", &b_inserted), "SUM(A:D)");
        assert_eq!(adjust("SUM(1:1)", host, "S", &b_inserted), "SUM(1:1)");
        let b_deleted = cols(1, 1, false);
        assert_eq!(adjust("A1+B1+C1", host, "S", &b_deleted), "A1+#REF!+B1");
        assert_eq!(
            adjust("XLOOKUP(A1,A:C,C:C)", host, "S", &b_deleted),
            "XLOOKUP(A1,A:B,B:B)"
        );
    }

    #[test]
    fn lines_and_ranges_move() {
        let insert = rows(5, 3, true);
        assert_eq!(insert.moved(4), Some(4));
        assert_eq!(insert.moved(5), Some(8));
        assert_eq!(insert.moved(MAX_ROWS - 1), None);
        let delete = rows(5, 3, false);
        assert_eq!(delete.moved(6), None);
        assert_eq!(delete.moved(8), Some(5));
        let range = CellRange::parse("B3:C12").unwrap();
        assert_eq!(delete.moved_range(range).unwrap().to_string(), "B3:C9");
        assert_eq!(delete.moved_range(CellRange::parse("A6:A8").unwrap()), None);
        assert_eq!(insert.moved_range(range).unwrap().to_string(), "B3:C15");
    }

    #[test]
    fn a_workbook_moves_cells_merges_sizes_and_formulas() {
        use crate::model::{Cell, SheetBuilder, SheetKind, Value, Workbook};
        use std::sync::Arc;

        let formula = |col: u32, text: &str| {
            let mut cell = Cell::new(col, Value::Empty);
            cell.formula = Some(Arc::from(text));
            cell
        };
        let mut sheet = SheetBuilder::new("表", SheetKind::Worksheet);
        for row in 0..5 {
            sheet.push(row, Cell::new(0, Value::Number(f64::from(row + 1))));
        }
        sheet.push(5, formula(0, "SUM(A1:A5)"));
        sheet.merge(CellRange::parse("B2:C3").unwrap());
        sheet.row_sizes.set(4, 5, 40.0);
        let mut other = SheetBuilder::new("他", SheetKind::Worksheet);
        other.push(0, formula(0, "表!A5*2"));
        let mut book = Workbook {
            sheets: vec![sheet.finish(), other.finish()],
            names: vec![crate::model::DefinedName {
                name: "Total".into(),
                local: None,
                formula: "表!$A$6".into(),
            }],
            ..Workbook::default()
        };
        let changed = book.restructure(
            0,
            &Lines {
                columns: false,
                at: 1,
                count: 2,
                insert: true,
            },
        );
        assert_eq!(changed, [(1, CellRef::new(0, 0))]);
        let s = &book.sheets[0];
        assert_eq!(
            s.cell(CellRef::parse("A1").unwrap()).unwrap().value,
            Value::Number(1.0)
        );
        assert_eq!(
            s.cell(CellRef::parse("A4").unwrap()).unwrap().value,
            Value::Number(2.0)
        );
        assert!(s.cell(CellRef::parse("A2").unwrap()).is_none());
        assert_eq!(
            s.cell(CellRef::parse("A8").unwrap())
                .unwrap()
                .formula
                .as_deref(),
            Some("SUM(A1:A7)")
        );
        assert_eq!(s.merges[0].to_string(), "B4:C5");
        assert_eq!(book.names[0].formula, "表!$A$8");
        assert_eq!(s.row_sizes.size(6), 40.0);
        assert_eq!(
            book.sheets[1]
                .cell(CellRef::new(0, 0))
                .unwrap()
                .formula
                .as_deref(),
            Some("表!A7*2")
        );
        // Deleting what was inserted brings it all back.
        book.restructure(
            0,
            &Lines {
                columns: false,
                at: 1,
                count: 2,
                insert: false,
            },
        );
        let s = &book.sheets[0];
        assert_eq!(
            s.cell(CellRef::parse("A6").unwrap())
                .unwrap()
                .formula
                .as_deref(),
            Some("SUM(A1:A5)")
        );
        assert_eq!(s.merges[0].to_string(), "B2:C3");
        // Deleting column A breaks the other sheet's reference.
        book.restructure(
            0,
            &Lines {
                columns: true,
                at: 0,
                count: 1,
                insert: false,
            },
        );
        assert_eq!(book.sheets[0].cell_count(), 0);
        assert_eq!(book.sheets[0].merges[0].to_string(), "A2:B3");
        assert_eq!(
            book.sheets[1]
                .cell(CellRef::new(0, 0))
                .unwrap()
                .formula
                .as_deref(),
            Some("#REF!*2")
        );
    }
}
