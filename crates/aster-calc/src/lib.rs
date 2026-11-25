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

use aster_sheets::cond::ExprEngine;
use aster_sheets::{CalcEngine, CalcError, Cancel, CellRef, Computed, Edit, Value, Workbook};
use formualizer::common::{ExcelError, ExcelErrorKind};
use formualizer::eval::engine::CancelToken;
use formualizer::{ASTNode, DateSystem, LiteralValue, WorkbookConfig};

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

/// Evaluates single formulas over the values a workbook holds, for
/// conditional formats (see [`aster_sheets::cond`]): the formula is read
/// and worked out in the context of a cell, without being put in one, so
/// `ROW()` and relative references mean what they do there.
///
/// The engine behind it holds values only (the calculated results of the
/// formula cells). It is filled on first use and, after
/// [`ExprEngine::invalidate`], brought up to date row by row: each row of
/// the workbook is summed up in a hash, and only rows whose hash is not
/// the one remembered are written again. Changing a few cells of a
/// million costs a pass of hashing, not a reload. A workbook with more
/// than [`ExprProbe::MAX_CELLS`] cells is not loaded and no formula is
/// evaluated.
#[derive(Default)]
pub struct ExprProbe {
    loaded: Option<Loaded>,
    /// Cells may have changed since the engine was filled.
    stale: bool,
    /// The workbook is too big to load.
    refused: bool,
    parsed: std::collections::HashMap<String, Option<Arc<ASTNode>>>,
}

/// What the engine holds of one row: a hash of its values and the columns
/// they span.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RowState {
    hash: u64,
    first: u32,
    last: u32,
}

/// Rows of a sheet to write again, and rows that went.
type Work<'a> = (Vec<&'a aster_sheets::Row>, Vec<(u32, RowState)>);

struct Loaded {
    engine: Engine,
    date_system: DateSystem,
    /// Sheet names, and the rows of each as the engine has them.
    sheets: Vec<(String, std::collections::BTreeMap<u32, RowState>)>,
    names: Vec<aster_sheets::model::DefinedName>,
}

fn hash_row(row: &aster_sheets::Row) -> RowState {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let (mut first, mut last) = (u32::MAX, 0u32);
    for cell in &row.cells {
        cell.col.hash(&mut hasher);
        match &cell.value {
            Value::Empty => continue,
            Value::Number(n) => (1u8, n.to_bits()).hash(&mut hasher),
            Value::Text(t) => (2u8, &**t).hash(&mut hasher),
            Value::Bool(b) => (3u8, *b).hash(&mut hasher),
            Value::Error(e) => (4u8, &**e).hash(&mut hasher),
        }
        first = first.min(cell.col);
        last = last.max(cell.col);
    }
    RowState {
        hash: hasher.finish(),
        first,
        last,
    }
}

impl ExprProbe {
    /// The most cells (with a value) a workbook may have for formulas of
    /// conditional formats to be evaluated.
    pub const MAX_CELLS: usize = 4_000_000;
    /// The most formulas whose parse is kept.
    const MAX_PARSED: usize = 10_000;
    /// The longest formula parsed, characters.
    const MAX_FORMULA: usize = 8_192;

    pub fn new() -> Self {
        Self::default()
    }

    fn new_engine(workbook: &Workbook) -> (Engine, DateSystem) {
        let date_system = if workbook.date1904 {
            DateSystem::Excel1904
        } else {
            DateSystem::Excel1900
        };
        let mut config = WorkbookConfig::ephemeral();
        config.eval = config.eval.with_date_system(date_system);
        (Engine::new_with_config(config), date_system)
    }

    fn load(workbook: &Workbook) -> Option<Loaded> {
        if workbook
            .sheets
            .iter()
            .map(|s| s.cell_count())
            .sum::<usize>()
            > Self::MAX_CELLS
        {
            return None;
        }
        let (mut engine, date_system) = Self::new_engine(workbook);
        let mut states = Vec::with_capacity(workbook.sheets.len());
        for sheet in &workbook.sheets {
            engine.add_sheet(&sheet.name).ok()?;
            if let Some(used) = sheet.used_range() {
                engine.ensure_sheet_capacity(
                    &sheet.name,
                    used.last.row as usize + 1,
                    used.last.col as usize + 1,
                );
            }
            let mut rows = std::collections::BTreeMap::new();
            for row in sheet.rows() {
                write_row(&mut engine, &sheet.name, row);
                rows.insert(row.index, hash_row(row));
            }
            states.push((sheet.name.clone(), rows));
        }
        define_names(&mut engine, workbook);
        Some(Loaded {
            engine,
            date_system,
            sheets: states,
            names: workbook.names.clone(),
        })
    }

