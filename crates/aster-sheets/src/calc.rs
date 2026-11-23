//! The calculation engine, behind an interface of our own (ADR 0002).
//!
//! The model is the truth: an engine is built from a [`Workbook`]'s cell
//! inputs and holds only state derived from them, so it can be thrown away
//! and built again at any time. The engine itself lives in an adapter crate
//! (`aster-calc`); this crate does not depend on it.
//!
//! Calculation runs on a worker thread and can be cancelled. Until results
//! arrive, cells show the values the file was saved with.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::address::CellRef;
use crate::model::{Value, Workbook};

/// Asks a running calculation to stop at its next checkpoint. Clones share
/// one signal.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// The flag itself, for engines that take one.
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
}

/// A change to cell inputs. This version is read-only and sends none: an
/// empty list means "calculate everything".
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Value {
        sheet: usize,
        at: CellRef,
        value: Value,
    },
    Formula {
        sheet: usize,
        at: CellRef,
        formula: String,
    },
}

/// A formula cell's result.
#[derive(Clone, Debug, PartialEq)]
pub struct Computed {
    pub sheet: usize,
    pub at: CellRef,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalcError {
    Cancelled,
    /// The engine could not take the workbook or failed while calculating.
    Engine(String),
}

impl fmt::Display for CalcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CalcError::Cancelled => f.write_str("calculation cancelled"),
            CalcError::Engine(detail) => write!(f, "calculation failed: {detail}"),
        }
    }
}

impl std::error::Error for CalcError {}

pub trait CalcEngine: Send {
    /// Apply `edits` and bring every formula up to date; returns the result
    /// of each formula cell whose value is known.
    fn apply(&mut self, edits: &[Edit], cancel: &Cancel) -> Result<Vec<Computed>, CalcError>;
}

impl Workbook {
    /// A copy with calculated results in place of the saved ones. Results for
    /// cells that are not formulas, or that no longer exist, are ignored.
    pub fn with_results(&self, results: &[Computed]) -> (Workbook, usize) {
        let mut out = self.clone();
        let mut changed = 0;
        for result in results {
            let Some(sheet) = out.sheets.get_mut(result.sheet) else {
                continue;
            };
            if let Some(cell) = sheet.cell_mut(result.at)
                && cell.formula.is_some()
                && cell.value != result.value
            {
                cell.value = result.value.clone();
                changed += 1;
            }
        }
        (out, changed)
    }

    /// Every formula cell: sheet index, address and formula text.
    pub fn formulas(&self) -> impl Iterator<Item = (usize, CellRef, &str)> {
        self.sheets.iter().enumerate().flat_map(|(index, sheet)| {
            sheet.rows().iter().flat_map(move |row| {
                row.cells.iter().filter_map(move |cell| {
                    cell.formula
                        .as_deref()
                        .map(|formula| (index, CellRef::new(row.index, cell.col), formula))
                })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, SheetBuilder, SheetKind};

    #[test]
    fn results_replace_saved_values_of_formulas_only() {
        let mut builder = SheetBuilder::new("Sheet1", SheetKind::Worksheet);
        builder.push(0, Cell::new(0, Value::Number(1.0)));
        let mut formula = Cell::new(1, Value::Number(0.0));
        formula.formula = Some(Arc::from("A1*2"));
        builder.push(0, formula);
        let workbook = Workbook {
            sheets: vec![builder.finish()],
            ..Workbook::default()
        };
        assert_eq!(workbook.formulas().count(), 1);
        let results = [
            Computed {
                sheet: 0,
                at: CellRef::new(0, 1),
                value: Value::Number(2.0),
            },
            // Not a formula: left alone.
            Computed {
                sheet: 0,
                at: CellRef::new(0, 0),
                value: Value::Number(9.0),
            },
            Computed {
                sheet: 5,
                at: CellRef::new(0, 0),
                value: Value::Number(9.0),
            },
        ];
        let (updated, changed) = workbook.with_results(&results);
        assert_eq!(changed, 1);
        let sheet = &updated.sheets[0];
        assert_eq!(
            sheet.cell(CellRef::new(0, 1)).unwrap().value,
            Value::Number(2.0)
        );
        assert_eq!(
            sheet.cell(CellRef::new(0, 0)).unwrap().value,
            Value::Number(1.0)
        );
    }

    #[test]
    fn cancel_is_shared_between_clones() {
        let cancel = Cancel::new();
        let other = cancel.clone();
        assert!(!cancel.is_cancelled());
        other.cancel();
        assert!(cancel.is_cancelled());
        assert!(cancel.flag().load(Ordering::Relaxed));
    }
}
