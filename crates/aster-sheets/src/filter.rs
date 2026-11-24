//! The autofilter: a block of data with a header row whose columns can
//! show only some of their values, as Excel's filter buttons do.
//!
//! A row shows when, in every filtered column, the text its cell shows
//! (formatted, as on screen) is one of those picked, or the cell is empty
//! and empty cells are picked. Rows the filter hides are kept apart from
//! rows hidden by hand, so taking the filter away shows only its own.

use std::collections::{BTreeMap, BTreeSet};

use crate::address::{CellRange, CellRef};
use crate::model::{Sheet, Workbook};

/// An autofilter on a sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct AutoFilter {
    /// The header row and the data below it.
    pub range: CellRange,
    /// The filtered columns (sheet column indices) and what they show.
    pub columns: BTreeMap<u32, Shown>,
    /// Columns the file filters some other way (conditions, top ten,
    /// colours, date groups): kept as the file has them, not offered here.
    pub others: BTreeSet<u32>,
    /// The rows this filter hides.
    pub hidden: BTreeSet<u32>,
}

/// What a filtered column shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shown {
    /// The texts shown, as cells show them.
    pub values: BTreeSet<String>,
    /// Empty cells are shown.
    pub blanks: bool,
}

impl AutoFilter {
    /// A filter on a block, nothing filtered yet.
    pub fn new(range: CellRange) -> Self {
        Self {
            range,
            columns: BTreeMap::new(),
            others: BTreeSet::new(),
            hidden: BTreeSet::new(),
        }
    }

    /// Whether a column is filtered, its own way or the file's.
    pub fn filters(&self, col: u32) -> bool {
        self.columns.contains_key(&col) || self.others.contains(&col)
    }

    /// The rows under the header.
    pub fn data_rows(&self) -> std::ops::RangeInclusive<u32> {
        self.range.first.row + 1..=self.range.last.row
    }
}

impl Workbook {
    /// What a cell shows, for filtering: its formatted text; empty for none.
    fn shown_text(&self, sheet: &Sheet, at: CellRef) -> String {
        sheet
            .cell(at)
            .and_then(|cell| self.display(cell))
            .map(|display| display.text)
            .unwrap_or_default()
    }

    /// The different texts a column of a filter's data has, in order, and
    /// whether it has empty cells: what its list offers.
    pub fn filter_values(
        &self,
        sheet: usize,
        filter: &AutoFilter,
        col: u32,
    ) -> (Vec<String>, bool) {
        let Some(sheet) = self.sheets.get(sheet) else {
            return (Vec::new(), false);
        };
        let mut values = BTreeSet::new();
        let mut blanks = false;
        for row in filter.data_rows() {
            let text = self.shown_text(sheet, CellRef::new(row, col));
            if text.is_empty() {
                blanks = true;
            } else {
                values.insert(text);
            }
        }
        (values.into_iter().collect(), blanks)
    }

    /// The rows of a filter's data that its columns hide now.
    pub fn filtered_out(&self, sheet: usize, filter: &AutoFilter) -> BTreeSet<u32> {
        let Some(sheet) = self.sheets.get(sheet) else {
            return BTreeSet::new();
        };
        filter
            .data_rows()
            .filter(|&row| {
                filter.columns.iter().any(|(&col, shown)| {
                    let text = self.shown_text(sheet, CellRef::new(row, col));
                    if text.is_empty() {
                        !shown.blanks
                    } else {
                        !shown.values.contains(&text)
                    }
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, SheetBuilder, SheetKind, Value};
    use std::sync::Arc;

    #[test]
    fn rows_show_when_every_filtered_column_picks_them() {
        let mut sheet = SheetBuilder::new("表", SheetKind::Worksheet);
        let rows = [
            ("城市", "区"),
            ("东京", "A"),
            ("大阪", "B"),
            ("东京", ""),
            ("札幌", "A"),
        ];
        for (r, (city, area)) in rows.iter().enumerate() {
            sheet.push(r as u32, Cell::new(0, Value::Text(Arc::from(*city))));
            if !area.is_empty() {
                sheet.push(r as u32, Cell::new(1, Value::Text(Arc::from(*area))));
            }
        }
        let book = Workbook {
            sheets: vec![sheet.finish()],
            ..Workbook::default()
        };
        let mut filter = AutoFilter::new(CellRange::parse("A1:B5").unwrap());
        let (values, blanks) = book.filter_values(0, &filter, 1);
        assert_eq!(values, ["A", "B"]);
        assert!(blanks);
        assert!(book.filtered_out(0, &filter).is_empty());
        filter.columns.insert(
            0,
            Shown {
                values: ["东京".to_owned()].into(),
                blanks: false,
            },
        );
        assert_eq!(book.filtered_out(0, &filter), [2, 4].into());
        filter.columns.insert(
            1,
            Shown {
                values: ["A".to_owned()].into(),
                blanks: false,
            },
        );
        assert_eq!(book.filtered_out(0, &filter), [2, 3, 4].into());
    }
}
