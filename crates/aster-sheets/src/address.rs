//! A1-style cell addresses. Rows and columns are 0-based in the model and
//! 1-based (`A1`) only in text.

use std::fmt;

/// Rows of an Excel 2007+ sheet.
pub const MAX_ROWS: u32 = 1_048_576;
/// Columns of an Excel 2007+ sheet: `A` to `XFD`.
pub const MAX_COLUMNS: u32 = 16_384;

/// One cell, 0-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct CellRef {
    pub row: u32,
    pub col: u32,
}

impl CellRef {
    pub const fn new(row: u32, col: u32) -> Self {
        Self { row, col }
    }

    /// `B12`, `xfd1048576`. No `$`, no sheet name. `None` when it is not a
    /// cell address or lies outside the sheet.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        let letters = bytes.iter().take_while(|b| b.is_ascii_alphabetic()).count();
        if letters == 0 || letters > 3 || letters == bytes.len() {
            return None;
        }
        let digits = &text[letters..];
        if !digits.bytes().all(|b| b.is_ascii_digit()) || digits.starts_with('0') {
            return None;
        }
        let col = column_index(&text[..letters])?;
        let row: u32 = digits.parse().ok()?;
        (1..=MAX_ROWS)
            .contains(&row)
            .then(|| Self::new(row - 1, col))
    }
}

impl fmt::Display for CellRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", column_name(self.col), self.row + 1)
    }
}

/// A rectangle of cells, both corners included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CellRange {
    pub first: CellRef,
    pub last: CellRef,
}

impl CellRange {
    pub fn new(a: CellRef, b: CellRef) -> Self {
        Self {
            first: CellRef::new(a.row.min(b.row), a.col.min(b.col)),
            last: CellRef::new(a.row.max(b.row), a.col.max(b.col)),
        }
    }

    pub fn single(cell: CellRef) -> Self {
        Self::new(cell, cell)
    }

    /// `A1:C3` or `B2`.
    pub fn parse(text: &str) -> Option<Self> {
        match text.split_once(':') {
            Some((a, b)) => Some(Self::new(CellRef::parse(a)?, CellRef::parse(b)?)),
            None => CellRef::parse(text).map(Self::single),
        }
    }

    pub fn contains(&self, cell: CellRef) -> bool {
        (self.first.row..=self.last.row).contains(&cell.row)
            && (self.first.col..=self.last.col).contains(&cell.col)
    }

    pub fn rows(&self) -> u32 {
        self.last.row - self.first.row + 1
    }

    pub fn cols(&self) -> u32 {
        self.last.col - self.first.col + 1
    }

    /// Whether the two share a cell.
    pub fn intersects(&self, other: &CellRange) -> bool {
        self.first.row <= other.last.row
            && other.first.row <= self.last.row
            && self.first.col <= other.last.col
            && other.first.col <= self.last.col
    }

    /// The smallest range holding both.
    pub fn union(&self, other: &CellRange) -> CellRange {
        CellRange {
            first: CellRef::new(
                self.first.row.min(other.first.row),
                self.first.col.min(other.first.col),
            ),
            last: CellRef::new(
                self.last.row.max(other.last.row),
                self.last.col.max(other.last.col),
            ),
        }
    }
}

impl fmt::Display for CellRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.first == self.last {
            write!(f, "{}", self.first)
        } else {
            write!(f, "{}:{}", self.first, self.last)
        }
    }
}

/// `A` for 0, `Z` for 25, `AA` for 26.
pub fn column_name(col: u32) -> String {
    let mut n = col + 1;
    let mut name = Vec::with_capacity(3);
    while n > 0 {
        let rem = (n - 1) % 26;
        name.push(b'A' + rem as u8);
        n = (n - 1) / 26;
    }
    name.reverse();
    String::from_utf8(name).expect("ASCII letters")
}

/// The inverse of [`column_name`], ignoring case. `None` past `XFD`.
pub fn column_index(letters: &str) -> Option<u32> {
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut n: u32 = 0;
    for b in letters.bytes() {
        if !b.is_ascii_alphabetic() {
            return None;
        }
        n = n * 26 + u32::from(b.to_ascii_uppercase() - b'A') + 1;
    }
    (n <= MAX_COLUMNS).then(|| n - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_names_round_trip() {
        for (col, name) in [(0, "A"), (25, "Z"), (26, "AA"), (701, "ZZ"), (702, "AAA")] {
            assert_eq!(column_name(col), name);
            assert_eq!(column_index(name), Some(col));
        }
        assert_eq!(column_name(MAX_COLUMNS - 1), "XFD");
        assert_eq!(column_index("XFE"), None);
        assert_eq!(column_index("xfd"), Some(MAX_COLUMNS - 1));
        for col in 0..20_000 {
            assert_eq!(
                column_index(&column_name(col)),
                (col < MAX_COLUMNS).then_some(col)
            );
        }
    }

    #[test]
    fn addresses_parse_and_print() {
        assert_eq!(CellRef::parse("B12"), Some(CellRef::new(11, 1)));
        assert_eq!(CellRef::parse("b12"), Some(CellRef::new(11, 1)));
        assert_eq!(
            CellRef::parse("XFD1048576"),
            Some(CellRef::new(1_048_575, 16_383))
        );
        for bad in [
            "", "A", "12", "A0", "A01", "A1048577", "XFE1", "A1B", "$A$1", "Ａ1",
        ] {
            assert_eq!(CellRef::parse(bad), None, "{bad}");
        }
        assert_eq!(CellRef::new(0, 0).to_string(), "A1");
        let range = CellRange::parse("C3:A1").unwrap();
        assert_eq!(range.to_string(), "A1:C3");
        assert_eq!((range.rows(), range.cols()), (3, 3));
        assert!(range.contains(CellRef::new(1, 1)));
        assert!(!range.contains(CellRef::new(3, 0)));
        assert_eq!(CellRange::parse("D4").unwrap().to_string(), "D4");
    }
}
