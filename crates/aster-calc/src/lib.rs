//! formualizer 0.15.0 behind [`aster_sheets::CalcEngine`] (ADR 0002).
//!
//! The engine is built from our model's cell inputs, never from the file:
//! values and formulas are copied in, and only the results of formula cells
//! are read back. Two of the differences ADR 0002 lists are handled here:
//!
//! - formualizer has dates as a type of their own; they are turned back into
//!   serial numbers in the workbook's date system, as Excel stores them;
//! - a function it has not implemented (`#N/IMPL!`) keeps the result the
//!   file was saved with, rather than replacing a right answer by an error.
//!
//! A formula that refers to an empty cell gives 0, as in Excel.

use std::sync::Arc;

use aster_sheets::{CalcEngine, CalcError, Cancel, CellRef, Computed, Edit, Value, Workbook};
use formualizer::common::{ExcelError, ExcelErrorKind};
use formualizer::eval::engine::CancelToken;
use formualizer::{DateSystem, LiteralValue, WorkbookConfig};

type Engine = formualizer::Workbook;

pub struct Formualizer {
    engine: Engine,
    sheets: Vec<String>,
    formulas: Vec<(usize, CellRef)>,
    date_system: DateSystem,
}

fn engine_error(error: impl std::fmt::Display) -> CalcError {
    CalcError::Engine(error.to_string())
}

impl Formualizer {
    /// Copy the workbook's inputs into a new engine. Nothing is calculated
    /// until [`CalcEngine::apply`].
    pub fn load(workbook: &Workbook) -> Result<Self, CalcError> {
        let date_system = if workbook.date1904 {
            DateSystem::Excel1904
        } else {
            DateSystem::Excel1900
        };
        let mut config = WorkbookConfig::ephemeral();
        config.eval = config.eval.with_date_system(date_system);
        let mut engine = Engine::new_with_config(config);
        let mut sheets = Vec::with_capacity(workbook.sheets.len());
        let mut formulas = Vec::new();
        for (index, sheet) in workbook.sheets.iter().enumerate() {
            engine.add_sheet(&sheet.name).map_err(engine_error)?;
            sheets.push(sheet.name.clone());
            for row in sheet.rows() {
                // Runs of plain values in one call each.
                let mut run: Vec<LiteralValue> = Vec::new();
                let mut run_start = 0u32;
                let mut next_col = 0u32;
                let flush = |engine: &mut Engine, run: &mut Vec<LiteralValue>, start: u32| {
                    if run.is_empty() {
                        return Ok(());
                    }
                    let values = vec![std::mem::take(run)];
                    engine
                        .set_values(&sheet.name, row.index + 1, start + 1, &values)
                        .map_err(engine_error)
                };
                for cell in &row.cells {
                    if let Some(formula) = &cell.formula {
                        flush(&mut engine, &mut run, run_start)?;
                        engine
                            .set_formula(&sheet.name, row.index + 1, cell.col + 1, formula)
                            .map_err(engine_error)?;
                        formulas.push((index, CellRef::new(row.index, cell.col)));
                        continue;
                    }
                    let Some(value) = literal(&cell.value) else {
                        continue;
                    };
                    if run.is_empty() || cell.col != next_col {
                        flush(&mut engine, &mut run, run_start)?;
                        run_start = cell.col;
                    }
                    run.push(value);
                    next_col = cell.col + 1;
                }
                flush(&mut engine, &mut run, run_start)?;
            }
        }
        Ok(Self {
            engine,
            sheets,
            formulas,
            date_system,
        })
    }

    fn sheet_name(&self, index: usize) -> Result<&str, CalcError> {
        self.sheets
            .get(index)
            .map(String::as_str)
            .ok_or_else(|| CalcError::Engine(format!("no sheet {index}")))
    }

    fn result(&self, value: LiteralValue) -> Option<Value> {
        Some(match value {
            LiteralValue::Number(n) => Value::Number(n),
            LiteralValue::Int(n) => Value::Number(n as f64),
            LiteralValue::Text(text) => Value::Text(Arc::from(text)),
            LiteralValue::Boolean(b) => Value::Bool(b),
            // Not implemented: keep what the file says.
            LiteralValue::Error(error) if error.kind == ExcelErrorKind::NImpl => return None,
            LiteralValue::Error(error) => Value::Error(Arc::from(error.kind.to_string())),
            LiteralValue::Empty => Value::Number(0.0),
            LiteralValue::Pending => return None,
            LiteralValue::Array(rows) => {
                let first = rows.into_iter().next()?.into_iter().next()?;
                return self.result(first);
            }
            other => Value::Number(other.as_serial_number_for(self.date_system)?),
        })
    }
}