    /// Bring the engine's values up to the workbook's: the rows that
    /// differ are written again, or, when most do, it starts over.
    fn sync(&mut self, workbook: &Workbook) {
        let Some(loaded) = &mut self.loaded else {
            return;
        };
        let same_shape = loaded.sheets.len() == workbook.sheets.len()
            && loaded
                .sheets
                .iter()
                .zip(&workbook.sheets)
                .all(|((name, _), sheet)| *name == sheet.name)
            && loaded.names == workbook.names;
        if !same_shape {
            self.loaded = Self::load(workbook);
            self.refused = self.loaded.is_none();
            return;
        }
        // Rows to write again, and rows that went, per sheet.
        let mut work: Vec<Work<'_>> = Vec::new();
        let mut changed = 0usize;
        let mut total = 0usize;
        for ((_, known), sheet) in loaded.sheets.iter().zip(&workbook.sheets) {
            let mut rewrite = Vec::new();
            let mut seen = 0usize;
            for row in sheet.rows() {
                let state = hash_row(row);
                seen += 1;
                if known.get(&row.index) != Some(&state) {
                    rewrite.push(row);
                }
            }
            let gone: Vec<(u32, RowState)> = known
                .iter()
                .filter(|(index, _)| sheet.row(**index).is_none())
                .map(|(index, state)| (*index, *state))
                .collect();
            changed += rewrite.len() + gone.len();
            total += seen.max(known.len());
            work.push((rewrite, gone));
        }
        // A row inserted shifts most of them: starting over is quicker.
        if changed > 256 && changed * 4 > total {
            self.loaded = Self::load(workbook);
            self.refused = self.loaded.is_none();
            return;
        }
        for (((name, known), sheet), (rewrite, gone)) in
            loaded.sheets.iter_mut().zip(&workbook.sheets).zip(work)
        {
            for (index, state) in gone {
                clear_row(&mut loaded.engine, name, index, state);
                known.remove(&index);
            }
            for row in rewrite {
                if let Some(old) = known.get(&row.index) {
                    clear_row(&mut loaded.engine, name, row.index, *old);
                }
                write_row(&mut loaded.engine, &sheet.name, row);
                known.insert(row.index, hash_row(row));
            }
        }
    }

    fn parse(&mut self, formula: &str) -> Option<Arc<ASTNode>> {
        if let Some(found) = self.parsed.get(formula) {
            return found.clone();
        }
        if self.parsed.len() >= Self::MAX_PARSED {
            self.parsed.clear();
        }
        let ast = (formula.chars().count() <= Self::MAX_FORMULA)
            .then(|| formualizer::parse::parser::parse(format!("={formula}")).ok())
            .flatten()
            .map(Arc::new);
        self.parsed.insert(formula.to_owned(), ast.clone());
        ast
    }
}

/// A row's values into the engine, runs of neighbours in one call each.
fn write_row(engine: &mut Engine, sheet: &str, row: &aster_sheets::Row) {
    let mut run: Vec<LiteralValue> = Vec::new();
    let mut run_start = 0u32;
    let mut next_col = 0u32;
    let flush = |engine: &mut Engine, run: &mut Vec<LiteralValue>, start: u32| {
        if run.is_empty() {
            return;
        }
        let values = vec![std::mem::take(run)];
        let _ = engine.set_values(sheet, row.index + 1, start + 1, &values);
    };
    for cell in &row.cells {
        let Some(value) = literal(&cell.value) else {
            continue;
        };
        if run.is_empty() || cell.col != next_col {
            flush(engine, &mut run, run_start);
            run_start = cell.col;
        }
        run.push(value);
        next_col = cell.col + 1;
    }
    flush(engine, &mut run, run_start);
}

/// What a row held, emptied.
fn clear_row(engine: &mut Engine, sheet: &str, index: u32, state: RowState) {
    if state.first > state.last {
        return;
    }
    let width = (state.last - state.first + 1) as usize;
    let values = vec![vec![LiteralValue::Empty; width]];
    let _ = engine.set_values(sheet, index + 1, state.first + 1, &values);
}

