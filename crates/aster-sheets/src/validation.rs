//! Data validation: the lists Excel offers in a cell's drop-down arrow.
//!
//! Only `list` rules are modelled. A rule of another kind (a whole number
//! between two limits, a date, a custom formula) is not read into the
//! model: the file keeps it as it is, and it is not enforced here.

use crate::address::{CellRange, CellRef};
use crate::model::{Sheet, Workbook};

/// What the rule does with a value that is not allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ErrorStyle {
    /// The value is refused.
    #[default]
    Stop,
    /// Excel asks whether to keep it; here it is kept, with a notice.
    Warning,
    /// Excel tells, then keeps it; here it is kept, with a notice.
    Information,
}

/// Where a list's values come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListSource {
    /// Typed into the rule: `"Yes,No,Maybe"`.
    Items(Vec<String>),
    /// A range, a defined name or another formula, as the file writes it
    /// (`$A$1:$A$3`, `Lists!$B$2:$B$9`, `Colours`).
    Formula(String),
}

/// A list rule on some cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Validation {
    pub ranges: Vec<CellRange>,
    pub source: ListSource,
    /// An empty entry is allowed.
    pub allow_blank: bool,
    /// The arrow is shown (Excel's "In-cell dropdown").
    pub show_arrow: bool,
    /// A value outside the list is checked at all (Excel's "Show error
    /// alert"); without it anything may be typed.
    pub show_error: bool,
    pub style: ErrorStyle,
    pub error_title: String,
    /// What the rule says to a refused value; empty for the default.
    pub error: String,
}

impl Validation {
    pub fn covers(&self, at: CellRef) -> bool {
        self.ranges.iter().any(|range| range.contains(at))
    }
}

/// A range written in a formula: `$A$1:$A$3`, `Lists!A1:A9`,
/// `'My list'!$B$2`. The sheet's name, if there is one, and the cells.
pub fn parse_range(text: &str) -> Option<(Option<String>, CellRange)> {
    let text = text.trim().trim_start_matches('=');
    let (sheet, cells) = match text.rsplit_once('!') {
        Some((sheet, cells)) => {
            let name = match sheet.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
                Some(quoted) => quoted.replace("''", "'"),
                None => sheet.to_owned(),
            };
            (Some(name), cells)
        }
        None => (None, text),
    };
    let cells: String = cells.chars().filter(|c| *c != '$').collect();
    CellRange::parse(&cells).map(|range| (sheet, range))
}

impl Workbook {
    /// The list rule that applies to a cell of a sheet (of its merged
    /// area's first cell).
    pub fn validation_at(&self, sheet: usize, at: CellRef) -> Option<&Validation> {
        let target = self.sheets.get(sheet)?;
        let at = target.merge_at(at).map_or(at, |m| m.first);
        target.validations.iter().find(|rule| rule.covers(at))
    }

    /// The values a rule offers, as text. `None` when they cannot be told
    /// (a formula that is not a range or a name for one).
    pub fn list_items(&self, sheet: usize, rule: &Validation) -> Option<Vec<String>> {
        let items = match &rule.source {
            ListSource::Items(items) => items.clone(),
            ListSource::Formula(formula) => {
                let (name, range) = self.resolve(sheet, formula, 0)?;
                let source = match name {
                    Some(name) => self
                        .sheets
                        .iter()
                        .find(|s| s.name.eq_ignore_ascii_case(&name) || s.name == name)?,
                    None => self.sheets.get(sheet)?,
                };
                // A list is a single row or column; more is read row by row.
                let mut items = Vec::new();
                for row in range.first.row..=range.last.row.min(range.first.row + 10_000) {
                    for col in range.first.col..=range.last.col.min(range.first.col + 1_000) {
                        let text = source
                            .cell(CellRef::new(row, col))
                            .and_then(|cell| self.display(cell))
                            .map(|d| d.text)
                            .unwrap_or_default();
                        if !text.is_empty() {
                            items.push(text);
                        }
                    }
                }
                items
            }
        };
        Some(items)
    }

    /// A range or a name of one, followed through defined names.
    fn resolve(
        &self,
        sheet: usize,
        formula: &str,
        depth: u8,
    ) -> Option<(Option<String>, CellRange)> {
        if let Some(found) = parse_range(formula) {
            return Some(found);
        }
        if depth > 3 {
            return None;
        }
        let name = formula.trim().trim_start_matches('=');
        let defined = self
            .names
            .iter()
            .find(|n| n.local == Some(sheet) && n.name.eq_ignore_ascii_case(name))
            .or_else(|| {
                self.names
                    .iter()
                    .find(|n| n.local.is_none() && n.name.eq_ignore_ascii_case(name))
            })?;
        self.resolve(sheet, &defined.formula, depth + 1)
    }

    /// Whether a typed entry may go into a cell. `Err` with the rule when
    /// the cell has a list rule that checks entries and the entry is not
    /// on the list. A rule whose values cannot be told allows anything.
    pub fn check_entry(&self, sheet: usize, at: CellRef, text: &str) -> Result<(), &Validation> {
        let Some(rule) = self.validation_at(sheet, at) else {
            return Ok(());
        };
        if !rule.show_error || text.is_empty() && rule.allow_blank || text.is_empty() {
            return Ok(());
        }
        let Some(items) = self.list_items(sheet, rule) else {
            return Ok(());
        };
        let exact = matches!(rule.source, ListSource::Items(_));
        let listed = items.iter().any(|item| {
            if exact {
                item == text
            } else {
                item.to_lowercase() == text.to_lowercase()
            }
        });
        if listed { Ok(()) } else { Err(rule) }
    }
}

