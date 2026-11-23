//! What the model needs to know about formula text before there is an engine.
//!
//! XLSX stores a block of similar formulas once ("shared formulas"): the first
//! cell has the text and the others only point at it. To show the formula of
//! any of those cells, the relative references in the first cell's text are
//! moved by the distance between the two cells. [`shift`] does that.

use crate::address::{MAX_COLUMNS, MAX_ROWS, column_index, column_name};

/// Move the relative cell references in `formula` by `rows` and `cols`.
/// Absolute parts (`$A$1`) stay; text in quotes, sheet names in quotes and
/// function names are left alone. A reference moved off the sheet becomes
/// `#REF!`, as Excel shows it.
pub fn shift(formula: &str, rows: i64, cols: i64) -> String {
    if rows == 0 && cols == 0 {
        return formula.to_owned();
    }
    let bytes = formula.as_bytes();
    let mut out = String::with_capacity(formula.len() + 8);
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'"' | b'\'' => {
                // A string or a quoted sheet name; a doubled quote is an
                // escaped one.
                let start = i;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b {
                        if bytes.get(i + 1) == Some(&b) {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                out.push_str(&formula[start..i]);
            }
            b'[' => {
                // Structured references and external workbook indices.
                let end = formula[i..].find(']').map_or(bytes.len(), |at| i + at + 1);
                out.push_str(&formula[i..end]);
                i = end;
            }
            b'$' | b'A'..=b'Z' | b'a'..=b'z' => {
                let start = i;
                let preceded = start > 0 && is_name_byte(bytes[start - 1]);
                let mut end = i;
                while end < bytes.len() && (is_name_byte(bytes[end]) || bytes[end] == b'$') {
                    end += 1;
                }
                let word = &formula[start..end];
                let called = bytes.get(end) == Some(&b'(');
                let qualified = bytes.get(end) == Some(&b'!');
                match (!preceded && !called && !qualified)
                    .then(|| parse_ref(word))
                    .flatten()
                {
                    Some(reference) => out.push_str(&reference.moved(rows, cols)),
                    None => out.push_str(word),
                }
                i = end;
            }
            _ => {
                // Multi-byte characters are copied whole.
                let len = formula[i..].chars().next().map_or(1, char::len_utf8);
                out.push_str(&formula[i..i + len]);
                i += len;
            }
        }
    }
    out
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b >= 0x80
}

struct Reference {
    col: u32,
    col_absolute: bool,
    row: u32,
    row_absolute: bool,
}

impl Reference {
    fn moved(&self, rows: i64, cols: i64) -> String {
        let col = if self.col_absolute {
            i64::from(self.col)
        } else {
            i64::from(self.col) + cols
        };
        let row = if self.row_absolute {
            i64::from(self.row)
        } else {
            i64::from(self.row) + rows
        };
        if !(0..i64::from(MAX_COLUMNS)).contains(&col) || !(0..i64::from(MAX_ROWS)).contains(&row) {
            return "#REF!".to_owned();
        }
        format!(
            "{}{}{}{}",
            if self.col_absolute { "$" } else { "" },
            column_name(col as u32),
            if self.row_absolute { "$" } else { "" },
            row + 1
        )
    }
}

/// `A1`, `$A1`, `A$1`, `$A$1`; nothing else.
fn parse_ref(word: &str) -> Option<Reference> {
    let (col_absolute, rest) = match word.strip_prefix('$') {
        Some(rest) => (true, rest),
        None => (false, word),
    };
    let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    let col = column_index(&rest[..letters])?;
    let rest = &rest[letters..];
    let (row_absolute, digits) = match rest.strip_prefix('$') {
        Some(digits) => (true, digits),
        None => (false, rest),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let row: u32 = digits.parse().ok()?;
    (1..=MAX_ROWS).contains(&row).then_some(Reference {
        col,
        col_absolute,
        row: row - 1,
        row_absolute,
    })
}

#[cfg(test)]
mod tests {
    use super::shift;

    #[test]
    fn relative_references_move_and_absolute_ones_stay() {
        assert_eq!(shift("A1+B2", 1, 0), "A2+B3");
        assert_eq!(shift("SUM(A1:A10)*$C$1", 2, 1), "SUM(B3:B12)*$C$1");
        assert_eq!(shift("$A1+A$1", 3, 3), "$A4+D$1");
        assert_eq!(shift("A1", 0, 0), "A1");
    }

    #[test]
    fn text_names_and_functions_are_left_alone() {
        assert_eq!(
            shift("IF(A1>0,\"B2 合计\",LOG10(A1))", 1, 0),
            "IF(A2>0,\"B2 合计\",LOG10(A2))"
        );
        assert_eq!(shift("'Q1 B2'!A1+Sheet2!B1", 1, 0), "'Q1 B2'!A2+Sheet2!B2");
        assert_eq!(
            shift("\"say \"\"A1\"\"\"&A1", 1, 0),
            "\"say \"\"A1\"\"\"&A2"
        );
        assert_eq!(
            shift("Table1[[#This Row],[A1]]+C3", 1, 0),
            "Table1[[#This Row],[A1]]+C4"
        );
        assert_eq!(
            shift("_xlfn.XLOOKUP(A1,売上!A:A,B1)", 1, 0),
            "_xlfn.XLOOKUP(A2,売上!A:A,B2)"
        );
        assert_eq!(shift("TRUE+A1", 1, 0), "TRUE+A2");
    }

    #[test]
    fn references_moved_off_the_sheet_become_ref_errors() {
        assert_eq!(shift("A1", -1, 0), "#REF!");
        assert_eq!(shift("XFD1", 0, 1), "#REF!");
    }
}