impl ExprEngine for ExprProbe {
    fn evaluate(
        &mut self,
        workbook: &Workbook,
        sheet: usize,
        formula: &str,
        at: CellRef,
    ) -> Option<Value> {
        use formualizer::eval::interpreter::Interpreter;
        use formualizer::eval::reference::{CellRef as Cell, Coord};

        let ast = self.parse(formula)?;
        if self.loaded.is_none() && !self.refused {
            self.loaded = Self::load(workbook);
            self.refused = self.loaded.is_none();
            self.stale = false;
        } else if self.stale {
            self.sync(workbook);
            self.stale = false;
        }
        let loaded = self.loaded.as_ref()?;
        let name = &workbook.sheets.get(sheet)?.name;
        let id = loaded.engine.engine().sheet_id(name)?;
        let cell = Cell::new(id, Coord::new(at.row, at.col, true, true));
        let interpreter = Interpreter::new_with_cell(loaded.engine.engine(), name, cell);
        let value = interpreter.evaluate_ast(&ast).ok()?.into_literal();
        literal_value(value, loaded.date_system)
    }

    fn invalidate(&mut self) {
        self.stale = true;
        // A workbook that was too big may have shrunk.
        self.refused = false;
    }
}

/// A formula's result as a model value; an empty one is `Empty`, which
/// compares as 0 or empty text where that is wanted.
fn literal_value(value: LiteralValue, date_system: DateSystem) -> Option<Value> {
    Some(match value {
        LiteralValue::Number(n) => Value::Number(n),
        LiteralValue::Int(n) => Value::Number(n as f64),
        LiteralValue::Text(text) => Value::Text(Arc::from(text)),
        LiteralValue::Boolean(b) => Value::Bool(b),
        LiteralValue::Error(error) => Value::Error(Arc::from(error.kind.to_string())),
        LiteralValue::Empty | LiteralValue::Pending => Value::Empty,
        LiteralValue::Array(rows) => {
            let first = rows.into_iter().next()?.into_iter().next()?;
            return literal_value(first, date_system);
        }
        other => Value::Number(other.as_serial_number_for(date_system)?),
    })
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

    #[test]
    fn a_probe_evaluates_formulas_in_the_context_of_a_cell() {
        let mut workbook = workbook();
        // Calculated results stand in the cells, as the view holds them.
        let results = results(&workbook);
        workbook = workbook.with_results(&results).0;
        let mut probe = ExprProbe::new();
        let mut eval = |sheet: usize, formula: &str, cell: &str| {
            probe.evaluate(&workbook, sheet, formula, CellRef::parse(cell).unwrap())
        };
        // The cell's own row and column.
        assert_eq!(eval(1, "ROW()", "C7"), Some(Value::Number(7.0)));
        assert_eq!(eval(1, "MOD(ROW(),2)=0", "C8"), Some(Value::Bool(true)));
        assert_eq!(eval(1, "COLUMN()", "C7"), Some(Value::Number(3.0)));
        // Cells and ranges, here and on another sheet.
        assert_eq!(eval(1, "$A$1+B1", "E1"), Some(Value::Number(7.0)));
        assert_eq!(eval(1, "SUM($A$1:$B$1)", "E1"), Some(Value::Number(7.0)));
        assert_eq!(eval(1, "'価格表'!B2>100", "E1"), Some(Value::Bool(true)));
        assert_eq!(
            eval(1, "AND($A$1>1,$B$1<10)", "E1"),
            Some(Value::Bool(true))
        );
        assert_eq!(
            eval(1, "COUNTIF($A$1:$B$1,\">3\")", "E1"),
            Some(Value::Number(1.0))
        );
        // A text function over a value, and an error as a value.
        assert_eq!(eval(0, "LEN(A1)", "D1"), Some(Value::Number(2.0)));
        assert!(matches!(eval(1, "1/0", "E1"), Some(Value::Error(_))));
        // What does not parse is not a value.
        assert_eq!(eval(1, "SUM((", "E1"), None);
        assert_eq!(eval(7, "1", "E1"), None);
    }

    #[test]
    fn a_probe_reads_again_after_a_change_and_refuses_what_is_too_long() {
        let mut workbook = workbook();
        let mut probe = ExprProbe::new();
        let cell = CellRef::parse("E1").unwrap();
        assert_eq!(
            probe.evaluate(&workbook, 1, "A1", cell),
            Some(Value::Number(2.0))
        );
        workbook.set_cells(
            1,
            vec![(
                CellRef::parse("A1").unwrap(),
                Some(Cell::new(0, Value::Number(8.0))),
            )],
        );
        // Not seen until the probe is told.
        assert_eq!(
            probe.evaluate(&workbook, 1, "A1", cell),
            Some(Value::Number(2.0))
        );
        probe.invalidate();
        assert_eq!(
            probe.evaluate(&workbook, 1, "A1", cell),
            Some(Value::Number(8.0))
        );
        let long = format!("1{}", "+1".repeat(ExprProbe::MAX_FORMULA));
        assert_eq!(probe.evaluate(&workbook, 1, &long, cell), None);
    }

    #[test]
    fn a_probe_loads_a_big_sheet_in_reasonable_time() {
        let mut sheet = SheetBuilder::new("大", SheetKind::Worksheet);
        for row in 0..200_000u32 {
            for col in 0..5u32 {
                sheet.push(row, Cell::new(col, Value::Number(f64::from(row + col))));
            }
        }
        let workbook = Workbook {
            sheets: vec![sheet.finish()],
            ..Workbook::default()
        };
        let mut probe = ExprProbe::new();
        let started = std::time::Instant::now();
        let at = CellRef::parse("G1").unwrap();
        assert_eq!(
            probe.evaluate(&workbook, 0, "SUM(A1:E3)", at),
            Some(Value::Number(45.0))
        );
        let loading = started.elapsed();
        // Later formulas cost nothing like it.
        let started = std::time::Instant::now();
        for row in 0..500u32 {
            let at = CellRef::new(row, 6);
            let formula = format!("A{}>5", row + 1);
            assert!(probe.evaluate(&workbook, 0, &formula, at).is_some());
        }
        let evaluating = started.elapsed();
        eprintln!("one million cells: load {loading:?}, 500 formulas {evaluating:?}");
        assert!(loading.as_secs_f64() < 10.0);
        assert!(evaluating.as_secs_f64() < 2.0);
    }

    #[test]
    fn a_probe_follows_edits_by_writing_only_the_rows_that_changed() {
        let mut sheet = SheetBuilder::new("大", SheetKind::Worksheet);
        for row in 0..200_000u32 {
            sheet.push(row, Cell::new(0, Value::Number(f64::from(row))));
            sheet.push(row, Cell::new(1, Value::Number(1.0)));
        }
        let mut workbook = Workbook {
            sheets: vec![sheet.finish()],
            ..Workbook::default()
        };
        let mut probe = ExprProbe::new();
        let at = CellRef::parse("D1").unwrap();
        let value = |probe: &mut ExprProbe, workbook: &Workbook, formula: &str| {
            probe.evaluate(workbook, 0, formula, at)
        };
        assert_eq!(
            value(&mut probe, &workbook, "A100+B100"),
            Some(Value::Number(100.0))
        );
        // A cell changed, one added in a new row, a row's cells cleared.
        workbook.set_cells(
            0,
            vec![
                (CellRef::new(99, 0), Some(Cell::new(0, Value::Number(5.0)))),
                (
                    CellRef::new(300_000, 2),
                    Some(Cell::new(2, Value::Number(7.0))),
                ),
                (CellRef::new(5, 0), None),
                (CellRef::new(5, 1), None),
            ],
        );
        probe.invalidate();
        let started = std::time::Instant::now();
        assert_eq!(
            value(&mut probe, &workbook, "A100+B100"),
            Some(Value::Number(6.0))
        );
        let synced = started.elapsed();
        assert_eq!(
            value(&mut probe, &workbook, "C300001"),
            Some(Value::Number(7.0))
        );
        assert_eq!(
            value(&mut probe, &workbook, "A6+B6+1"),
            Some(Value::Number(1.0))
        );
        assert_eq!(value(&mut probe, &workbook, "A7"), Some(Value::Number(6.0)));
        eprintln!("sync of 400000 cells after an edit: {synced:?}");
        assert!(synced.as_secs_f64() < 0.5, "{synced:?}");
        // Lines inserted: most rows moved, the probe starts again and is right.
        workbook.restructure(
            0,
            &aster_sheets::structure::Lines {
                columns: false,
                at: 0,
                count: 3,
                insert: true,
            },
        );
        probe.invalidate();
        assert_eq!(
            value(&mut probe, &workbook, "A103"),
            Some(Value::Number(5.0))
        );
        assert_eq!(
            value(&mut probe, &workbook, "A104"),
            Some(Value::Number(100.0))
        );
        assert_eq!(value(&mut probe, &workbook, "A1"), Some(Value::Empty));
    }
}