fn literal(value: &Value) -> Option<LiteralValue> {
    Some(match value {
        Value::Empty => return None,
        Value::Number(n) => LiteralValue::Number(*n),
        Value::Text(text) => LiteralValue::Text(text.to_string()),
        Value::Bool(b) => LiteralValue::Boolean(*b),
        Value::Error(e) => LiteralValue::Error(ExcelError::from_error_string(e)),
    })
}

impl CalcEngine for Formualizer {
    fn apply(&mut self, edits: &[Edit], cancel: &Cancel) -> Result<Vec<Computed>, CalcError> {
        for edit in edits {
            match edit {
                Edit::Value { sheet, at, value } => {
                    let name = self.sheet_name(*sheet)?.to_owned();
                    let value = literal(value).unwrap_or(LiteralValue::Empty);
                    self.engine
                        .set_value(&name, at.row + 1, at.col + 1, value)
                        .map_err(engine_error)?;
                }
                Edit::Formula { sheet, at, formula } => {
                    let name = self.sheet_name(*sheet)?.to_owned();
                    self.engine
                        .set_formula(&name, at.row + 1, at.col + 1, formula)
                        .map_err(engine_error)?;
                    if !self.formulas.contains(&(*sheet, *at)) {
                        self.formulas.push((*sheet, *at));
                    }
                }
            }
        }
        if cancel.is_cancelled() {
            return Err(CalcError::Cancelled);
        }
        let outcome = self
            .engine
            .evaluate_all_cancellable(CancelToken::from_flag(cancel.flag()));
        if cancel.is_cancelled() {
            return Err(CalcError::Cancelled);
        }
        outcome.map_err(engine_error)?;
        let mut out = Vec::with_capacity(self.formulas.len());
        for &(sheet, at) in &self.formulas {
            let name = &self.sheets[sheet];
            let Some(value) = self.engine.get_value(name, at.row + 1, at.col + 1) else {
                continue;
            };
            if let Some(value) = self.result(value) {
                out.push(Computed { sheet, at, value });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aster_sheets::{Cell, SheetBuilder, SheetKind};

    fn formula(col: u32, text: &str, saved: Value) -> Cell {
        let mut cell = Cell::new(col, saved);
        cell.formula = Some(Arc::from(text));
        cell
    }

    fn text(s: &str) -> Value {
        Value::Text(Arc::from(s))
    }

    fn workbook() -> Workbook {
        let mut prices = SheetBuilder::new("価格表", SheetKind::Worksheet);
        prices.push(0, Cell::new(0, text("苹果")));
        prices.push(0, Cell::new(1, Value::Number(3.5)));
        prices.push(1, Cell::new(0, text("りんご")));
        prices.push(1, Cell::new(1, Value::Number(120.0)));
        let mut orders = SheetBuilder::new("订单", SheetKind::Worksheet);
        orders.push(0, Cell::new(0, Value::Number(2.0)));
        orders.push(0, Cell::new(1, Value::Number(5.0)));
        // A saved result that is out of date: the engine corrects it.
        orders.push(0, formula(2, "A1*B1", Value::Number(999.0)));
        // No saved result at all.
        orders.push(1, formula(2, "SUM(A1:B1)", Value::Empty));
        orders.push(
            2,
            formula(
                0,
                "VLOOKUP(\"りんご\",'価格表'!A1:B2,2,FALSE)",
                Value::Empty,
            ),
        );
        orders.push(3, formula(0, "MID(\"東京都港区\",3,2)", Value::Empty));
        orders.push(4, formula(0, "1/0", Value::Empty));
        orders.push(5, formula(0, "Z99", Value::Empty));
        orders.push(6, formula(0, "DATE(2026,11,22)", Value::Empty));
        orders.push(7, formula(0, "A1>1", Value::Empty));
        Workbook {
            sheets: vec![prices.finish(), orders.finish()],
            ..Workbook::default()
        }
    }

    fn results(workbook: &Workbook) -> Vec<Computed> {
        let mut engine = Formualizer::load(workbook).unwrap();
        engine.apply(&[], &Cancel::new()).unwrap()
    }

    fn at(results: &[Computed], sheet: usize, cell: &str) -> Value {
        let at = CellRef::parse(cell).unwrap();
        results
            .iter()
            .find(|r| r.sheet == sheet && r.at == at)
            .map(|r| r.value.clone())
            .unwrap_or(Value::Empty)
    }

    #[test]
    fn formulas_are_calculated_from_the_model() {
        let workbook = workbook();
        let results = results(&workbook);
        assert_eq!(at(&results, 1, "C1"), Value::Number(10.0));
        assert_eq!(at(&results, 1, "C2"), Value::Number(7.0));
        assert_eq!(at(&results, 1, "A3"), Value::Number(120.0));
        assert_eq!(at(&results, 1, "A4"), text("都港"));
        assert_eq!(at(&results, 1, "A5"), Value::Error(Arc::from("#DIV/0!")));
        // An empty cell is 0.
        assert_eq!(at(&results, 1, "A6"), Value::Number(0.0));
        // A date comes back as its serial number.
        assert_eq!(at(&results, 1, "A7"), Value::Number(46_348.0));
        assert_eq!(at(&results, 1, "A8"), Value::Bool(true));
        let (updated, changed) = workbook.with_results(&results);
        assert!(changed >= 8);
        let shown = updated
            .display(
                updated.sheets[1]
                    .cell(CellRef::parse("C1").unwrap())
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(shown.text, "10");
    }

    #[test]
    fn the_1904_date_system() {
        let mut workbook = workbook();
        workbook.date1904 = true;
        let results = results(&workbook);
        // 1,462 days fewer than in the 1900 system.
        assert_eq!(at(&results, 1, "A7"), Value::Number(46_348.0 - 1_462.0));
    }

    #[test]
    fn edits_are_applied_before_calculating() {
        let workbook = workbook();
        let mut engine = Formualizer::load(&workbook).unwrap();
        engine.apply(&[], &Cancel::new()).unwrap();
        let edit = Edit::Value {
            sheet: 1,
            at: CellRef::parse("A1").unwrap(),
            value: Value::Number(4.0),
        };
        let results = engine.apply(&[edit], &Cancel::new()).unwrap();
        assert_eq!(at(&results, 1, "C1"), Value::Number(20.0));
    }

    #[test]
    fn a_cancelled_calculation_says_so() {
        let mut engine = Formualizer::load(&workbook()).unwrap();
        let cancel = Cancel::new();
        cancel.cancel();
        assert_eq!(engine.apply(&[], &cancel), Err(CalcError::Cancelled));
    }

    #[test]
    fn an_empty_workbook() {
        let workbook = Workbook {
            sheets: vec![aster_sheets::Sheet::empty("Sheet1", SheetKind::Worksheet)],
            ..Workbook::default()
        };
        assert!(results(&workbook).is_empty());
    }

    #[test]
    fn many_formulas() {
        // 20,000 rows, each adding up its two values and the row above.
        let mut sheet = SheetBuilder::new("Big", SheetKind::Worksheet);
        for row in 0..20_000u32 {
            sheet.push(row, Cell::new(0, Value::Number(f64::from(row))));
            sheet.push(row, Cell::new(1, Value::Number(1.0)));
            let text = if row == 0 {
                "A1+B1".to_owned()
            } else {
                format!("A{r}+B{r}+C{p}", r = row + 1, p = row)
            };
            sheet.push(row, formula(2, &text, Value::Empty));
        }
        let workbook = Workbook {
            sheets: vec![sheet.finish()],
            ..Workbook::default()
        };
        let started = std::time::Instant::now();
        let results = results(&workbook);
        assert_eq!(results.len(), 20_000);
        // Sum of 0..n plus n ones.
        let n = 20_000.0;
        assert_eq!(
            at(&results, 0, "C20000"),
            Value::Number((n - 1.0) * n / 2.0 + n)
        );
        assert!(started.elapsed().as_secs() < 30);
    }
}
