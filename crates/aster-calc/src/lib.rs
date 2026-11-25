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
    /// Formulas found in a circular reference at the last calculation.
    circular: Vec<(usize, CellRef)>,
}

fn engine_error(error: impl std::fmt::Display) -> CalcError {
    CalcError::Engine(error.to_string())
}

/// The workbook's defined names, for formulas that use them: a range of
/// cells as a range, a constant as a value, anything else as a formula. A
/// name the engine does not take is left out: formulas that use it show
/// `#NAME?`, as they did.
fn define_names(engine: &mut Engine, workbook: &Workbook) {
    use formualizer::eval::engine::named_range::{NameScope, NamedDefinition};
    use formualizer::eval::reference::{CellRef as At, Coord, RangeRef};

    for name in &workbook.names {
        let scope = match name.local {
            None => NameScope::Workbook,
            Some(index) => {
                let Some(id) = workbook
                    .sheets
                    .get(index)
                    .and_then(|sheet| engine.engine().sheet_id(&sheet.name))
                else {
                    continue;
                };
                NameScope::Sheet(id)
            }
        };
        let text = name.formula.trim();
        let definition = if let Some((sheet, range)) = reference(text)
            && let Some(id) = engine.engine().sheet_id(&sheet)
        {
            let at = |cell: aster_sheets::CellRef| {
                At::new(id, Coord::new(cell.row, cell.col, true, true))
            };
            if range.first == range.last {
                NamedDefinition::Cell(at(range.first))
            } else {
                NamedDefinition::Range(RangeRef::new(at(range.first), at(range.last)))
            }
        } else if let Ok(number) = text.parse::<f64>() {
            NamedDefinition::Literal(LiteralValue::Number(number))
        } else if text.eq_ignore_ascii_case("TRUE") || text.eq_ignore_ascii_case("FALSE") {
            NamedDefinition::Literal(LiteralValue::Boolean(text.eq_ignore_ascii_case("TRUE")))
        } else {
            match formualizer::parse::parser::parse(format!("={text}")) {
                Ok(ast) => NamedDefinition::Formula {
                    ast,
                    dependencies: Vec::new(),
                    range_deps: Vec::new(),
                },
                Err(_) => continue,
            }
        };
        let _ = engine
            .engine_mut()
            .define_name(&name.name, definition, scope);
    }
}

