//! A sheet's print area: the cells that print, as Excel keeps them in the
//! defined name `_xlnm.Print_Area` of the sheet.
//!
//! The name's text is one or more areas separated by commas, each written
//! with the sheet: `Sheet1!$A$1:$C$10`, `'My sheet'!$A:$B` (whole columns),
//! `Sheet1!$1:$3` (whole rows) or `Sheet1!$B$2` (one cell). This reads and
//! writes that text. A text it does not understand is not a print area
//! here, and the file's is left alone.

use crate::address::{CellRange, CellRef, MAX_COLUMNS, MAX_ROWS, column_index, column_name};
use crate::sheets::qualifier;

/// The name Excel gives a sheet's print area.
pub const NAME: &str = "_xlnm.Print_Area";

/// The areas `formula` stands for on the sheet called `sheet`; `None` if
/// it is anything else (another sheet, a function, a name ...).
pub fn parse(formula: &str, sheet: &str) -> Option<Vec<CellRange>> {
    let mut areas = Vec::new();
    for part in split_areas(formula.trim().trim_start_matches('=')) {
        let (qualifier, cells) = split_sheet(&part)?;
        if !qualifier.eq_ignore_ascii_case(sheet) {
            return None;
        }
        areas.push(parse_area(cells)?);
    }
    (!areas.is_empty()).then_some(areas)
}

/// The text of the name for `areas` on the sheet called `sheet`.
pub fn format(sheet: &str, areas: &[CellRange]) -> String {
    let qualifier = qualifier(sheet);
    areas
        .iter()
        .map(|area| format!("{qualifier}!{}", area_text(*area)))
        .collect::<Vec<_>>()
        .join(",")
}

fn area_text(area: CellRange) -> String {
    let (first, last) = (area.first, area.last);
    let whole_rows = first.col == 0 && last.col == MAX_COLUMNS - 1;
    let whole_columns = first.row == 0 && last.row == MAX_ROWS - 1;
    if whole_rows && !whole_columns {
        format!("${}:${}", first.row + 1, last.row + 1)
    } else if whole_columns && !whole_rows {
        format!("${}:${}", column_name(first.col), column_name(last.col))
    } else if first == last {
        format!("${}${}", column_name(first.col), first.row + 1)
    } else {
        format!(
            "${}${}:${}${}",
            column_name(first.col),
            first.row + 1,
            column_name(last.col),
            last.row + 1
        )
    }
}

/// The parts of `text` between the commas that are not inside quotes.
fn split_areas(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '\'' => {
                quoted = !quoted;
                current.push(c);
            }
            ',' if !quoted => out.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    out.push(current);
    out
}

/// `'My sheet'!$A$1` as the sheet's name and what follows the `!`.
fn split_sheet(part: &str) -> Option<(String, &str)> {
    let part = part.trim();
    if let Some(rest) = part.strip_prefix('\'') {
        let mut name = String::new();
        let mut chars = rest.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            if c == '\'' {
                if chars.peek().is_some_and(|(_, next)| *next == '\'') {
                    name.push('\'');
                    chars.next();
                    continue;
                }
                let after = &rest[at + 1..];
                return Some((name, after.strip_prefix('!')?));
            }
            name.push(c);
        }
        None
    } else {
        let (name, cells) = part.split_once('!')?;
        Some((name.to_owned(), cells))
    }
}