impl Sheet {
    /// Whether the cell shows a drop-down arrow when it is the active one.
    pub fn has_dropdown(&self, at: CellRef) -> bool {
        let at = self.merge_at(at).map_or(at, |m| m.first);
        self.validations
            .iter()
            .any(|rule| rule.show_arrow && rule.covers(at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, SheetBuilder, SheetKind, Value};

    fn rule(source: ListSource) -> Validation {
        Validation {
            ranges: vec![CellRange::parse("B2:B5").unwrap()],
            source,
            allow_blank: true,
            show_arrow: true,
            show_error: true,
            style: ErrorStyle::Stop,
            error_title: String::new(),
            error: String::new(),
        }
    }

    fn book(rule: Validation) -> Workbook {
        let mut data = SheetBuilder::new("数据", SheetKind::Worksheet);
        data.validations.push(rule);
        let mut lists = SheetBuilder::new("Lists", SheetKind::Worksheet);
        for (row, text) in ["Red", "Green", "Blue"].iter().enumerate() {
            lists.push(row as u32, Cell::new(2, Value::Text((*text).into())));
        }
        Workbook {
            sheets: vec![data.finish(), lists.finish()],
            names: vec![crate::model::DefinedName {
                name: "Colours".to_owned(),
                local: None,
                formula: "Lists!$C$1:$C$3".to_owned(),
            }],
            ..Workbook::default()
        }
    }

    #[test]
    fn ranges_are_read_with_sheets_dollars_and_quotes() {
        assert_eq!(
            parse_range("$A$1:$A$3"),
            Some((None, CellRange::parse("A1:A3").unwrap()))
        );
        assert_eq!(
            parse_range("'My ''list'!$B$2"),
            Some((Some("My 'list".to_owned()), CellRange::parse("B2").unwrap()))
        );
        assert_eq!(parse_range("Colours"), None);
    }

    #[test]
    fn typed_lists_are_exact_and_range_lists_ignore_case() {
        let at = CellRef::parse("B3").unwrap();
        let typed = book(rule(ListSource::Items(vec!["是".into(), "否".into()])));
        assert!(typed.sheets[0].has_dropdown(at));
        assert!(!typed.sheets[0].has_dropdown(CellRef::parse("C3").unwrap()));
        assert_eq!(typed.check_entry(0, at, "是"), Ok(()));
        assert!(typed.check_entry(0, at, "也许").is_err());
        assert_eq!(typed.check_entry(0, at, ""), Ok(()), "empty entries pass");
        let range = book(rule(ListSource::Formula("Lists!$C$1:$C$3".into())));
        assert_eq!(
            range.list_items(0, &range.sheets[0].validations[0]),
            Some(vec!["Red".into(), "Green".into(), "Blue".into()])
        );
        assert_eq!(range.check_entry(0, at, "green"), Ok(()));
        assert!(range.check_entry(0, at, "Pink").is_err());
        let named = book(rule(ListSource::Formula("Colours".into())));
        assert_eq!(named.check_entry(0, at, "Blue"), Ok(()));
        assert!(named.check_entry(0, at, "Pink").is_err());
        // Outside the rule's cells, or with the alert off, anything goes.
        assert_eq!(
            named.check_entry(0, CellRef::parse("A1").unwrap(), "Pink"),
            Ok(())
        );
        let mut quiet = rule(ListSource::Items(vec!["a".into()]));
        quiet.show_error = false;
        assert_eq!(book(quiet).check_entry(0, at, "z"), Ok(()));
        // A formula that is no range allows anything and offers nothing.
        let other = book(rule(ListSource::Formula("INDIRECT(\"A1\")".into())));
        assert_eq!(other.check_entry(0, at, "z"), Ok(()));
        assert_eq!(other.list_items(0, &other.sheets[0].validations[0]), None);
    }

    #[test]
    fn rules_and_their_lists_follow_inserted_and_deleted_lines() {
        use crate::structure::Lines;
        let mut workbook = book(rule(ListSource::Formula("Lists!$C$1:$C$3".into())));
        // Two rows inserted above the rule's cells, one above the list.
        workbook.restructure(
            0,
            &Lines {
                columns: false,
                at: 0,
                count: 2,
                insert: true,
            },
        );
        workbook.restructure(
            1,
            &Lines {
                columns: false,
                at: 0,
                count: 1,
                insert: true,
            },
        );
        let rule = &workbook.sheets[0].validations[0];
        assert_eq!(rule.ranges, [CellRange::parse("B4:B7").unwrap()]);
        assert_eq!(rule.source, ListSource::Formula("Lists!$C$2:$C$4".into()));
        assert_eq!(
            workbook.list_items(0, rule),
            Some(vec!["Red".into(), "Green".into(), "Blue".into()])
        );
        // Deleting all of the rule's cells takes the rule away.
        workbook.restructure(
            0,
            &Lines {
                columns: false,
                at: 3,
                count: 4,
                insert: false,
            },
        );
        assert!(workbook.sheets[0].validations.is_empty());
    }
}