/// `Sheet!$A$1` or `'My sheet'!$A$1:$B$9`: the sheet and the cells. Whole
/// rows and columns, several areas and anything else are not one.
fn reference(text: &str) -> Option<(String, aster_sheets::CellRange)> {
    let (sheet, rest) = if let Some(quoted) = text.strip_prefix('\'') {
        let mut name = String::new();
        let mut chars = quoted.char_indices();
        let mut end = None;
        while let Some((i, c)) = chars.next() {
            if c == '\'' {
                if quoted[i + 1..].starts_with('\'') {
                    name.push('\'');
                    chars.next();
                    continue;
                }
                end = Some(i + 1);
                break;
            }
            name.push(c);
        }
        (name, quoted[end?..].strip_prefix('!')?)
    } else {
        let (sheet, rest) = text.split_once('!')?;
        (sheet.to_owned(), rest)
    };
    let plain = rest.replace('$', "");
    if plain.contains([',', ' ', '(']) {
        return None;
    }
    Some((sheet, aster_sheets::CellRange::parse(&plain)?))
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
        // Formulas are staged as text and the dependency graph is built once,
        // at the first calculation: adding them one by one to a live graph
        // costs seconds on a workbook of 60,000 formulas.
        config.eval.defer_graph_building = true;
        let mut engine = Engine::new_with_config(config);
        let mut sheets = Vec::with_capacity(workbook.sheets.len());
        let mut formulas = Vec::new();
        for (index, sheet) in workbook.sheets.iter().enumerate() {
            engine.add_sheet(&sheet.name).map_err(engine_error)?;
            sheets.push(sheet.name.clone());
            // Room for every row at once: grown a row at a time, the
            // engine's column store copies itself over and over.
            if let Some(used) = sheet.used_range() {
                engine.ensure_sheet_capacity(
                    &sheet.name,
                    used.last.row as usize + 1,
                    used.last.col as usize + 1,
                );
            }
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
        define_names(&mut engine, workbook);
        Ok(Self {
            engine,
            sheets,
            formulas,
            date_system,
            circular: Vec::new(),
        })
    }

    /// Formulas in a circular reference at the last calculation.
    pub fn circular(&self) -> &[(usize, CellRef)] {
        &self.circular
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
        self.circular.clear();
        for &(sheet, at) in &self.formulas {
            let name = &self.sheets[sheet];
            let Some(value) = self.engine.get_value(name, at.row + 1, at.col + 1) else {
                continue;
            };
            // Excel shows 0 in a circular reference and warns about it.
            if let LiteralValue::Error(error) = &value
                && error.kind == ExcelErrorKind::Circ
            {
                self.circular.push((sheet, at));
                out.push(Computed {
                    sheet,
                    at,
                    value: Value::Number(0.0),
                });
                continue;
            }
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

    /// The formula hints list what the engine implements: every function it
    /// registers is described, every described one (older names included)
    /// is one it can calculate, and the arguments listed are at least as many as
    /// the engine asks for.
    #[test]
    fn the_hint_list_is_the_engines_list() {
        formualizer::eval::builtins::load_builtins();
        let registered = formualizer::eval::function_registry::snapshot_registered();
        let catalogue = aster_sheets::hints::catalogue();
        let mut known = std::collections::BTreeSet::new();
        for (namespace, name, function) in &registered {
            if !namespace.is_empty() {
                continue;
            }
            known.insert(name.clone());
            known.extend(function.aliases().iter().map(|a| (*a).to_owned()));
            let Some(info) = catalogue.get(name) else {
                panic!("{name} is registered but has no description");
            };
            // Excel may ask for fewer than the engine does (`IF(x)`), never
            // more.
            assert!(
                info.args.len() >= function.min_args() || info.repeats,
                "{name}: {} arguments listed, the engine asks for {}",
                info.args.len(),
                function.min_args()
            );
            for alias in function.aliases() {
                assert!(catalogue.get(alias).is_some(), "{alias} is not listed");
            }
        }
        for info in catalogue.all() {
            assert!(
                known.contains(&info.name),
                "{} is not in the engine",
                info.name
            );
        }
    }

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
    fn defined_names_are_used() {
        use aster_sheets::model::DefinedName;
        let mut summary = SheetBuilder::new("汇总", SheetKind::Worksheet);
        summary.push(0, Cell::new(1, Value::Number(0.1)));
        summary.push(1, Cell::new(1, Value::Number(200.0)));
        summary.push(2, formula(1, "ROUND(B2*(1+TaxRate),2)", Value::Empty));
        summary.push(3, formula(1, "SUM(Amounts)+Bonus", Value::Empty));
        summary.push(4, formula(1, "Twice", Value::Empty));
        summary.push(5, formula(1, "Here", Value::Empty));
        let mut other = SheetBuilder::new("My sheet", SheetKind::Worksheet);
        other.push(0, Cell::new(0, Value::Number(7.0)));
        other.push(1, formula(0, "Here", Value::Empty));
        let name = |name: &str, local: Option<usize>, formula: &str| DefinedName {
            name: name.into(),
            local,
            formula: formula.into(),
        };
        let workbook = Workbook {
            sheets: vec![summary.finish(), other.finish()],
            names: vec![
                name("TaxRate", None, "汇总!$B$1"),
                name("Amounts", None, "汇总!$B$1:$B$2"),
                name("Bonus", None, "5"),
                name("Twice", None, "汇总!$B$2*2"),
                name("Here", Some(1), "'My sheet'!$A$1"),
            ],
            ..Workbook::default()
        };
        let results = results(&workbook);
        assert_eq!(at(&results, 0, "B3"), Value::Number(220.0));
        assert_eq!(at(&results, 0, "B4"), Value::Number(205.1));
        assert_eq!(at(&results, 0, "B5"), Value::Number(400.0));
        // A name of one sheet is not seen from another.
        assert!(matches!(at(&results, 0, "B6"), Value::Error(_)));
        assert_eq!(at(&results, 1, "A2"), Value::Number(7.0));
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
    fn a_cell_in_the_far_corner() {
        let mut sheet = SheetBuilder::new("Far", SheetKind::Worksheet);
        sheet.push(0, Cell::new(0, Value::Number(2.0)));
        sheet.push(
            aster_sheets::MAX_ROWS - 1,
            Cell::new(aster_sheets::MAX_COLUMNS - 1, Value::Number(3.0)),
        );
        sheet.push(1, formula(0, "A1*XFD1048576", Value::Empty));
        let workbook = Workbook {
            sheets: vec![sheet.finish()],
            ..Workbook::default()
        };
        let started = std::time::Instant::now();
        let results = results(&workbook);
        assert_eq!(at(&results, 0, "A2"), Value::Number(6.0));
        assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
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

    #[test]
    fn edits_recalculate_what_depends_on_them_and_cycles_show_zero() {
        let mut sheet = SheetBuilder::new("連鎖", SheetKind::Worksheet);
        sheet.push(0, Cell::new(0, Value::Number(1.0)));
        for row in 1..2_000u32 {
            sheet.push(row, formula(0, &format!("A{row}+1"), Value::Empty));
        }
        let book = Workbook {
            sheets: vec![sheet.finish()],
            ..Workbook::default()
        };
        let mut engine = Formualizer::load(&book).unwrap();
        let last = |results: &[Computed]| {
            results
                .iter()
                .find(|c| c.at.row == 1_999)
                .map(|c| c.value.clone())
        };
        let results = engine.apply(&[], &Cancel::new()).unwrap();
        assert_eq!(last(&results), Some(Value::Number(2_000.0)));
        let results = engine
            .apply(
                &[Edit::Value {
                    sheet: 0,
                    at: CellRef::new(0, 0),
                    value: Value::Number(10.0),
                }],
                &Cancel::new(),
            )
            .unwrap();
        assert_eq!(last(&results), Some(Value::Number(2_009.0)));
        // Two formulas that refer to each other.
        let cycle = [
            Edit::Formula {
                sheet: 0,
                at: CellRef::new(0, 1),
                formula: "B2+1".into(),
            },
            Edit::Formula {
                sheet: 0,
                at: CellRef::new(1, 1),
                formula: "B1+1".into(),
            },
        ];
        let results = engine.apply(&cycle, &Cancel::new()).unwrap();
        assert_eq!(
            engine.circular(),
            [(0, CellRef::new(0, 1)), (0, CellRef::new(1, 1))]
        );
        let b2 = results.iter().find(|c| c.at == CellRef::new(1, 1)).unwrap();
        assert_eq!(b2.value, Value::Number(0.0));
        // A value in place of one of them breaks the cycle.
        let results = engine
            .apply(
                &[Edit::Value {
                    sheet: 0,
                    at: CellRef::new(0, 1),
                    value: Value::Number(3.0),
                }],
                &Cancel::new(),
            )
            .unwrap();
        assert!(engine.circular().is_empty());
        let b2 = results.iter().find(|c| c.at == CellRef::new(1, 1)).unwrap();
        assert_eq!(b2.value, Value::Number(4.0));
        // An emptied cell counts as 0.
        let results = engine
            .apply(
                &[Edit::Value {
                    sheet: 0,
                    at: CellRef::new(0, 1),
                    value: Value::Empty,
                }],
                &Cancel::new(),
            )
            .unwrap();
        let b2 = results.iter().find(|c| c.at == CellRef::new(1, 1)).unwrap();
        assert_eq!(b2.value, Value::Number(1.0));
    }
}