fn parse_area(text: &str) -> Option<CellRange> {
    let plain = text.replace('$', "");
    let (first, last) = match plain.split_once(':') {
        Some((a, b)) => (a, b),
        None => (plain.as_str(), plain.as_str()),
    };
    let end = |text: &str| -> Option<(Option<u32>, Option<u32>)> {
        if text.bytes().all(|b| b.is_ascii_digit()) && !text.is_empty() {
            let row: u32 = text.parse().ok()?;
            return (1..=MAX_ROWS).contains(&row).then(|| (None, Some(row - 1)));
        }
        if text.bytes().all(|b| b.is_ascii_alphabetic()) && !text.is_empty() {
            return Some((Some(column_index(text)?), None));
        }
        let at = CellRef::parse(text)?;
        Some((Some(at.col), Some(at.row)))
    };
    let (first, last) = (end(first)?, end(last)?);
    // Whole rows or columns take the other axis whole; the two ends of a
    // range are the same kind of thing.
    match (first, last) {
        ((Some(c1), Some(r1)), (Some(c2), Some(r2))) => Some(CellRange::new(
            CellRef::new(r1.min(r2), c1.min(c2)),
            CellRef::new(r1.max(r2), c1.max(c2)),
        )),
        ((Some(c1), None), (Some(c2), None)) => Some(CellRange::new(
            CellRef::new(0, c1.min(c2)),
            CellRef::new(MAX_ROWS - 1, c1.max(c2)),
        )),
        ((None, Some(r1)), (None, Some(r2))) => Some(CellRange::new(
            CellRef::new(r1.min(r2), 0),
            CellRef::new(r1.max(r2), MAX_COLUMNS - 1),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(text: &str) -> CellRange {
        CellRange::parse(text).unwrap()
    }

    #[test]
    fn excels_forms_are_read() {
        assert_eq!(
            parse("Sheet1!$A$1:$C$10", "Sheet1"),
            Some(vec![range("A1:C10")])
        );
        assert_eq!(parse("Sheet1!$B$2", "sheet1"), Some(vec![range("B2:B2")]));
        assert_eq!(
            parse(
                "'My ''best'' sheet'!$A$1:$B$2,'My ''best'' sheet'!$D$4:$E$5",
                "My 'best' sheet"
            ),
            Some(vec![range("A1:B2"), range("D4:E5")])
        );
        // Whole columns and rows take the other axis whole.
        let columns = parse("Sheet1!$A:$B", "Sheet1").unwrap()[0];
        assert_eq!(
            (columns.first, columns.last),
            (CellRef::new(0, 0), CellRef::new(MAX_ROWS - 1, 1))
        );
        let rows = parse("Sheet1!$2:$4", "Sheet1").unwrap()[0];
        assert_eq!(
            (rows.first, rows.last),
            (CellRef::new(1, 0), CellRef::new(3, MAX_COLUMNS - 1))
        );
        // Backwards corners are put right; a leading = is allowed.
        assert_eq!(parse("=Sheet1!C3:A1", "Sheet1"), Some(vec![range("A1:C3")]));
    }

    #[test]
    fn what_is_not_a_print_area_is_not_read() {
        for text in [
            "",
            "Other!$A$1:$B$2",
            "Sheet1!$A$1:$B$2,Other!$A$1",
            "OFFSET(Sheet1!$A$1,0,0,3,3)",
            "Sheet1!$A$1:$B",
            "Sheet1!Named",
            "$A$1:$B$2",
            "Sheet1!$A$0",
            "'Sheet1!$A$1",
        ] {
            assert_eq!(parse(text, "Sheet1"), None, "{text}");
        }
    }

    #[test]
    fn areas_are_written_the_way_excel_does() {
        assert_eq!(format("Sheet1", &[range("A1:C10")]), "Sheet1!$A$1:$C$10");
        assert_eq!(format("Sheet1", &[range("B2:B2")]), "Sheet1!$B$2");
        assert_eq!(
            format("My sheet", &[range("A1:B2"), range("D4:E5")]),
            "'My sheet'!$A$1:$B$2,'My sheet'!$D$4:$E$5"
        );
        assert_eq!(format("数据", &[range("A1:A2")]), "数据!$A$1:$A$2");
        assert_eq!(format("A1", &[range("A1:A2")]), "'A1'!$A$1:$A$2");
        let rows = CellRange::new(CellRef::new(1, 0), CellRef::new(3, MAX_COLUMNS - 1));
        assert_eq!(format("S", &[rows]), "S!$2:$4");
        let columns = CellRange::new(CellRef::new(0, 0), CellRef::new(MAX_ROWS - 1, 1));
        assert_eq!(format("S", &[columns]), "S!$A:$B");
        // Everything it writes it reads back.
        for areas in [
            vec![range("A1:C10")],
            vec![range("B2:B2"), range("D4:XFD9")],
            vec![rows, columns],
        ] {
            for sheet in ["S", "My sheet", "it's", "数据"] {
                assert_eq!(parse(&format(sheet, &areas), sheet), Some(areas.clone()));
            }
        }
    }
}
